use std::time::{Duration, Instant};

/// Microseconds elapsed since the server's monotonic epoch.
pub type ServerTimeUs = u64;

/// Wraps the server's monotonic epoch. The only source of session time.
///
/// Deliberately does not wrap `SystemTime`/UTC/NTP - `Instant` never moves
/// backwards, which wall-clock time can when the OS applies a correction.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    epoch: Instant,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    pub fn now_us(&self) -> ServerTimeUs {
        duration_to_us(self.epoch.elapsed())
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

pub fn duration_to_us(d: Duration) -> u64 {
    d.as_micros().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn now_us_advances_monotonically() {
        let clock = Clock::new();
        let first = clock.now_us();
        sleep(Duration::from_millis(5));
        let second = clock.now_us();
        assert!(second > first);
    }

    #[test]
    fn duration_to_us_converts_milliseconds() {
        assert_eq!(duration_to_us(Duration::from_millis(150)), 150_000);
    }

    #[test]
    fn duration_to_us_handles_zero() {
        assert_eq!(duration_to_us(Duration::from_secs(0)), 0);
    }
}
