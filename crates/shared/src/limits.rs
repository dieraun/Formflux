//! A soft, process-wide memory budget.
//!
//! Jobs reserve an estimate of the memory they are about to use.  When the
//! budget is exhausted the caller blocks until another job releases its
//! permit, instead of allowing the process to be killed by the OOM killer.
//! The guard is RAII so an error or panic in a worker still releases memory.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::error::{FormfluxError, Result};

/// A blocking memory budget that can be shared across worker threads.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: u64,
    used: Mutex<u64>,
    released: Condvar,
}

impl MemoryBudget {
    /// Create a budget with a hard ceiling in bytes.
    pub fn new(limit_bytes: u64) -> Self {
        Self {
            limit: limit_bytes.max(1),
            used: Mutex::new(0),
            released: Condvar::new(),
        }
    }

    /// Total budget in bytes.
    pub fn limit_bytes(&self) -> u64 {
        self.limit
    }

    /// Currently reserved bytes.
    pub fn used_bytes(&self) -> u64 {
        *self.lock()
    }

    /// Bytes available for a new reservation.
    pub fn available_bytes(&self) -> u64 {
        self.limit.saturating_sub(self.used_bytes())
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserve `bytes`, waiting while the budget is full.
    ///
    /// A request larger than the whole budget is rejected immediately rather
    /// than deadlocking forever.
    pub fn reserve(&self, bytes: u64) -> Result<MemoryPermit<'_>> {
        self.reserve_timeout(bytes, None)
    }

    /// Reserve with an optional timeout.  `None` waits forever.
    pub fn reserve_timeout(
        &self,
        bytes: u64,
        timeout: Option<Duration>,
    ) -> Result<MemoryPermit<'_>> {
        if bytes > self.limit {
            return Err(FormfluxError::Limit(format!(
                "a single job needs {} but the memory budget is only {}",
                crate::units::format_bytes(bytes),
                crate::units::format_bytes(self.limit)
            )));
        }
        let mut used = self.lock();
        if timeout.is_some() {
            let deadline = std::time::Instant::now() + timeout.unwrap_or_default();
            while used.saturating_add(bytes) > self.limit {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(FormfluxError::Limit(
                        "timed out waiting for the memory budget".to_string(),
                    ));
                }
                let (guard, wait_result) = self
                    .released
                    .wait_timeout(used, remaining)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                used = guard;
                if wait_result.timed_out() && used.saturating_add(bytes) > self.limit {
                    return Err(FormfluxError::Limit(
                        "timed out waiting for the memory budget".to_string(),
                    ));
                }
            }
        } else {
            while used.saturating_add(bytes) > self.limit {
                used = self
                    .released
                    .wait(used)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
        *used += bytes;
        Ok(MemoryPermit {
            budget: self,
            bytes,
        })
    }

    fn release(&self, bytes: u64) {
        let mut used = self.lock();
        *used = used.saturating_sub(bytes);
        drop(used);
        self.released.notify_all();
    }
}

/// RAII reservation returned by [`MemoryBudget::reserve`].
#[derive(Debug)]
pub struct MemoryPermit<'a> {
    budget: &'a MemoryBudget,
    bytes: u64,
}

impl MemoryPermit<'_> {
    /// Number of bytes reserved by this permit.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for MemoryPermit<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn reserves_and_releases() {
        let budget = MemoryBudget::new(100);
        {
            let permit = budget.reserve(60).unwrap();
            assert_eq!(permit.bytes(), 60);
            assert_eq!(budget.used_bytes(), 60);
            assert_eq!(budget.available_bytes(), 40);
        }
        assert_eq!(budget.used_bytes(), 0);
        assert_eq!(budget.available_bytes(), 100);
    }

    #[test]
    fn rejects_oversized_jobs() {
        let budget = MemoryBudget::new(100);
        assert!(budget.reserve(101).is_err());
    }

    #[test]
    fn timeout_does_not_deadlock() {
        let budget = MemoryBudget::new(100);
        let _held = budget.reserve(90).unwrap();
        let err = budget
            .reserve_timeout(20, Some(Duration::from_millis(20)))
            .unwrap_err();
        assert!(matches!(err, FormfluxError::Limit(_)));
    }

    #[test]
    fn releases_from_another_thread() {
        let budget = Arc::new(MemoryBudget::new(100));
        let held = budget.reserve(100).unwrap();
        let handle = std::thread::spawn({
            let budget = Arc::clone(&budget);
            move || {
                let permit = budget.reserve(50).unwrap();
                assert_eq!(permit.bytes(), 50);
                drop(permit);
            }
        });
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
        handle.join().unwrap();
        assert_eq!(budget.used_bytes(), 0);
    }
}
