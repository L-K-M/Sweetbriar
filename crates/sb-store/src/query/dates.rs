//! Validated explicit-zone date operands (spec/06 "Zoned date operands").
//!
//! A [`DatePredicate`] couples a raw [`DateValue`] and an IANA zone name
//! to its validated UTC-millisecond [`DateBounds`]. Every civil boundary
//! comes from the bundled timezone database via [`TimeZoneDatabase::bundled`];
//! no host zoneinfo files, host timezone, environment settings or a clock
//! seam participate. Jiff types stay private to this module.

use std::error::Error;
use std::fmt;

use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone, TimeZoneDatabase};
use sb_types::time::Timestamp;

/// One relative day is always exactly this many UTC milliseconds, even
/// across daylight-saving transitions (spec/06).
const RELATIVE_DAY_MS: i64 = 86_400_000;

/// A date operand as the query language spells it, owned and not yet
/// validated (spec/06). The absolute variants carry the raw `YYYY-MM-DD`
/// text so the parser can reuse it for error spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateValue {
    /// Days strictly before the named date.
    Before(String),
    /// Days strictly after the named date.
    After(String),
    /// The named day itself.
    On(String),
    /// The `days` fixed-length UTC days ending at `clock`, inclusive.
    WithinDays { days: u32, clock: Timestamp },
}

/// The validated UTC-millisecond bounds of a [`DatePredicate`]
/// (spec/06). `Copy` so comparisons never borrow the predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateBounds {
    /// Exclusive upper bound: the first instant of the named day.
    Before(Timestamp),
    /// Inclusive lower bound: the first instant after the named day.
    After(Timestamp),
    /// Half-open `[start, end)` covering the named day.
    On { start: Timestamp, end: Timestamp },
    /// Closed `[start, end]` fixed-length window ending at the clock.
    WithinDays { start: Timestamp, end: Timestamp },
}

/// The rejected operand plus a static reason (spec/06). The operand
/// variants let the later parser attach the error to its input span;
/// they are not parsed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateError {
    /// The `YYYY-MM-DD` text, or a civil bound it requires, is invalid.
    Date(&'static str),
    /// The zone is not a bundled IANA name spelled exactly.
    TimeZone(&'static str),
    /// The within-days count is invalid.
    Days(&'static str),
    /// The within-days clock window cannot be represented.
    Clock(&'static str),
}

impl fmt::Display for DateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (operand, reason) = match self {
            Self::Date(reason) => ("date", reason),
            Self::TimeZone(reason) => ("time zone", reason),
            Self::Days(reason) => ("days", reason),
            Self::Clock(reason) => ("clock", reason),
        };
        write!(f, "invalid {operand} operand: {reason}")
    }
}

impl Error for DateError {}

/// A [`DateValue`] validated against an explicit IANA zone name, keeping
/// the original operands and the resulting [`DateBounds`]. Immutable:
/// no setter can bypass validation, and reconstructing the same value
/// and zone yields the same bounds — relative clocks never re-evaluate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatePredicate {
    value: DateValue,
    zone: String,
    bounds: DateBounds,
}

impl DatePredicate {
    /// Validates `zone` as a bundled IANA name and `value` against it.
    pub fn new(value: DateValue, zone: &str) -> Result<Self, DateError> {
        let tz = load_zone(zone)?;
        let bounds = bounds(&value, &tz)?;
        Ok(Self {
            value,
            zone: zone.to_owned(),
            bounds,
        })
    }

    /// The original operand as it was validated.
    pub fn value(&self) -> &DateValue {
        &self.value
    }

    /// The zone name in the spelling it was validated under.
    pub fn zone(&self) -> &str {
        &self.zone
    }

    /// The validated UTC-millisecond bounds.
    pub fn bounds(&self) -> DateBounds {
        self.bounds
    }
}

/// Resolves `name` to a zone in the bundled database. `TimeZoneDatabase::get`
/// alone is not a validator: it matches names regardless of ASCII case and
/// specially accepts the unknown-zone sentinel. Requiring the exact spelling
/// in the bundled name list also rejects offsets, POSIX expressions and
/// file paths, none of which are IANA names.
fn load_zone(name: &str) -> Result<TimeZone, DateError> {
    fn rejected() -> DateError {
        DateError::TimeZone("not a bundled IANA time zone name")
    }
    let db = TimeZoneDatabase::bundled();
    // `UTC` is jiff's own constant, not a bundled tzdb entry.
    if name != "UTC" && !db.available().any(|known| known.as_str() == name) {
        return Err(rejected());
    }
    let tz = db.get(name).map_err(|_| rejected())?;
    if tz.is_unknown() {
        return Err(DateError::TimeZone(
            "the unknown-zone sentinel is not a zone",
        ));
    }
    Ok(tz)
}

/// Parses exactly ten ASCII `YYYY-MM-DD` bytes into a real proleptic
/// Gregorian date (year 0000–9999). No time/offset suffix, other width,
/// or alternate separator is normalized.
fn parse_date(raw: &str) -> Result<Date, DateError> {
    fn rejected() -> DateError {
        DateError::Date("expected exactly `YYYY-MM-DD`")
    }
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(rejected());
    }
    let number = |range: std::ops::Range<usize>| -> Result<i32, DateError> {
        let mut value = 0i32;
        for &byte in &bytes[range] {
            if !byte.is_ascii_digit() {
                return Err(rejected());
            }
            value = value * 10 + i32::from(byte - b'0');
        }
        Ok(value)
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    Date::new(
        i16::try_from(year).expect("four digits stay below 10000"),
        i8::try_from(month).expect("two digits stay below 100"),
        i8::try_from(day).expect("two digits stay below 100"),
    )
    .map_err(|_| DateError::Date("not a real proleptic Gregorian date"))
}

/// The earliest actual instant whose local civil time is `dt` or later.
///
/// An unambiguous `dt` maps to itself and a fold maps to its earlier
/// occurrence. For a gap, jiff's offset-based disambiguation re-applies
/// an offset to the missing civil time, which is not the boundary: when
/// a gap straddles midnight that lands the instant inside the previous
/// day or too far into this one. The boundary is the transition that
/// ends the gap — the first instant whose local time has caught up.
fn earliest_at_or_after(tz: &TimeZone, dt: DateTime) -> Result<jiff::Timestamp, DateError> {
    fn overflow() -> DateError {
        DateError::Date("civil boundary overflows the timestamp range")
    }
    let ambiguous = tz.to_ambiguous_timestamp(dt);
    match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { .. } => ambiguous.unambiguous().map_err(|_| overflow()),
        AmbiguousOffset::Fold { .. } => ambiguous.earlier().map_err(|_| overflow()),
        AmbiguousOffset::Gap { .. } => {
            // `earlier` maps the missing civil time to an instant before
            // the gap; the transition ending it is the boundary.
            let before_gap = ambiguous.earlier().map_err(|_| overflow())?;
            tz.following(before_gap)
                .map(|transition| transition.timestamp())
                .find(|&instant| tz.to_datetime(instant) >= dt)
                .ok_or(DateError::Date(
                    "no transition ends the daylight-saving gap",
                ))
        }
    }
}

/// The first instant of `date` in `tz`, or an error when the zone skips
/// the date entirely (e.g. Pacific/Apia 2011-12-30): the gap-ending
/// transition then already carries a later local date.
fn day_start(tz: &TimeZone, date: Date) -> Result<jiff::Timestamp, DateError> {
    let start = earliest_at_or_after(tz, DateTime::from(date))?;
    if tz.to_datetime(start).date() != date {
        return Err(DateError::Date("the zone skips this civil date entirely"));
    }
    Ok(start)
}

/// The first instant after `date`: the boundary at the following civil
/// midnight. An absent next date is not an error — its end boundary is
/// the real transition past it — but civil arithmetic that cannot be
/// represented (past year 9999) is.
fn next_day_start(tz: &TimeZone, date: Date) -> Result<jiff::Timestamp, DateError> {
    let tomorrow = date
        .tomorrow()
        .map_err(|_| DateError::Date("the civil maximum has no following day"))?;
    earliest_at_or_after(tz, DateTime::from(tomorrow))
}

fn to_timestamp(instant: jiff::Timestamp) -> Timestamp {
    Timestamp::from_millis(instant.as_millisecond())
}

fn bounds(value: &DateValue, tz: &TimeZone) -> Result<DateBounds, DateError> {
    match value {
        DateValue::Before(raw) => {
            let date = parse_date(raw)?;
            Ok(DateBounds::Before(to_timestamp(day_start(tz, date)?)))
        }
        DateValue::After(raw) => {
            let date = parse_date(raw)?;
            day_start(tz, date)?;
            Ok(DateBounds::After(to_timestamp(next_day_start(tz, date)?)))
        }
        DateValue::On(raw) => {
            let date = parse_date(raw)?;
            Ok(DateBounds::On {
                start: to_timestamp(day_start(tz, date)?),
                end: to_timestamp(next_day_start(tz, date)?),
            })
        }
        DateValue::WithinDays { days, clock } => {
            if *days == 0 {
                return Err(DateError::Days("within-days needs at least one day"));
            }
            // u32 days times this constant always fits in i64.
            let span = i64::from(*days) * RELATIVE_DAY_MS;
            let start = clock.as_millis().checked_sub(span).ok_or(DateError::Clock(
                "the window underflows the timestamp range",
            ))?;
            Ok(DateBounds::WithinDays {
                start: Timestamp::from_millis(start),
                end: *clock,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::datetime;
    use std::process::Command;

    fn predicate(value: DateValue, zone: &str) -> DatePredicate {
        DatePredicate::new(value, zone).expect("operand should validate")
    }

    fn bounds_of(value: DateValue, zone: &str) -> DateBounds {
        predicate(value, zone).bounds()
    }

    fn error_of(value: DateValue, zone: &str) -> DateError {
        DatePredicate::new(value, zone).expect_err("operand should be rejected")
    }

    /// Independent expectations: known UTC instants pinned through jiff's
    /// RFC 3339 parser, never through the boundary code under test.
    fn ms(iso: &str) -> i64 {
        iso.parse::<jiff::Timestamp>()
            .expect("fixture instant")
            .as_millisecond()
    }

    fn t(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    fn on(raw: &str) -> DateValue {
        DateValue::On(raw.to_owned())
    }

    /// Every operator's bounds, in both UTC and a real offset zone, plus
    /// the immutable predicate's getters, equality and lossless
    /// reconstruction — an explicit relative clock included (spec/06).
    #[test]
    fn all_bounds_in_utc_and_zurich() {
        let cases: [(DateValue, DateBounds); 8] = [
            (
                on("2024-06-15"),
                DateBounds::On {
                    start: t(ms("2024-06-15T00:00:00Z")),
                    end: t(ms("2024-06-16T00:00:00Z")),
                },
            ),
            (
                DateValue::Before("2024-06-15".to_owned()),
                DateBounds::Before(t(ms("2024-06-15T00:00:00Z"))),
            ),
            (
                DateValue::After("2024-06-15".to_owned()),
                DateBounds::After(t(ms("2024-06-16T00:00:00Z"))),
            ),
            (
                DateValue::WithinDays {
                    days: 3,
                    clock: t(1_718_496_000_000),
                },
                DateBounds::WithinDays {
                    start: t(1_718_496_000_000 - 3 * 86_400_000),
                    end: t(1_718_496_000_000),
                },
            ),
            // Europe/Zurich is CEST (+02:00) in June.
            (
                on("2024-06-15"),
                DateBounds::On {
                    start: t(ms("2024-06-14T22:00:00Z")),
                    end: t(ms("2024-06-15T22:00:00Z")),
                },
            ),
            (
                DateValue::Before("2024-06-15".to_owned()),
                DateBounds::Before(t(ms("2024-06-14T22:00:00Z"))),
            ),
            (
                DateValue::After("2024-06-15".to_owned()),
                DateBounds::After(t(ms("2024-06-15T22:00:00Z"))),
            ),
            (
                DateValue::WithinDays {
                    days: 3,
                    clock: t(1_718_496_000_000),
                },
                DateBounds::WithinDays {
                    start: t(1_718_496_000_000 - 3 * 86_400_000),
                    end: t(1_718_496_000_000),
                },
            ),
        ];
        for (i, (value, expected)) in cases.iter().enumerate() {
            let zone = if i < 4 { "UTC" } else { "Europe/Zurich" };
            let pred = predicate(value.clone(), zone);
            assert_eq!(pred.value(), value);
            assert_eq!(pred.zone(), zone);
            assert_eq!(pred.bounds(), *expected, "{value:?} in {zone}");
            // Reconstructing from the kept operands validates identically:
            // relative clocks are never re-evaluated against "now".
            let rebuilt = predicate(pred.value().clone(), pred.zone());
            assert_eq!(rebuilt, pred);
        }
    }

    /// New York's 23- and 25-hour days have known UTC start/end instants;
    /// a relative day stays exactly 86_400_000 ms across the same weekend.
    #[test]
    fn dst_days_are_not_forced_to_fixed_length() {
        let spring = DateBounds::On {
            start: t(ms("2024-03-10T05:00:00Z")),
            end: t(ms("2024-03-11T04:00:00Z")),
        };
        let fall = DateBounds::On {
            start: t(ms("2024-11-03T04:00:00Z")),
            end: t(ms("2024-11-04T05:00:00Z")),
        };
        assert_eq!(bounds_of(on("2024-03-10"), "America/New_York"), spring);
        assert_eq!(bounds_of(on("2024-11-03"), "America/New_York"), fall);
        if let DateBounds::On { start, end } = spring {
            assert_eq!(end.as_millis() - start.as_millis(), 23 * 3_600_000);
        }
        if let DateBounds::On { start, end } = fall {
            assert_eq!(end.as_millis() - start.as_millis(), 25 * 3_600_000);
        }
        // The relative window is fixed-length over the spring weekend.
        let clock = t(ms("2024-03-11T04:00:00Z"));
        assert_eq!(
            bounds_of(DateValue::WithinDays { days: 1, clock }, "America/New_York"),
            DateBounds::WithinDays {
                start: t(ms("2024-03-10T04:00:00Z")),
                end: clock,
            },
        );
    }

    /// A gap sitting on civil midnight (São Paulo 2018-11-04, 00:00→01:00)
    /// starts the day at the transition ending the gap, and a midnight
    /// fold (Havana 2011-11-13, 01:00→00:00) picks the earlier of the
    /// two real midnights.
    #[test]
    fn midnight_gap_and_fold_use_real_transitions() {
        // São Paulo: the 03:00Z transition ends the gap at local 01:00.
        let sp = DateBounds::On {
            start: t(ms("2018-11-04T03:00:00Z")),
            end: t(ms("2018-11-05T02:00:00Z")),
        };
        assert_eq!(bounds_of(on("2018-11-04"), "America/Sao_Paulo"), sp);

        // Havana: the 2011-11-13 fall-back (local 01:00→00:00) folds over
        // civil midnight, which occurs at 04:00Z and again at 05:00Z; the
        // earlier occurrence starts the day.
        let zone = load_zone("America/Havana").unwrap();
        let midnight = datetime(2011, 11, 13, 0, 0, 0, 0);
        assert!(matches!(
            zone.to_ambiguous_timestamp(midnight).offset(),
            AmbiguousOffset::Fold { .. }
        ));
        let start = earliest_at_or_after(&zone, midnight).unwrap();
        assert_eq!(start.as_millisecond(), ms("2011-11-13T04:00:00Z"));
        let havana = DateBounds::On {
            start: t(ms("2011-11-13T04:00:00Z")),
            end: t(ms("2011-11-14T05:00:00Z")),
        };
        assert_eq!(bounds_of(on("2011-11-13"), "America/Havana"), havana);
    }

    /// The POSIX zone STD0DST,J60/23:30,J300/23:30 has a real jiff gap
    /// straddling midnight: local 2026-03-01T23:30 jumps to 03-02T00:30.
    /// March 2's first instant is therefore 2026-03-01T23:30Z — what the
    /// transition ending the gap establishes — while jiff's `compatible`
    /// disambiguation of the missing midnight lands at 00:00Z (local
    /// 01:00), half an hour too far into the day. This exercises the private
    /// boundary helper; the public constructor rejects POSIX input.
    #[test]
    fn gap_straddling_midnight_starts_at_gap_end() {
        let zone = TimeZone::posix("STD0DST,J60/23:30,J300/23:30").unwrap();
        let midnight = datetime(2026, 3, 2, 0, 0, 0, 0);
        assert!(matches!(
            zone.to_ambiguous_timestamp(midnight).offset(),
            AmbiguousOffset::Gap { .. }
        ));
        let compatible = zone.to_ambiguous_timestamp(midnight).compatible().unwrap();
        assert_eq!(compatible.as_millisecond(), ms("2026-03-02T00:00:00Z"));
        let start = earliest_at_or_after(&zone, midnight).unwrap();
        assert_eq!(start.as_millisecond(), ms("2026-03-01T23:30:00Z"));
        // The first instant is already civil March 2 locally (00:30).
        assert_eq!(zone.to_datetime(start), datetime(2026, 3, 2, 0, 30, 0, 0));
        // The day ends at the next midnight boundary, 00:00 local = 23:00Z.
        let next = next_day_start(&zone, date_of(start, &zone)).unwrap();
        assert_eq!(next.as_millisecond(), ms("2026-03-02T23:00:00Z"));
    }

    fn date_of(instant: jiff::Timestamp, zone: &TimeZone) -> Date {
        zone.to_datetime(instant).date()
    }

    /// Apia skipped 2011-12-30 entirely crossing the dateline: requesting
    /// it is a date error, while the existing 29th uses the real 10:00Z
    /// transition as its upper boundary rather than failing.
    #[test]
    fn a_zone_skipping_the_requested_date_is_rejected() {
        assert_eq!(
            bounds_of(on("2011-12-29"), "Pacific/Apia"),
            DateBounds::On {
                start: t(ms("2011-12-29T10:00:00Z")),
                end: t(ms("2011-12-30T10:00:00Z")),
            },
        );
        assert_eq!(
            bounds_of(DateValue::After("2011-12-29".to_owned()), "Pacific/Apia"),
            DateBounds::After(t(ms("2011-12-30T10:00:00Z"))),
        );
        for value in [
            on("2011-12-30"),
            DateValue::After("2011-12-30".to_owned()),
            DateValue::Before("2011-12-30".to_owned()),
        ] {
            assert_eq!(
                error_of(value, "Pacific/Apia"),
                DateError::Date("the zone skips this civil date entirely"),
            );
        }
    }

    /// Exactly ten ASCII `YYYY-MM-DD` bytes naming a real proleptic
    /// Gregorian date; every other shape or impossible day is refused.
    #[test]
    fn absolute_dates_are_strict() {
        for raw in [
            "",
            "2024-6-15",
            "2024-06-5",
            "024-06-15",
            "2024/06/15",
            "2024-06-15T00:00",
            "2024-06-15 00:00:00",
            "2024-06-15+13:00",
            "2024-06-15Z",
            " 2024-06-15",
            "2024-06-15 ",
            "2024-0A-15",
            "2024-06-١٥",
            "2024-13-01",
            "2024-00-10",
            "2024-01-00",
            "2024-01-32",
            "2023-02-29",
            "1900-02-29",
        ] {
            assert!(
                matches!(error_of(on(raw), "UTC"), DateError::Date(_)),
                "{raw:?} should fail"
            );
        }
        // Leap day, century leap and the year limits all validate.
        for raw in ["2024-02-29", "2000-02-29", "0000-01-01", "9999-12-29"] {
            predicate(on(raw), "UTC");
        }
        assert_eq!(
            bounds_of(on("0000-01-01"), "UTC"),
            DateBounds::On {
                start: t(ms("0000-01-01T00:00:00Z")),
                end: t(ms("0000-01-02T00:00:00Z")),
            },
        );
        // Near the timestamp range's top the required boundaries stop being
        // representable. `Before` on the last day it can express still works
        // — it needs no tomorrow — while `On`/`After` overflow, and nothing
        // under 9999-12-31 can produce a start.
        assert_eq!(
            bounds_of(DateValue::Before("9999-12-30".to_owned()), "UTC"),
            DateBounds::Before(t(ms("9999-12-30T00:00:00Z"))),
        );
        for value in [
            on("9999-12-30"),
            DateValue::After("9999-12-30".to_owned()),
            DateValue::Before("9999-12-31".to_owned()),
            on("9999-12-31"),
        ] {
            assert!(
                matches!(error_of(value.clone(), "UTC"), DateError::Date(_)),
                "{value:?} must not produce an unrepresentable boundary"
            );
        }
    }

    /// Only exact-spelling bundled IANA names and aliases pass: wrong
    /// case, unknown names, the unknown-zone sentinel, offsets, POSIX
    /// expressions and file paths are all the timezone operand's error.
    #[test]
    fn zones_match_bundled_names_case_sensitively() {
        for zone in [
            "",
            "america/new_york",
            "AMERICA/NEW_YORK",
            "utc",
            "Etc/Unknown",
            "etc/unknown",
            "Mars/Olympus",
            "+05:00",
            "-1200",
            "STD0DST,J60/23:30,J300/23:30",
            "/usr/share/zoneinfo/America/New_York",
            "../../etc/passwd",
            "America/New_York/extra",
        ] {
            assert!(
                matches!(error_of(on("2024-06-15"), zone), DateError::TimeZone(_)),
                "{zone:?} should fail"
            );
        }
        // Canonical names, aliases and the UTC constant all work, and the
        // validated spelling is kept verbatim.
        for zone in ["UTC", "Etc/UTC", "America/New_York", "US/Eastern", "GMT"] {
            let pred = predicate(on("2024-06-15"), zone);
            assert_eq!(pred.zone(), zone);
        }
        // An alias computes the same bounds as its canonical name.
        assert_eq!(
            bounds_of(on("2024-03-10"), "US/Eastern"),
            bounds_of(on("2024-03-10"), "America/New_York"),
        );
    }

    /// Positive u32 days of exactly 86_400_000 ms; the explicit i64 clock
    /// may be zero, negative or `i64::MAX`, but a window that cannot be
    /// represented refuses instead of saturating.
    #[test]
    fn within_days_limits_are_checked() {
        assert_eq!(
            error_of(
                DateValue::WithinDays {
                    days: 0,
                    clock: t(0),
                },
                "UTC",
            ),
            DateError::Days("within-days needs at least one day"),
        );
        // The zone operand is still validated for relative values.
        assert!(matches!(
            error_of(
                DateValue::WithinDays {
                    days: 1,
                    clock: t(0),
                },
                "Nowhere",
            ),
            DateError::TimeZone(_)
        ));
        // u32::MAX days fits i64 arithmetic without overflow.
        let full = i64::from(u32::MAX) * 86_400_000;
        assert_eq!(
            bounds_of(
                DateValue::WithinDays {
                    days: u32::MAX,
                    clock: t(0),
                },
                "UTC",
            ),
            DateBounds::WithinDays {
                start: t(-full),
                end: t(0),
            },
        );
        // The closed end carries i64::MAX verbatim — no end+1 trickery.
        assert_eq!(
            bounds_of(
                DateValue::WithinDays {
                    days: 1,
                    clock: t(i64::MAX),
                },
                "UTC",
            ),
            DateBounds::WithinDays {
                start: t(i64::MAX - 86_400_000),
                end: t(i64::MAX),
            },
        );
        // A window reaching exactly i64::MIN is allowed; one millisecond
        // further refuses rather than saturating.
        assert_eq!(
            bounds_of(
                DateValue::WithinDays {
                    days: 1,
                    clock: t(i64::MIN + 86_400_000),
                },
                "UTC",
            ),
            DateBounds::WithinDays {
                start: t(i64::MIN),
                end: t(i64::MIN + 86_400_000),
            },
        );
        for clock in [i64::MIN, i64::MIN + 86_399_999] {
            assert!(matches!(
                error_of(
                    DateValue::WithinDays {
                        days: 1,
                        clock: t(clock),
                    },
                    "UTC",
                ),
                DateError::Clock(_)
            ));
        }
        assert_eq!(
            bounds_of(
                DateValue::WithinDays {
                    days: 2,
                    clock: t(-1),
                },
                "UTC",
            ),
            DateBounds::WithinDays {
                start: t(-172_800_001),
                end: t(-1),
            },
        );
    }

    /// Child processes get hostile `TZ`/`TZDIR` settings: the bundled
    /// database and explicit operands must decide the same bounds anyway.
    #[test]
    fn bounds_ignore_process_timezone_environment() {
        let exe = std::env::current_exe().unwrap();
        let child = "query::dates::tests::environment_child";
        let runs = [
            ("TZ", "America/Los_Angeles", None),
            ("TZ", "not/a-zone", None),
            ("TZDIR", "/definitely-not-a-zoneinfo-dir", None),
            (
                "TZ",
                "Pacific/Kiritimati",
                Some(("TZDIR", "/definitely-not-a-zoneinfo-dir")),
            ),
        ];
        for (var, value, extra) in runs {
            let mut command = Command::new(&exe);
            command
                .args([child, "--ignored", "--exact"])
                .env(var, value);
            if let Some((var2, value2)) = extra {
                command.env(var2, value2);
            }
            let status = command.status().expect("spawn child test");
            assert!(status.success(), "child failed with {var}={value}");
        }
    }

    /// Driven only by `bounds_ignore_process_timezone_environment`.
    #[test]
    #[ignore = "child process for the environment-independence proof"]
    fn environment_child() {
        assert_eq!(
            bounds_of(on("2024-03-10"), "America/New_York"),
            DateBounds::On {
                start: t(ms("2024-03-10T05:00:00Z")),
                end: t(ms("2024-03-11T04:00:00Z")),
            },
        );
        let clock = t(ms("2011-12-31T12:34:56Z"));
        assert_eq!(
            bounds_of(DateValue::WithinDays { days: 7, clock }, "Pacific/Apia",),
            DateBounds::WithinDays {
                start: t(ms("2011-12-31T12:34:56Z") - 7 * 86_400_000),
                end: clock,
            },
        );
    }

    /// The error vocabulary distinguishes the failing operand for the
    /// later parser's span attachment (spec/06).
    #[test]
    fn errors_name_the_offending_operand() {
        assert!(matches!(
            error_of(on("not a date"), "UTC"),
            DateError::Date(_)
        ));
        assert!(matches!(
            error_of(on("2024-06-15"), "not a zone"),
            DateError::TimeZone(_)
        ));
        assert!(matches!(
            error_of(
                DateValue::WithinDays {
                    days: 0,
                    clock: t(0),
                },
                "UTC",
            ),
            DateError::Days(_)
        ));
        assert!(matches!(
            error_of(
                DateValue::WithinDays {
                    days: 1,
                    clock: t(i64::MIN),
                },
                "UTC",
            ),
            DateError::Clock(_)
        ));
        assert_eq!(
            DateError::TimeZone("reason").to_string(),
            "invalid time zone operand: reason"
        );
    }
}
