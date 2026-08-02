//! ISO-8601 timestamp formatting, using civil calendar math rather than
//! pulling in a datetime dependency for one display need.

/// Formats milliseconds since the Unix epoch as an ISO-8601 UTC timestamp
/// (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn iso8601_ms(wall_ms: u64) -> String {
    let secs = wall_ms / 1000;
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day / 60) % 60;
    let second = time_of_day % 60;
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(i64::MAX));
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: converts a day count since the Unix
/// epoch (1970-01-01) into a (year, month, day) proleptic Gregorian date.
/// <http://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::iso8601_ms;

    #[test]
    fn formats_a_leap_day_midnight() {
        assert_eq!(iso8601_ms(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn formats_the_last_second_of_a_year() {
        assert_eq!(iso8601_ms(1_735_689_599_000), "2024-12-31T23:59:59Z");
    }
}
