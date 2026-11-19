//! `std.time`: the clocks, the calendar (UTC, proleptic Gregorian) and formatting.
//!
//! All the arithmetic is on integers, and `src/native/runtime.h` has the same functions, so a
//! program prints the same dates and durations on both backends.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Nanoseconds since the first reading in this process.
pub fn monotonic_ns() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

/// Nanoseconds since 1970-01-01 00:00:00 UTC.
pub fn unix_ns() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

/// Wait; the caller has checked that `secs` is sensible.
pub fn sleep(secs: f64) {
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
}

/// `Some(message)` if `secs` is not a duration `sleep` accepts.
pub fn bad_sleep(secs: f64) -> Option<&'static str> {
    if !(secs >= 0.0) {
        Some("sleep needs a number of seconds that is not negative")
    } else if secs > 1e9 {
        Some("sleep: the duration is too long (the limit is 1e9 seconds)")
    } else {
        None
    }
}

/// (year, month, day, hour, minute, second) of a Unix time.
pub fn civil(secs: i64) -> [i64; 6] {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    [y, m, d, rem / 3600, rem % 3600 / 60, rem % 60]
}

/// 0 is Monday.
pub fn weekday(secs: i64) -> i64 {
    (secs.div_euclid(86400) + 3).rem_euclid(7)
}

/// The Unix time of a date, or `None` if it does not exist.
pub fn from_civil(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    const DIM: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(-1_000_000_000..=1_000_000_000).contains(&y) || !(1..=12).contains(&m) || !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=59).contains(&s) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    if d < 1 || d > DIM[(m - 1) as usize] + if m == 2 && leap { 1 } else { 0 } {
        return None;
    }
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + s)
}

/// "2026-11-19T07:13:02Z"; a year before 0 gets a minus sign.
pub fn iso(secs: i64) -> String {
    let [y, mo, d, h, mi, s] = civil(secs);
    let sign = if y < 0 { "-" } else { "" };
    format!("{sign}{:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z", y.abs())
}

/// "950 ns", "1.500 µs", "12.345 ms", "1.250 s", "2m 05s", "1h 02m 05s"; truncated, not rounded.
pub fn duration(ns: i64) -> String {
    let u = ns.unsigned_abs();
    let sign = if ns < 0 { "-" } else { "" };
    let body = if u < 1_000 {
        format!("{u} ns")
    } else if u < 1_000_000 {
        format!("{}.{:03} \u{b5}s", u / 1000, u % 1000)
    } else if u < 1_000_000_000 {
        format!("{}.{:03} ms", u / 1_000_000, u % 1_000_000 / 1000)
    } else if u < 60_000_000_000 {
        format!("{}.{:03} s", u / 1_000_000_000, u % 1_000_000_000 / 1_000_000)
    } else if u < 3_600_000_000_000 {
        format!("{}m {:02}s", u / 60_000_000_000, u % 60_000_000_000 / 1_000_000_000)
    } else {
        format!("{}h {:02}m {:02}s", u / 3_600_000_000_000, u % 3_600_000_000_000 / 60_000_000_000, u % 60_000_000_000 / 1_000_000_000)
    };
    format!("{sign}{body}")
}
