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
    /// The caller dropped an in-flight physical send, normally after Ctrl+C.
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptOutcome {
    /// A successful HTTP response was accepted by the retry loop. Streaming
    /// body decoding can still fail later and is reported through ChatStream.
    ResponseAccepted,
    /// This attempt ended and another physical send will follow after a delay.
    RetryScheduled,
    /// A final HTTP response was rejected without another retry.
    Rejected,
    /// A final transport error ended the logical provider request.
    TransportFailed,
    /// A retryable attempt consumed too much budget to start another send.
    DeadlineExceeded,
    /// The physical send was interrupted before a response or transport error.
    Cancelled,
}

/// One completed physical HTTP attempt inside a logical provider request.
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
#[derive(Clone)]
pub struct ProviderTelemetrySink(Arc<dyn Fn(ProviderAttemptEvent) + Send + Sync + 'static>);

impl ProviderTelemetrySink {
    pub fn new<F>(observer: F) -> Self
    where
        F: Fn(ProviderAttemptEvent) + Send + Sync + 'static,
    {
        Self(Arc::new(observer))
    }

    pub fn record(&self, event: ProviderAttemptEvent) {
        (self.0)(event);
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

        sink.clone().record(event.clone());
        assert_eq!(&*events.lock().unwrap(), &[event]);
    }
}
