//! Clock abstraction for testable time
//!
//! Provides a trait for getting the current time, with implementations
//! for real system time and mock time for testing.

use std::time::{SystemTime, UNIX_EPOCH};

/// Trait for getting the current wall clock time in milliseconds
pub trait Clock: Send + Sync {
    /// Get the current time in milliseconds since Unix epoch
    fn now_ms(&self) -> u64;
}

/// Real system clock implementation
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64
    }
}

/// Mock clock for testing - returns a fixed time
#[derive(Debug, Clone, Copy)]
pub struct MockClock {
    pub time_ms: u64,
}

impl MockClock {
    pub fn new(time_ms: u64) -> Self {
        Self { time_ms }
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> u64 {
        self.time_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_clock_returns_reasonable_time() {
        let clock = SystemClock;
        let now = clock.now_ms();
        // Should be after 2025-01-01
        assert!(now > 1_735_689_600_000);
    }

    #[test]
    fn test_mock_clock_returns_fixed_time() {
        let clock = MockClock::new(12345);
        assert_eq!(clock.now_ms(), 12345);
    }
}
