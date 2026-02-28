//! Hybrid Logical Clock (HLC) implementation
//!
//! HLCs combine wall clock time with a logical counter to provide
//! causally consistent ordering even with clock drift.

use crate::clock::{Clock, SystemClock};
use std::cmp::Ordering;

/// Default maximum drift allowed before clamping (1 hour in ms)
pub const DEFAULT_MAX_DRIFT_MS: u64 = 60 * 60 * 1000;

/// Hybrid Logical Clock
#[derive(Debug, Clone, Copy, PartialEq, Eq, borsh::BorshSerialize, borsh::BorshDeserialize)]
pub struct HLC {
    /// Wall clock time in milliseconds since Unix epoch
    pub wall_time: u64,
    /// Logical counter for ordering events at same wall_time
    pub counter: u32,
}

impl HLC {
    /// Create a new HLC with the given wall_time and counter
    pub fn new(wall_time: u64, counter: u32) -> Self {
        Self { wall_time, counter }
    }

    /// Create an HLC from the current system time
    pub fn now() -> Self {
        Self::now_with_clock(&SystemClock)
    }

    /// Create an HLC from the given clock (for testing)
    pub fn now_with_clock(clock: &impl Clock) -> Self {
        Self {
            wall_time: clock.now_ms(),
            counter: 0,
        }
    }

    /// Update this clock upon receiving a message with the given HLC.
    /// Uses the system clock.
    pub fn update(&self, received: &HLC) -> HLC {
        self.update_with_clock(received, &SystemClock)
    }

    /// Update this clock with an explicit clock source (for testing)
    pub fn update_with_clock(&self, received: &HLC, clock: &impl Clock) -> HLC {
        let local_wall_time = clock.now_ms();

        if local_wall_time > self.wall_time && local_wall_time > received.wall_time {
            // Local wall clock is ahead of everything, use it
            HLC::new(local_wall_time, 0)
        } else if self.wall_time > received.wall_time {
            // Our last HLC is ahead, increment counter
            HLC::new(self.wall_time, self.counter + 1)
        } else if received.wall_time > self.wall_time {
            // Received HLC is ahead, use it and increment
            HLC::new(received.wall_time, received.counter + 1)
        } else {
            // Same wall_time, take max counter and increment
            HLC::new(self.wall_time, self.counter.max(received.counter) + 1)
        }
    }

    /// Clamp a potentially-future HLC to be at most parent + 1.
    /// Returns the clamped HLC if the original exceeds max_drift,
    /// otherwise returns the original.
    pub fn clamp_future(&self, parent: &HLC, local_wall_time: u64, max_drift_ms: u64) -> HLC {
        if self.wall_time > local_wall_time + max_drift_ms {
            // Clamp to parent + 1 (deterministic across all nodes)
            HLC::new(parent.wall_time, parent.counter + 1)
        } else {
            *self
        }
    }

    /// Clamp with clock source (convenience method)
    pub fn clamp_future_with_clock(
        &self,
        parent: &HLC,
        clock: &impl Clock,
        max_drift_ms: u64,
    ) -> HLC {
        self.clamp_future(parent, clock.now_ms(), max_drift_ms)
    }

    /// Check if this HLC exceeds the given wall time by more than max_drift
    pub fn is_future(&self, local_wall_time: u64, max_drift_ms: u64) -> bool {
        self.wall_time > local_wall_time + max_drift_ms
    }

    /// Increment this HLC for a new local event
    pub fn tick(&self) -> HLC {
        self.tick_with_clock(&SystemClock)
    }

    /// Increment with explicit clock (for testing)
    pub fn tick_with_clock(&self, clock: &impl Clock) -> HLC {
        let now = clock.now_ms();
        if now > self.wall_time {
            HLC::new(now, 0)
        } else {
            HLC::new(self.wall_time, self.counter + 1)
        }
    }
}

impl Ord for HLC {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.wall_time.cmp(&other.wall_time) {
            Ordering::Equal => self.counter.cmp(&other.counter),
            other => other,
        }
    }
}

impl std::fmt::Display for HLC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.wall_time, self.counter)
    }
}

impl PartialOrd for HLC {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Default for HLC {
    fn default() -> Self {
        Self::now()
    }
}

// Tuple conversions
impl From<(u64, u32)> for HLC {
    fn from((wall_time, counter): (u64, u32)) -> Self {
        HLC::new(wall_time, counter)
    }
}

impl From<HLC> for (u64, u32) {
    fn from(hlc: HLC) -> Self {
        (hlc.wall_time, hlc.counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;

    #[test]
    fn test_hlc_ordering() {
        let a = HLC::new(100, 0);
        let b = HLC::new(100, 1);
        let c = HLC::new(101, 0);

        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
    }

    #[test]
    fn test_hlc_update_received_ahead() {
        let local = HLC::new(100, 5);
        let received = HLC::new(200, 3);
        let clock = MockClock::new(50); // Wall clock behind both

        let updated = local.update_with_clock(&received, &clock);

        assert!(updated > received);
        assert_eq!(updated.wall_time, 200);
        assert_eq!(updated.counter, 4);
    }

    #[test]
    fn test_hlc_update_local_ahead() {
        let local = HLC::new(200, 5);
        let received = HLC::new(100, 3);
        let clock = MockClock::new(50); // Wall clock behind both

        let updated = local.update_with_clock(&received, &clock);

        assert!(updated > local);
        assert_eq!(updated.wall_time, 200);
        assert_eq!(updated.counter, 6);
    }

    #[test]
    fn test_hlc_update_wall_clock_ahead() {
        let local = HLC::new(100, 5);
        let received = HLC::new(150, 3);
        let clock = MockClock::new(500); // Wall clock ahead of both

        let updated = local.update_with_clock(&received, &clock);

        assert_eq!(updated.wall_time, 500);
        assert_eq!(updated.counter, 0);
    }

    #[test]
    fn test_hlc_clamp_future() {
        let future = HLC::new(2050_000_000_000, 0);
        let parent = HLC::new(100, 5);
        let clock = MockClock::new(1000);

        let clamped = future.clamp_future_with_clock(&parent, &clock, DEFAULT_MAX_DRIFT_MS);

        assert_eq!(clamped.wall_time, 100);
        assert_eq!(clamped.counter, 6);
    }

    #[test]
    fn test_hlc_clamp_within_drift() {
        let normal = HLC::new(1000, 3);
        let parent = HLC::new(100, 5);
        let clock = MockClock::new(900);

        let clamped = normal.clamp_future_with_clock(&parent, &clock, DEFAULT_MAX_DRIFT_MS);

        assert_eq!(clamped, normal);
    }

    #[test]
    fn test_clock_in_past_uses_received() {
        // Simulates Pi booting with clock at 1970
        let old_clock = HLC::new(0, 0);
        let received = HLC::new(1_700_000_000_000, 5);
        let clock = MockClock::new(0); // Clock also at 1970

        let updated = old_clock.update_with_clock(&received, &clock);

        assert_eq!(updated.wall_time, 1_700_000_000_000);
        assert_eq!(updated.counter, 6);
    }

    #[test]
    fn test_tick_with_mock_clock() {
        let hlc = HLC::new(100, 5);

        // Clock behind: counter increments
        let clock = MockClock::new(50);
        let ticked = hlc.tick_with_clock(&clock);
        assert_eq!(ticked.wall_time, 100);
        assert_eq!(ticked.counter, 6);

        // Clock ahead: use new wall time
        let clock = MockClock::new(200);
        let ticked = hlc.tick_with_clock(&clock);
        assert_eq!(ticked.wall_time, 200);
        assert_eq!(ticked.counter, 0);
    }

    #[test]
    fn test_now_with_mock_clock() {
        let clock = MockClock::new(12345);
        let hlc = HLC::now_with_clock(&clock);

        assert_eq!(hlc.wall_time, 12345);
        assert_eq!(hlc.counter, 0);
    }

    #[test]
    fn test_hlc_update_same_wall_time_collision() {
        // Both local and received have the same wall_time (collision branch)
        let local = HLC::new(100, 5);
        let received = HLC::new(100, 8);
        let clock = MockClock::new(50); // Wall clock behind both

        let updated = local.update_with_clock(&received, &clock);

        // Should take max(5, 8) + 1 = 9
        assert_eq!(updated.wall_time, 100);
        assert_eq!(updated.counter, 9);
    }

    #[test]
    fn test_hlc_update_same_wall_time_local_counter_higher() {
        // Same wall_time, but local has higher counter
        let local = HLC::new(100, 10);
        let received = HLC::new(100, 3);
        let clock = MockClock::new(50);

        let updated = local.update_with_clock(&received, &clock);

        // Should take max(10, 3) + 1 = 11
        assert_eq!(updated.wall_time, 100);
        assert_eq!(updated.counter, 11);
    }

    #[test]
    fn test_is_future() {
        let hlc = HLC::new(1000, 0);

        // Within drift: not future
        assert!(!hlc.is_future(500, 600));

        // Exactly at drift boundary: not future (>= vs >)
        assert!(!hlc.is_future(500, 500));

        // Beyond drift: is future
        assert!(hlc.is_future(500, 400));

        // Way in the future
        let future = HLC::new(2050_000_000_000, 0);
        assert!(future.is_future(1_700_000_000_000, DEFAULT_MAX_DRIFT_MS));
    }

    #[test]
    fn test_system_clock_smoke() {
        // Ensure SystemClock compiles and returns reasonable values
        let hlc = HLC::now();
        // Should be after 2025-01-01 (1735689600000 ms)
        assert!(hlc.wall_time > 1_735_689_600_000);
        assert_eq!(hlc.counter, 0);
    }

    #[test]
    fn test_default_uses_system_clock() {
        let hlc = HLC::default();
        // Should be after 2025-01-01
        assert!(hlc.wall_time > 1_735_689_600_000);
        assert_eq!(hlc.counter, 0);
    }

    #[test]
    fn test_update_with_system_clock_smoke() {
        let local = HLC::new(100, 5);
        let received = HLC::new(200, 3);

        // This should use the real system clock internally
        let updated = local.update(&received);

        // Updated should be greater than both
        assert!(updated > local);
        assert!(updated > received);
    }

    #[test]
    fn test_tick_with_system_clock_smoke() {
        let hlc = HLC::new(100, 5);
        let ticked = hlc.tick();

        // Should be greater than original
        assert!(ticked > hlc);
    }
}
