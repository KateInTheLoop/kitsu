//! Provider transport telemetry with a deliberately safe, bounded vocabulary.
//!
//! Events contain no request URLs, headers, bodies, model prompts, or response
//! text. This makes the API suitable for user-facing traces without turning an
//! observability feature into a credential or content exfiltration channel.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptCategory {
    Success,
    RateLimited,
    Unavailable,
    Authentication,
    ClientError,
    ServerError,
    UnexpectedStatus,
    Timeout,
    Connection,
    /// The HTTP response body exceeded a hard client-side byte ceiling.
    ResponseTooLarge,
    /// The complete response body arrived but could not be decoded safely.
    InvalidResponse,
    /// The caller dropped an in-flight send/body or a scheduled retry backoff,
    /// normally after Ctrl+C.
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptOutcome {
    /// A successful HTTP response was accepted by the retry loop. Retained for
    /// adapters that expose a distinct header phase; OpenAI-compatible
    /// transports delay their single terminal event until body completion.
    ResponseAccepted,
    /// The accepted HTTP response body was completely read and decoded.
    BodyCompleted,
    /// Reading or decoding an accepted HTTP response body failed.
    BodyFailed,
    /// This attempt ended and another physical send will follow after a delay.
    RetryScheduled,
    /// A final HTTP response was rejected without another retry.
    Rejected,
    /// A final transport error ended the logical provider request.
    TransportFailed,
    /// A retryable attempt consumed too much budget to start another send.
    DeadlineExceeded,
    /// The physical send/body, or the wait before a scheduled attempt, was
    /// interrupted before another provider disposition.
    Cancelled,
}

/// One completed physical HTTP attempt inside a logical provider request, or a
/// terminal cancellation of the next scheduled attempt during its backoff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAttemptEvent {
    /// One-based physical attempt number.
    pub attempt: u32,
    /// Configured upper bound (`retries + 1`), saturated at `u32::MAX`.
    pub max_attempts: u32,
    pub category: ProviderAttemptCategory,
    /// Numeric HTTP status only; never response text or headers.
    pub status: Option<u16>,
    pub outcome: ProviderAttemptOutcome,
    /// True when no later physical attempt will be made for this request.
    pub terminal: bool,
    /// Selected sleep before the next attempt, when one can be scheduled.
    pub retry_delay_ms: Option<u64>,
    /// True only when a valid Retry-After value governed the selected delay.
    pub retry_after_used: bool,
    /// Approximate budget left immediately after this attempt completed.
    pub remaining_deadline_ms: u64,
}

/// Cloneable callback used to connect an adapter to a runtime event log.
///
/// A fallible observer can return `false` to revoke the request before another
/// physical send starts. This is how durable automation stops retry work after
/// its event sink has failed: recording the disposition that discovered the
/// failure is allowed, but no later network attempt is admitted.
#[derive(Clone)]
pub struct ProviderTelemetrySink(Arc<dyn Fn(ProviderAttemptEvent) -> bool + Send + Sync + 'static>);

impl ProviderTelemetrySink {
    pub fn new<F>(observer: F) -> Self
    where
        F: Fn(ProviderAttemptEvent) + Send + Sync + 'static,
    {
        Self(Arc::new(move |event| {
            observer(event);
            true
        }))
    }

    pub fn with_fallible_observer<F>(observer: F) -> Self
    where
        F: Fn(ProviderAttemptEvent) -> bool + Send + Sync + 'static,
    {
        Self(Arc::new(observer))
    }

    /// Record one disposition. `false` means the observer can no longer make
    /// subsequent physical attempts observable, so retrying must stop.
    pub fn record(&self, event: ProviderAttemptEvent) -> bool {
        (self.0)(event)
    }
}

impl fmt::Debug for ProviderTelemetrySink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderTelemetrySink")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn cloned_sink_reports_to_the_same_observer() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&events);
        let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
        let event = ProviderAttemptEvent {
            attempt: 1,
            max_attempts: 3,
            category: ProviderAttemptCategory::RateLimited,
            status: Some(429),
            outcome: ProviderAttemptOutcome::RetryScheduled,
            terminal: false,
            retry_delay_ms: Some(1000),
            retry_after_used: true,
            remaining_deadline_ms: 29_000,
        };

        assert!(sink.clone().record(event.clone()));
        assert_eq!(&*events.lock().unwrap(), &[event]);
    }

    #[test]
    fn fallible_sink_propagates_observer_rejection() {
        let sink = ProviderTelemetrySink::with_fallible_observer(|_| false);
        assert!(!sink.record(ProviderAttemptEvent {
            attempt: 1,
            max_attempts: 1,
            category: ProviderAttemptCategory::Success,
            status: Some(200),
            outcome: ProviderAttemptOutcome::BodyCompleted,
            terminal: true,
            retry_delay_ms: None,
            retry_after_used: false,
            remaining_deadline_ms: 0,
        }));
    }
}
