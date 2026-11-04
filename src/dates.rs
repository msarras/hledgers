//! Dates (Hledger.Data.Dates). For now only the calendar date itself, the
//! `Day` of Haskell's Data.Time.

use std::fmt;

/// A date in the proleptic Gregorian calendar, as the number of days since
/// 1970-01-01.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i32);

impl Day {
    /// The date with this year, month (1-12) and day of month, if it exists
    /// (Data.Time's fromGregorianValid).
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Option<Day> {
        let valid = (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month);
        valid.then(|| Day::from_civil(i64::from(year), month, day))
    }

    /// Year, month (1-12) and day of month (Data.Time's toGregorian).
    pub fn ymd(self) -> (i32, u32, u32) {
        // Howard Hinnant's civil_from_days.
        let z = i64::from(self.0) + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        (year as i32, month, day)
    }

    pub fn add_days(self, n: i32) -> Day {
        Day(self.0 + n)
    }

    /// The number of days from `other` to this date (Data.Time's diffDays).
    pub fn diff_days(self, other: Day) -> i32 {
        self.0 - other.0
    }

    /// This date `n` months later (or earlier), with the day of month clipped
    /// to the end of a shorter month (Data.Time's addGregorianMonthsClip).
    pub fn add_months_clip(self, n: i32) -> Day {
        let (y, m, d) = self.ymd();
        let months = i64::from(y) * 12 + i64::from(m) - 1 + i64::from(n);
        let (year, month) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
        let year = i32::try_from(year).expect("a year in Day's range");
        Day::from_civil(i64::from(year), month, d.min(days_in_month(year, month)))
    }

    /// This date `n` years later (or earlier), with 29 February clipped to
    /// the 28th in a common year (Data.Time's addGregorianYearsClip).
    pub fn add_years_clip(self, n: i32) -> Day {
        self.add_months_clip(n * 12)
    }

    /// The day of the week, 1 for Monday to 7 for Sunday.
    pub fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday.
        (i64::from(self.0) + 3).rem_euclid(7) as u32 + 1
    }

    /// The quarter of the year, 1 to 4.
    pub fn quarter(self) -> u32 {
        (self.ymd().1 - 1) / 3 + 1
    }

    /// Howard Hinnant's days_from_civil, for a valid date.
    fn from_civil(year: i64, month: u32, day: u32) -> Day {
        let y = year - i64::from(month <= 2);
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = i64::from(month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Day(i32::try_from(era * 146_097 + doe - 719_468).expect("a date in Day's range"))
    }
}

pub fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The number of days in a month (1-12) of a year.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `YYYY-MM-DD`, as Haskell shows a Day: the year has at least four digits,
/// and a minus sign before them if it is negative.
impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = self.ymd();
        let sign = if y < 0 { "-" } else { "" };
        write!(f, "{sign}{:04}-{m:02}-{d:02}", y.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn epoch_and_round_trips() {
        assert_eq!(day(1970, 1, 1), Day(0));
        assert_eq!(day(1969, 12, 31), Day(-1));
        assert_eq!(day(2000, 3, 1), Day(11_017));
        assert_eq!(day(0, 3, 1).ymd(), (0, 3, 1));
        // Every day from year -221 to 4160 round-trips and follows the last.
        let mut prev = Day(-800_000).ymd();
        assert_eq!(prev.0, -221);
        for n in -799_999..800_000 {
            let (y, m, d) = Day(n).ymd();
            assert_eq!(Day::from_ymd(y, m, d), Some(Day(n)));
            let next = if d == 1 && m == 1 {
                (prev.0 + 1, 12, 31) == (y, prev.1, prev.2)
            } else if d == 1 {
                prev.0 == y && prev.1 + 1 == m && prev.2 == days_in_month(prev.0, prev.1)
            } else {
                prev == (y, m, d - 1)
            };
            assert!(next, "{prev:?} then {:?}", (y, m, d));
            prev = (y, m, d);
        }
    }

    #[test]
    fn invalid_dates() {
        assert_eq!(Day::from_ymd(2023, 2, 29), None);
        assert_eq!(Day::from_ymd(2023, 4, 31), None);
        assert_eq!(Day::from_ymd(2023, 13, 1), None);
        assert_eq!(Day::from_ymd(2023, 0, 1), None);
        assert_eq!(Day::from_ymd(2023, 1, 0), None);
        assert_eq!(Day::from_ymd(2024, 2, 29), Some(day(2024, 2, 29)));
    }

    #[test]
    fn leap_years() {
        assert!(is_leap_year(2000));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2023));
        assert!(is_leap_year(0));
        assert!(is_leap_year(-4));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2023, 9), 30);
        assert_eq!(days_in_month(2023, 12), 31);
    }

    #[test]
    fn arithmetic() {
        assert_eq!(day(2023, 12, 31).add_days(1), day(2024, 1, 1));
        assert_eq!(day(2024, 3, 1).add_days(-1), day(2024, 2, 29));
        assert_eq!(day(2024, 3, 1).diff_days(day(2024, 2, 1)), 29);
        assert_eq!(day(2024, 2, 1).diff_days(day(2024, 3, 1)), -29);
        assert_eq!(day(2024, 1, 31).add_months_clip(1), day(2024, 2, 29));
        assert_eq!(day(2023, 1, 31).add_months_clip(1), day(2023, 2, 28));
        assert_eq!(day(2023, 3, 31).add_months_clip(-1), day(2023, 2, 28));
        assert_eq!(day(2023, 11, 15).add_months_clip(3), day(2024, 2, 15));
        assert_eq!(day(2023, 1, 15).add_months_clip(-13), day(2021, 12, 15));
        assert_eq!(day(2024, 2, 29).add_years_clip(1), day(2025, 2, 28));
        assert_eq!(day(2024, 2, 29).add_years_clip(4), day(2028, 2, 29));
        assert_eq!(day(2024, 2, 29).add_years_clip(-1), day(2023, 2, 28));
    }

    #[test]
    fn weekday_and_quarter() {
        assert_eq!(day(1970, 1, 1).weekday(), 4);
        assert_eq!(day(2024, 1, 1).weekday(), 1);
        assert_eq!(day(2024, 1, 7).weekday(), 7);
        assert_eq!(day(1969, 12, 29).weekday(), 1);
        assert_eq!(day(1600, 3, 1).weekday(), 3);
        assert_eq!(day(2024, 3, 31).quarter(), 1);
        assert_eq!(day(2024, 4, 1).quarter(), 2);
        assert_eq!(day(2024, 9, 30).quarter(), 3);
        assert_eq!(day(2024, 12, 31).quarter(), 4);
    }

    #[test]
    fn display() {
        assert_eq!(day(2024, 3, 5).to_string(), "2024-03-05");
        assert_eq!(day(987, 12, 31).to_string(), "0987-12-31");
        assert_eq!(day(0, 1, 1).to_string(), "0000-01-01");
        assert_eq!(day(-5, 6, 7).to_string(), "-0005-06-07");
        assert_eq!(day(12345, 1, 2).to_string(), "12345-01-02");
        assert!(day(2024, 1, 2) < day(2024, 1, 10));
    }
}
