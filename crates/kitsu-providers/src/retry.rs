//! Provider-neutral bounded retry and physical-attempt lifecycle mechanisms.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;

use crate::telemetry::{
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptOutcome, ProviderTelemetrySink,
};
use crate::ProviderError;

pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_JITTER: Duration = Duration::from_millis(250);

pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value
            .parse::<u64>()
            .unwrap_or(MAX_RETRY_DELAY.as_secs())
            .min(MAX_RETRY_DELAY.as_secs());
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::DateTime::<chrono::Utc>::from(now);
    let delay = retry_at
        .with_timezone(&chrono::Utc)
        .signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::ZERO);
    Some(delay.min(MAX_RETRY_DELAY))
}

pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    let multiplier = 1u64 << exponent;
    Duration::from_millis(200 * multiplier)
}

pub(crate) fn add_retry_jitter(delay: Duration, sample: u64) -> Duration {
    let jitter_cap = (delay / 10).min(MAX_RETRY_JITTER);
    if jitter_cap.is_zero() {
        return delay.min(MAX_RETRY_DELAY);
    }
    let slots = jitter_cap.as_nanos() as u64 + 1;
    let jitter = Duration::from_nanos(sample % slots);
    delay.saturating_add(jitter).min(MAX_RETRY_DELAY)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryDelayDecision {
    pub(crate) delay: Duration,
    pub(crate) retry_after_used: bool,
}

pub(crate) fn retry_delay_decision(
    attempt: u32,
    retry_after: Option<&reqwest::header::HeaderValue>,
    now: SystemTime,
    jitter_sample: u64,
) -> RetryDelayDecision {
    let advertised = retry_after
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, now));
    let local = backoff_delay(attempt);
    let retry_after_used = advertised.is_some_and(|delay| delay >= local);
    RetryDelayDecision {
        delay: add_retry_jitter(local.max(advertised.unwrap_or_default()), jitter_sample),
        retry_after_used,
    }
}

#[cfg(test)]
pub(crate) fn retry_delay(
    attempt: u32,
    retry_after: Option<&reqwest::header::HeaderValue>,
    now: SystemTime,
    jitter_sample: u64,
) -> Duration {
    retry_delay_decision(attempt, retry_after, now, jitter_sample).delay
}

pub(crate) fn retry_jitter_sample(attempt: u32) -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let clock = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_nanos()))
        .unwrap_or(0);
    clock ^ counter.rotate_left(17) ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

pub(crate) fn remaining_request_time(
    started: tokio::time::Instant,
    timeout: Duration,
) -> Option<Duration> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
}

pub(crate) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn remaining_deadline_millis(started: tokio::time::Instant, timeout: Duration) -> u64 {
    remaining_request_time(started, timeout)
        .map(duration_millis)
        .unwrap_or(0)
}

pub(crate) fn status_category(status: StatusCode) -> ProviderAttemptCategory {
    match status {
        StatusCode::TOO_MANY_REQUESTS => ProviderAttemptCategory::RateLimited,
        StatusCode::SERVICE_UNAVAILABLE => ProviderAttemptCategory::Unavailable,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderAttemptCategory::Authentication,
        status if status.is_success() => ProviderAttemptCategory::Success,
        status if status.is_client_error() => ProviderAttemptCategory::ClientError,
        status if status.is_server_error() => ProviderAttemptCategory::ServerError,
        _ => ProviderAttemptCategory::UnexpectedStatus,
    }
}

pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 429 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 529
    )
}

pub(crate) fn record_attempt(
    telemetry: Option<&ProviderTelemetrySink>,
    event: ProviderAttemptEvent,
) -> bool {
    telemetry.map_or(true, |telemetry| telemetry.record(event))
}

pub(crate) struct InFlightAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl InFlightAttemptGuard {
    pub(crate) fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
            started,
            timeout,
            armed: true,
        }
    }

    pub(crate) fn completed(mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: self.attempt,
                max_attempts: self.max_attempts,
                category: ProviderAttemptCategory::Cancelled,
                status: None,
                outcome: ProviderAttemptOutcome::Cancelled,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

pub(crate) struct ResponseBodyAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    status: u16,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl ResponseBodyAttemptGuard {
    pub(crate) fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        status: u16,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
            status,
            started,
            timeout,
            armed: true,
        }
    }

    pub(crate) fn completed(mut self) {
        self.record(
            ProviderAttemptCategory::Success,
            ProviderAttemptOutcome::BodyCompleted,
        );
        self.armed = false;
    }

    pub(crate) fn failed(mut self, error: &ProviderError) {
        let category = match error {
            ProviderError::Timeout => ProviderAttemptCategory::Timeout,
            ProviderError::Connection(_) => ProviderAttemptCategory::Connection,
            ProviderError::ResponseTooLarge { .. } => ProviderAttemptCategory::ResponseTooLarge,
            _ => ProviderAttemptCategory::InvalidResponse,
        };
        self.record(category, ProviderAttemptOutcome::BodyFailed);
        self.armed = false;
    }

    fn record(&self, category: ProviderAttemptCategory, outcome: ProviderAttemptOutcome) {
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: self.attempt,
                max_attempts: self.max_attempts,
                category,
                status: Some(self.status),
                outcome,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

impl Drop for ResponseBodyAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.record(
            ProviderAttemptCategory::Cancelled,
            ProviderAttemptOutcome::Cancelled,
        );
    }
}

pub(crate) struct RetryBackoffAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl RetryBackoffAttemptGuard {
    pub(crate) fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
            started,
            timeout,
            armed: true,
        }
    }

    fn completed(mut self) {
        self.armed = false;
    }

    fn deadline_exceeded(mut self) {
        self.record(
            ProviderAttemptCategory::Timeout,
            ProviderAttemptOutcome::DeadlineExceeded,
        );
        self.armed = false;
    }

    fn record(&self, category: ProviderAttemptCategory, outcome: ProviderAttemptOutcome) {
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: self.attempt,
                max_attempts: self.max_attempts,
                category,
                status: None,
                outcome,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

impl Drop for RetryBackoffAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.record(
            ProviderAttemptCategory::Cancelled,
            ProviderAttemptOutcome::Cancelled,
        );
    }
}

pub(crate) async fn sleep_before_retry(
    started: tokio::time::Instant,
    timeout: Duration,
    delay: Duration,
) -> Result<(), ProviderError> {
    let remaining = remaining_request_time(started, timeout).ok_or(ProviderError::Timeout)?;
    if delay >= remaining {
        tokio::time::sleep(remaining).await;
        return Err(ProviderError::Timeout);
    }
    tokio::time::sleep(delay).await;
    Ok(())
}

pub(crate) async fn sleep_before_retry_observed(
    started: tokio::time::Instant,
    timeout: Duration,
    delay: Duration,
    guard: RetryBackoffAttemptGuard,
) -> Result<(), ProviderError> {
    match sleep_before_retry(started, timeout, delay).await {
        Ok(()) => {
            if remaining_request_time(started, timeout).is_some() {
                guard.completed();
                Ok(())
            } else {
                guard.deadline_exceeded();
                Err(ProviderError::Timeout)
            }
        }
        Err(error) => {
            guard.deadline_exceeded();
            Err(error)
        }
    }
}
