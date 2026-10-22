//! The stores' timestamp: UTC RFC 3339 with exactly 9 fractional digits and `Z`
//! (nisshi log-schema.md, masu storage-format.md). No date crate (ADR 0002).

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    from_unix(d.as_secs() as i64, d.subsec_nanos())
}

/// journald's `__REALTIME_TIMESTAMP` (microseconds), padded to 9 digits.
pub fn from_micros(us: u64) -> String {
    from_unix((us / 1_000_000) as i64, (us % 1_000_000) as u32 * 1000)
}

/// civil_from_days (Howard Hinnant).
pub fn from_unix(secs: i64, nanos: u32) -> String {
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Unix nanoseconds of a valid timestamp (days_from_civil, Howard Hinnant); `None` outside
/// what an `i64` holds (about 1677 to 2262), though the schema accepts years 0000 to 9999.
pub fn to_unix_nanos(s: &str) -> Option<i64> {
    if !is_timestamp(s) {
        return None;
    }
    let n = |a: usize, b: usize| s[a..b].parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + n(11, 13)? * 3600 + n(14, 16)? * 60 + n(17, 19)?;
    (secs as i128 * 1_000_000_000 + n(20, 29)? as i128)
        .try_into()
        .ok()
}

/// `2026-10-19T08:14:00.000213904Z`: shape, then a real UTC date and time.
pub fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 30 || !s.is_ascii() {
        return false;
    }
    let shape = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (29, b'Z'),
    ];
    if shape.iter().any(|&(i, c)| b[i] != c) {
        return false;
    }
    let num = |from: usize, to: usize| -> Option<u32> {
        b[from..to]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| s[from..to].parse().ok())?
    };
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(sec), Some(_)) = (
        num(0, 4),
        num(5, 7),
        num(8, 10),
        num(11, 13),
        num(14, 16),
        num(17, 19),
        num(20, 29),
    ) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&mo) && d >= 1 && d <= days[mo as usize - 1] && h < 24 && mi < 60 && sec < 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_validates() {
        assert_eq!(from_unix(0, 0), "1970-01-01T00:00:00.000000000Z");
        assert_eq!(
            from_unix(1_792_397_643, 512_004_117),
            "2026-10-19T08:14:03.512004117Z"
        );
        assert_eq!(
            from_micros(1_792_397_643_512_004),
            "2026-10-19T08:14:03.512004000Z"
        );
        assert!(is_timestamp(&now()));
        assert_eq!(
            to_unix_nanos("2026-10-19T08:14:03.512004117Z"),
            Some(1_792_397_643_512_004_117)
        );
        assert_eq!(to_unix_nanos("1970-01-01T00:00:00.000000000Z"), Some(0));
        assert_eq!(
            to_unix_nanos("2028-02-29T23:59:59.999999999Z").map(|n| from_unix(
                n.div_euclid(1_000_000_000),
                n.rem_euclid(1_000_000_000) as u32
            )),
            Some("2028-02-29T23:59:59.999999999Z".to_string())
        );
        assert_eq!(to_unix_nanos("x"), None);
        // Valid timestamps outside the i64 nanosecond range, and its exact limits.
        assert_eq!(to_unix_nanos("2300-01-01T00:00:00.000000000Z"), None);
        assert_eq!(to_unix_nanos("1600-01-01T00:00:00.000000000Z"), None);
        assert_eq!(
            to_unix_nanos("2262-04-11T23:47:16.854775807Z"),
            Some(i64::MAX)
        );
        assert_eq!(to_unix_nanos("2262-04-11T23:47:16.854775808Z"), None);
        assert_eq!(
            to_unix_nanos("1677-09-21T00:12:43.145224192Z"),
            Some(i64::MIN)
        );
        assert_eq!(to_unix_nanos("1677-09-21T00:12:43.145224191Z"), None);
        assert!(is_timestamp("2028-02-29T23:59:59.999999999Z"));
        for bad in [
            "2026-10-19T08:14:00.000213Z",
            "2026-02-29T08:14:00.000213904Z",
            "2026-10-19T08:14:60.000213904Z",
            "2026-10-19t08:14:00.000213904z",
        ] {
            assert!(!is_timestamp(bad), "{bad}");
        }
    }
}
