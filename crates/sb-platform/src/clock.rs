//! `SystemClock`: the system wall clock behind the `Clock` seam.

use std::time::{SystemTime, UNIX_EPOCH};

use sb_types::time::{Clock, Timestamp};

/// The system clock, read once per call.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(millis_since_epoch(SystemTime::now()))
    }
}

/// Signed milliseconds from the epoch to `time`: negative before it,
/// and clamped to `i64`, whose range `SystemTime` exceeds. `i128` holds
/// any `Duration` in milliseconds, so nothing wraps.
fn millis_since_epoch(time: SystemTime) -> i64 {
    let millis = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_millis() as i128,
        Err(before) => -(before.duration().as_millis() as i128),
    };
    millis.clamp(i64::MIN.into(), i64::MAX.into()) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    /// Beyond `i64` milliseconds in either direction.
    const PAST_I64_MS: Duration = Duration::from_millis((1 << 63) + 1_000);

    /// The clock reads the wall clock: between two samples taken
    /// around it, so a constant clock fails. The wall clock may step
    /// back between samples; such a round is retried.
    #[test]
    fn reads_the_wall_clock() {
        for _ in 0..10 {
            let before = millis_since_epoch(SystemTime::now());
            let now = SystemClock.now().as_millis();
            let after = millis_since_epoch(SystemTime::now());
            if before > after {
                continue;
            }
            assert!(
                (before..=after).contains(&now),
                "{before} <= {now} <= {after}"
            );
            return;
        }
        panic!("the wall clock stepped back in every round");
    }

    #[test]
    fn converts_both_sides_of_the_epoch() {
        let offset = Duration::from_millis(1_500);
        assert_eq!(millis_since_epoch(UNIX_EPOCH), 0);
        assert_eq!(millis_since_epoch(UNIX_EPOCH + offset), 1_500);
        assert_eq!(millis_since_epoch(UNIX_EPOCH - offset), -1_500);
    }

    #[test]
    fn clamps_beyond_i64_milliseconds() {
        let future = UNIX_EPOCH
            .checked_add(PAST_I64_MS)
            .expect("SystemTime holds it");
        let past = UNIX_EPOCH
            .checked_sub(PAST_I64_MS)
            .expect("SystemTime holds it");
        assert_eq!(millis_since_epoch(future), i64::MAX);
        assert_eq!(millis_since_epoch(past), i64::MIN);
    }
}
