//! Uniform progress reporting for long-running jobs.

use std::sync::mpsc as std_mpsc;

use serde::Serialize;
use tokio::sync::mpsc;

/// One progress update.  All fields except `stage` and `percent` are optional
/// so fast jobs can report a simple percentage.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Machine-readable stage name (`convert`, `hash`, `copy`, ...).
    pub stage: String,
    /// Completion in percent, clamped to 0..=100.
    pub percent: f64,
    /// Human-readable detail.
    pub message: Option<String>,
    /// Work completed in bytes.
    pub bytes_done: Option<u64>,
    /// Total work in bytes, when known.
    pub bytes_total: Option<u64>,
    /// Formatted speed string.
    pub speed: Option<String>,
    /// Estimated seconds remaining.
    pub eta_seconds: Option<f64>,
}

impl ProgressEvent {
    /// Create an event with a stage and percentage.
    pub fn new(stage: impl Into<String>, percent: f64) -> Self {
        Self {
            stage: stage.into(),
            percent: if percent.is_finite() {
                percent.clamp(0.0, 100.0)
            } else {
                0.0
            },
            message: None,
            bytes_done: None,
            bytes_total: None,
            speed: None,
            eta_seconds: None,
        }
    }

    /// Attach a human message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Attach byte counters.
    pub fn with_bytes(mut self, done: u64, total: Option<u64>) -> Self {
        self.bytes_done = Some(done);
        self.bytes_total = total;
        self
    }

    /// Attach a formatted speed.
    pub fn with_speed(mut self, speed: impl Into<String>) -> Self {
        self.speed = Some(speed.into());
        self
    }

    /// Attach an ETA in seconds.
    pub fn with_eta(mut self, eta_seconds: Option<f64>) -> Self {
        self.eta_seconds = eta_seconds.filter(|value| value.is_finite() && *value >= 0.0);
        self
    }

    /// True when the job reported completion.
    pub fn is_complete(&self) -> bool {
        self.percent >= 100.0
    }
}

/// Anything that can receive progress updates.  Implementations must be
/// cheap and non-blocking because they run on worker threads.
pub trait ProgressSink: Send + Sync {
    /// Consume one event.
    fn report(&self, event: ProgressEvent);
}

/// Discards every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn report(&self, _event: ProgressEvent) {}
}

/// Forwards events to a Tokio unbounded channel; dropping the receiver is
/// harmless.
#[derive(Debug, Clone)]
pub struct ChannelProgress {
    sender: mpsc::UnboundedSender<ProgressEvent>,
}

impl ChannelProgress {
    /// Wrap a Tokio sender.
    pub fn new(sender: mpsc::UnboundedSender<ProgressEvent>) -> Self {
        Self { sender }
    }

    /// Pair of sink and receiver.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<ProgressEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self::new(sender), receiver)
    }
}

impl ProgressSink for ChannelProgress {
    fn report(&self, event: ProgressEvent) {
        let _ = self.sender.send(event);
    }
}

/// Forwards events to a blocking `std` channel, useful for tests.
#[derive(Debug, Clone)]
pub struct StdChannelProgress {
    sender: std_mpsc::Sender<ProgressEvent>,
}

impl StdChannelProgress {
    /// Wrap a standard sender.
    pub fn new(sender: std_mpsc::Sender<ProgressEvent>) -> Self {
        Self { sender }
    }
}

impl ProgressSink for StdChannelProgress {
    fn report(&self, event: ProgressEvent) {
        let _ = self.sender.send(event);
    }
}

/// Records every event in memory (tests and CLI previews).
#[derive(Debug, Default)]
pub struct CollectProgress {
    events: std::sync::Mutex<Vec<ProgressEvent>>,
}

impl CollectProgress {
    /// Create an empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the recorded events.
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}

impl ProgressSink for CollectProgress {
    fn report(&self, event: ProgressEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// Adapter for closures; keeps CLI code concise.
pub struct FnProgress<F>(pub F)
where
    F: Fn(ProgressEvent) + Send + Sync;

impl<F> ProgressSink for FnProgress<F>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    fn report(&self, event: ProgressEvent) {
        (self.0)(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_clamped() {
        assert_eq!(ProgressEvent::new("x", 120.0).percent, 100.0);
        assert_eq!(ProgressEvent::new("x", -1.0).percent, 0.0);
        assert_eq!(ProgressEvent::new("x", f64::NAN).percent, 0.0);
    }

    #[test]
    fn builders_set_fields() {
        let event = ProgressEvent::new("hash", 50.0)
            .with_message("half")
            .with_bytes(5, Some(10))
            .with_speed("1 MB/s")
            .with_eta(Some(2.0));
        assert_eq!(event.message.as_deref(), Some("half"));
        assert_eq!(event.bytes_done, Some(5));
        assert_eq!(event.speed.as_deref(), Some("1 MB/s"));
        assert!(!event.is_complete());
    }

    #[test]
    fn collector_records() {
        let collector = CollectProgress::new();
        collector.report(ProgressEvent::new("a", 1.0));
        collector.report(ProgressEvent::new("a", 100.0));
        assert_eq!(collector.events().len(), 2);
        assert!(collector.events()[1].is_complete());
    }
}
