//! `Timestamp` and the `Clock` seam (spec/02 rule 4, spec/01 rule 5):
//! the one time vocabulary every layer shares, and the one source of
//! "now", so tests run deterministically.

/// A moment in time: milliseconds since the Unix epoch, UTC (spec/02
/// rule 4) — the unit every `INTEGER` timestamp column stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn from_millis(millis_since_epoch: i64) -> Self {
        Self(millis_since_epoch)
    }

    pub fn as_millis(&self) -> i64 {
        self.0
    }
}

/// The clock seam (spec/01 rule 5): leaves read the time through it
/// and callers inject the implementation — the system clock from
/// `sb-platform`, a fixed one in tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}
