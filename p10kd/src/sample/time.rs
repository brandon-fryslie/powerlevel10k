//! Facts: `[]` — delivered when the wall clock next crosses a multiple of `interval`.
//! The reply itself is the tick; the shell re-requests to get the next one.

use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::protocol::Facts;

use super::SampleResult;

pub fn sample(interval: Duration) -> SampleResult {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    thread::sleep(until_next_boundary(now, interval.as_secs_f64()));
    Ok(Facts::default())
}

fn until_next_boundary(now: f64, interval: f64) -> Duration {
    Duration::from_secs_f64(interval - now % interval)
}

impl From<std::time::SystemTimeError> for super::SampleError {
    fn from(e: std::time::SystemTimeError) -> Self {
        super::SampleError(format!("system clock is before the epoch: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeps_to_the_next_multiple_of_the_interval() {
        assert_eq!(until_next_boundary(10.25, 1.0), Duration::from_secs_f64(0.75));
        assert!((until_next_boundary(10.0, 1.0).as_secs_f64() - 1.0).abs() < 1e-9);
        assert!((until_next_boundary(7.0, 5.0).as_secs_f64() - 3.0).abs() < 1e-9);
    }
}
