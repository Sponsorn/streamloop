//! The wall clock behind a trait, so tests set the time instead of waiting for it.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock a test sets by hand. Timers run on tokio's time, so a test that waits advances both:
/// `clock.advance(d)` together with `tokio::time::advance(d)`.
pub struct TestClock(Mutex<SystemTime>);

impl TestClock {
    pub fn new(start: SystemTime) -> Self {
        Self(Mutex::new(start))
    }

    pub fn set(&self, t: SystemTime) {
        *self.0.lock().unwrap() = t;
    }

    pub fn advance(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_clock_reports_what_was_set_and_moves_only_when_advanced() {
        // Arrange
        let clock = TestClock::new(UNIX_EPOCH + Duration::from_secs(100));
        // Act
        clock.advance(Duration::from_secs(5));
        let advanced = clock.now();
        clock.set(UNIX_EPOCH);
        // Assert
        assert_eq!(advanced, UNIX_EPOCH + Duration::from_secs(105));
        assert_eq!(clock.now(), UNIX_EPOCH);
    }
}
