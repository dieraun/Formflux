//! Cooperative cancellation shared by CLI, GUI and worker threads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{FormfluxError, Result};

/// A cheap, cloneable cancellation flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create a fresh, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.  Idempotent and thread-safe.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Return [`FormfluxError::Cancelled`] when cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(FormfluxError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// A token that can never be cancelled, useful for tests and one-shot calls.
    pub fn never() -> Self {
        Self::new()
    }
}

/// A token shared by a group of jobs; cancelling one cancels all.
#[derive(Debug, Clone, Default)]
pub struct CancelGroup {
    token: CancelToken,
}

impl CancelGroup {
    /// Create an empty group.
    pub fn new() -> Self {
        Self::default()
    }

    /// Token to hand to each worker.
    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }

    /// Cancel every worker using this group.
    pub fn cancel_all(&self) {
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_lifecycle() {
        let token = CancelToken::new();
        assert!(token.check().is_ok());
        token.cancel();
        assert!(token.is_cancelled());
        assert!(token.check().unwrap_err().is_cancelled());
    }

    #[test]
    fn clones_share_state() {
        let token = CancelToken::new();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }
}
