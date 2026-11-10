//! Dates and periods (Hledger.Data.Dates, Hledger.Data.Period and
//! Hledger.Data.DayPartition): calendar dates; date spans, whose ends may be
//! exact or flexible; the standard periods (a day, week, month, quarter or
//! year); report intervals and the partitions of time they make; and the
//! parsers of smart dates and period expressions.
//!
//! Functions that upstream runs in IO to learn today's date take it as a
//! parameter instead. Years are kept within 10^14 years of year 0, where
//! upstream's are unbounded: a date further away (a period expression with
//! a twenty-digit year) is moved to that limit rather than wrapping or
//! failing.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use crate::parse::{Alts, PResult, Parser, choice, choice_try, is_non_newline_space, or};

/// A date in the proleptic Gregorian calendar, as the number of days since
/// 1970-01-01.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i64);

/// The furthest year from year 0 that dates are made in, and about the
/// number of days to it.
const MAX_YEAR: i64 = 100_000_000_000_000;
const MAX_DAY: i64 = MAX_YEAR * 366;

impl Day {
    /// The date with this year, month (1-12) and day of month, if it exists
    /// (Data.Time's fromGregorianValid).
    pub fn from_ymd(year: i64, month: u32, day: u32) -> Option<Day> {
        let valid = (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month);
        valid.then(|| Day::from_civil(year.clamp(-MAX_YEAR, MAX_YEAR), month, day))
    }

    /// The date with this year, month and day of month, the month clipped
    /// to 1-12 and the day to the month's length (Data.Time's fromGregorian).
    pub fn from_gregorian(year: i64, month: i64, day: i64) -> Day {
        let year = year.clamp(-MAX_YEAR, MAX_YEAR);
        let month = month.clamp(1, 12) as u32;
        let day = day.clamp(1, i64::from(days_in_month(year, month))) as u32;
        Day::from_civil(year, month, day)
    }

    /// Year, month (1-12) and day of month (Data.Time's toGregorian).
    pub fn ymd(self) -> (i64, u32, u32) {
        // Howard Hinnant's civil_from_days.
        let z = self.0 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        (year, month, day)
    }

    pub fn year(self) -> i64 {
        self.ymd().0
    }

    pub fn add_days(self, n: i64) -> Day {
        Day::from_number(self.0.saturating_add(n))
    }

    /// The number of days from `other` to this date (Data.Time's diffDays).
    pub fn diff_days(self, other: Day) -> i64 {
        self.0 - other.0
    }

    /// This date `n` months later (or earlier), with the day of month clipped
    /// to the end of a shorter month (Data.Time's addGregorianMonthsClip).
    pub fn add_months_clip(self, n: i64) -> Day {
        let (y, m, d) = self.ymd();
        let months = (y * 12 + i64::from(m) - 1).saturating_add(n);
        Day::from_gregorian(
            months.div_euclid(12),
            months.rem_euclid(12) + 1,
            i64::from(d),
        )
    }

    /// This date `n` years later (or earlier), with 29 February clipped to
    /// the 28th in a common year (Data.Time's addGregorianYearsClip).
    pub fn add_years_clip(self, n: i64) -> Day {
        self.add_months_clip(n.saturating_mul(12))
    }

    /// The day of the week, 1 for Monday to 7 for Sunday.
    pub fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday.
        (self.0 + 3).rem_euclid(7) as u32 + 1
    }

    /// The quarter of the year, 1 to 4.
    pub fn quarter(self) -> u32 {
        (self.ymd().1 - 1) / 3 + 1
    }

    /// The ISO 8601 week number, 1 to 53: the week (from Monday) whose
    /// Thursday is in the same year (Data.Time's `%V`).
    pub fn iso_week(self) -> u32 {
        let thursday = startofweek(self).add_days(3);
        let jan1 = Day::from_gregorian(thursday.year(), 1, 1);
        (thursday.diff_days(jan1) / 7 + 1) as u32
    }

    /// Howard Hinnant's days_from_civil, for a valid date.
    fn from_civil(year: i64, month: u32, day: u32) -> Day {
        let y = year - i64::from(month <= 2);
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = i64::from(month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Day::from_number(era * 146_097 + doe - 719_468)
    }

    /// The day with this number, or the first or last day in range.
    fn from_number(n: i64) -> Day {
        Day(n.clamp(-MAX_DAY, MAX_DAY))
    }
}

/// The local time zone's offset from UTC at this time, in seconds, as the C
/// library finds it (the TZ variable, else /etc/localtime); UTC if unknown
/// (getCurrentTimeZone).
pub fn local_utc_offset_seconds(utc_secs: i64) -> i64 {
    static LOCAL: std::sync::OnceLock<Option<tz::TimeZone>> = std::sync::OnceLock::new();
    LOCAL
        .get_or_init(|| match std::env::var("TZ") {
            Ok(s) if s.is_empty() => Some(tz::TimeZone::utc()),
            // a daylight saving zone without rules gets the C library's
            // default, the US rules
            Ok(s) => tz::TimeZone::from_posix_tz(&s)
                .or_else(|_| tz::TimeZone::from_posix_tz(&format!("{s},M3.2.0,M11.1.0")))
                .ok(),
            Err(_) => tz::TimeZone::local().ok(),
        })
        .as_ref()
        .and_then(|z| z.find_local_time_type(utc_secs).ok())
        .map_or(0, |t| i64::from(t.ut_offset()))
}

/// Today's date, in the local time zone (getCurrentDay).
pub fn current_day() -> Day {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let offset = local_utc_offset_seconds(secs);
    Day::from_ymd(1970, 1, 1)
        .expect("a valid date")
        .add_days((secs + offset).div_euclid(86400))
}

pub fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The number of days in a month (1-12) of a year.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `YYYY-MM-DD`, as Haskell shows a Day (showDate): the year has at least
/// four digits, and a minus sign before them if it is negative.
impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = self.ymd();
        let sign = if y < 0 { "-" } else { "" };
        write!(f, "{sign}{:04}-{m:02}-{d:02}", y.unsigned_abs())
    }
}

/// A date as Data.Time's `%F` formats it, which unlike [`Day`]'s Display
/// does not pad the year: `987-01-02`.
fn format_f(d: Day) -> String {
    let (y, m, d) = d.ymd();
    format!("{y}-{m:02}-{d:02}")
}

/// The date used where there is none: 0000-01-01 (nulldate).
pub fn null_date() -> Day {
    Day::from_gregorian(0, 1, 1)
}

/// The day before (prevday).
pub fn prevday(d: Day) -> Day {
    d.add_days(-1)
}

/// The day after (nextday).
pub fn nextday(d: Day) -> Day {
    d.add_days(1)
}

/// The Monday on or before a date: the start of its week (startofweek,
/// thisweek, mondayBefore).
pub fn startofweek(d: Day) -> Day {
    d.add_days(1 - i64::from(d.weekday()))
}

/// The start of the previous week (prevweek).
pub fn prevweek(d: Day) -> Day {
    startofweek(d.add_days(-7))
}

/// The start of the next week (nextweek).
pub fn nextweek(d: Day) -> Day {
    startofweek(d.add_days(7))
}

/// The first of the month (startofmonth, thismonth).
pub fn startofmonth(d: Day) -> Day {
    let (y, m, _) = d.ymd();
    Day::from_gregorian(y, m.into(), 1)
}

/// The first of the previous month (prevmonth).
pub fn prevmonth(d: Day) -> Day {
    startofmonth(d.add_months_clip(-1))
}

/// The first of the next month (nextmonth).
pub fn nextmonth(d: Day) -> Day {
    startofmonth(d.add_months_clip(1))
}

/// The first day of the quarter (startofquarter, thisquarter).
pub fn startofquarter(d: Day) -> Day {
    let (y, m, _) = d.ymd();
    Day::from_gregorian(y, i64::from((m - 1) / 3 * 3 + 1), 1)
}

/// The first of January (startofyear, thisyear).
pub fn startofyear(d: Day) -> Day {
    Day::from_gregorian(d.year(), 1, 1)
}

/// The first of January of the next year (nextyear).
pub fn nextyear(d: Day) -> Day {
    startofyear(d.add_years_clip(1))
}

/// This day of the date's month, clipped to the month (nthdayofmonth).
pub fn nthdayofmonth(n: i64, d: Day) -> Day {
    let (y, m, _) = d.ymd();
    Day::from_gregorian(y, m.into(), n)
}

/// The first of this month before the reference date's month: this year's
/// if earlier in the year, otherwise last year's (prevnamedmonth).
fn prevnamedmonth(month: i64, refdate: Day) -> Day {
    let (y, m, _) = refdate.ymd();
    let y = if month < i64::from(m) { y } else { y - 1 };
    Day::from_gregorian(y, month, 1)
}

/// The first of this month after the reference date's month
/// (nextnamedmonth, thisnamedmonth).
fn nextnamedmonth(month: i64, refdate: Day) -> Day {
    let (y, m, _) = refdate.ymd();
    let y = if month > i64::from(m) { y } else { y + 1 };
    Day::from_gregorian(y, month, 1)
}

/// The last such weekday (1-7) before the reference date
/// (prevnamedweekday).
fn prevnamedweekday(weekday: i64, refdate: Day) -> Day {
    let back = 1 + (i64::from(refdate.weekday()) - weekday - 1).rem_euclid(7);
    refdate.add_days(-back)
}

/// The next such weekday (1-7) after the reference date (nextnamedweekday,
/// thisnamedweekday).
fn nextnamedweekday(weekday: i64, refdate: Day) -> Day {
    let forward = 1 + (weekday - i64::from(refdate.weekday()) - 1).rem_euclid(7);
    refdate.add_days(forward)
}

pub fn valid_month(m: i64) -> bool {
    (1..=12).contains(&m)
}

pub fn valid_day(d: i64) -> bool {
    (1..=31).contains(&d)
}

/// The date with this year, month and day, if it exists
/// (fromGregorianValid, for parsed numbers).
fn from_gregorian_valid(y: i64, m: i64, d: i64) -> Option<Day> {
    let y = y.clamp(-MAX_YEAR, MAX_YEAR);
    let (m, d) = (u32::try_from(m).ok()?, u32::try_from(d).ok()?);
    Day::from_ymd(y, m, d)
}

/// The next date with this month (1-12) and day (1-31), on or after the
/// given date (nextmonthandday). A day past the end of the month runs into
/// the next.
pub fn nextmonthandday(m: i64, n: i64, date: Day) -> Day {
    assert!(
        valid_month(m),
        "nextmonthandday: month should be 1..12, not {m}"
    );
    assert!(
        valid_day(n),
        "nextmonthandday: day should be 1..31, not {n}"
    );
    let at = |s: Day| Day::from_gregorian(s.year(), m, 1).add_days(n - 1);
    let this_year = at(startofyear(date));
    if this_year >= date {
        this_year
    } else {
        at(nextyear(date))
    }
}

/// The next date with this day of month (1-31, clipped to the month), on
/// or after the given date (nextnthdayofmonth).
pub fn nextnthdayofmonth(n: i64, date: Day) -> Day {
    assert!(
        valid_day(n),
        "nextnthdayofmonth: day should be 1..31, not {n}"
    );
    let s = startofmonth(date);
    let this_month = nthdayofmonth(n, s);
    if this_month >= date {
        this_month
    } else {
        nthdayofmonth(n, nextmonth(s))
    }
}

/// The start of the week-long period starting on this weekday (1-7) that
/// contains the date (nthdayofweekcontaining).
pub fn nthdayofweekcontaining(n: i64, d: Day) -> Day {
    let s = startofweek(d);
    let same_week = s.add_days(n - 1);
    if same_week <= d {
        same_week
    } else {
        prevweek(s).add_days(n - 1)
    }
}

/// The next `n`th such weekday (1-7) of a month, on or after the date
/// (nextNthWeekdayOfMonth).
pub fn next_nth_weekday_of_month(n: i64, wd: i64, d: Day) -> Day {
    let this_month = advance_to_nth_weekday(n, wd, startofmonth(d));
    if this_month >= d {
        this_month
    } else {
        advance_to_nth_weekday(n, wd, nextmonth(d))
    }
}

/// The last `n`th such weekday (1-7) of a month, on or before the date
/// (prevNthWeekdayOfMonth).
pub fn prev_nth_weekday_of_month(n: i64, wd: i64, d: Day) -> Day {
    let this_month = advance_to_nth_weekday(n, wd, startofmonth(d));
    if this_month <= d {
        this_month
    } else {
        advance_to_nth_weekday(n, wd, prevmonth(d))
    }
}

/// The `n`th occurrence of a weekday (1-7) on or after the date
/// (advanceToNthWeekday).
pub fn advance_to_nth_weekday(n: i64, wd: i64, s: Day) -> Day {
    let first = startofweek(s).add_days(wd - 1);
    let behind = s.diff_days(first);
    let first = if behind > 0 {
        first.add_days((behind + 6) / 7 * 7)
    } else {
        first
    };
    // a count from the command line can be anything; Haskell's Int wraps
    first.add_days(7i64.wrapping_mul(n.wrapping_sub(1)))
}

/// The date `n` months later, on this day of month as nearly as the month
/// allows, so that stepping through the 31sts gives 1/31, 2/28, 3/31
/// (addGregorianMonthsToMonthday).
pub fn add_gregorian_months_to_monthday(dom: i64, n: i64, d: Day) -> Day {
    let (y, m, _) = d.add_months_clip(n).ymd();
    Day::from_gregorian(y, m.into(), dom)
}

/// A date that is either exact or flexible (EFDay). A flexible date, which
/// comes from a partial date such as a month, may be moved to an interval
/// boundary. Two are equal only if equally exact, as upstream's are;
/// upstream orders them by their days alone, which here is done where
/// they are compared, so that no order disagrees with equality.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EfDay {
    pub day: Day,
    pub exact: bool,
}

impl EfDay {
    pub fn exact(day: Day) -> EfDay {
        EfDay { day, exact: true }
    }

    pub fn flex(day: Day) -> EfDay {
        EfDay { day, exact: false }
    }

    /// Another day, as exact as this one (modifyEFDay).
    pub fn map(self, f: impl FnOnce(Day) -> Day) -> EfDay {
        EfDay {
            day: f(self.day),
            ..self
        }
    }
}

/// An end's day, to order ends by: an open one first.
fn end_day(e: &Option<EfDay>) -> Option<Day> {
    e.map(|e| e.day)
}

/// The date (showEFDate).
impl fmt::Display for EfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.day.fmt(f)
    }
}

/// As Haskell shows it: `Exact 2024-01-01`.
impl fmt::Debug for EfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.exact { "Exact" } else { "Flex" };
        write!(f, "{kind} {}", self.day)
    }
}

/// A span of time from a start date (inclusive) to an end date
/// (exclusive), either of which may be open (DateSpan). The default is the
/// span of all time (nulldatespan).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct DateSpan(pub Option<EfDay>, pub Option<EfDay>);

impl DateSpan {
    /// The span between these exact dates.
    pub fn exact(start: Option<Day>, end: Option<Day>) -> DateSpan {
        DateSpan(start.map(EfDay::exact), end.map(EfDay::exact))
    }

    /// The span between these flexible dates.
    pub fn flex(start: Option<Day>, end: Option<Day>) -> DateSpan {
        DateSpan(start.map(EfDay::flex), end.map(EfDay::flex))
    }

    /// The first date (spanStart).
    pub fn start(&self) -> Option<Day> {
        self.0.map(|d| d.day)
    }

    /// The day after the last date (spanEnd).
    pub fn end(&self) -> Option<Day> {
        self.1.map(|d| d.day)
    }

    /// The start's year (spanStartYear).
    pub fn start_year(&self) -> Option<i64> {
        self.start().map(Day::year)
    }

    /// The years of the start and end, those there are (spanYears).
    pub fn years(&self) -> Vec<i64> {
        self.start()
            .into_iter()
            .chain(self.end())
            .map(Day::year)
            .collect()
    }

    /// The number of days, if both ends are given (daysInSpan).
    pub fn days(&self) -> Option<i64> {
        Some(self.end()?.diff_days(self.start()?))
    }

    /// Whether it has both ends, and no days between them (isEmptySpan).
    pub fn is_empty(&self) -> bool {
        matches!(self, DateSpan(Some(s), Some(e)) if e.day <= s.day)
    }

    /// Whether it includes the date (spanContainsDate).
    pub fn contains(&self, d: Day) -> bool {
        self.start().is_none_or(|s| d >= s) && self.end().is_none_or(|e| d < e)
    }

    /// The days in both spans (spanIntersect). Spans that do not intersect
    /// give an empty span starting at the later start.
    pub fn intersect(self, other: DateSpan) -> DateSpan {
        DateSpan(
            later_definite(self.0, other.0),
            earlier_definite(self.1, other.1),
        )
    }

    /// With the open ends filled from the other span's ends, which can make
    /// a span ending before it starts (spanDefaultsFrom).
    pub fn defaults_from(self, other: DateSpan) -> DateSpan {
        DateSpan(self.0.or(other.0), self.1.or(other.1))
    }

    /// Like [`defaults_from`](DateSpan::defaults_from), but unchanged if
    /// that would end before it starts (spanValidDefaultsFrom).
    pub fn valid_defaults_from(self, other: DateSpan) -> DateSpan {
        match self.defaults_from(other) {
            DateSpan(b, e) if end_day(&b) >= end_day(&e) => self,
            s => s,
        }
    }

    /// The span enclosing both, open if either is (spanUnion).
    pub fn union(self, other: DateSpan) -> DateSpan {
        let later = match (self.1, other.1) {
            (Some(a), Some(b)) => Some(std::cmp::max_by_key(a, b, |e| e.day)),
            _ => None,
        };
        DateSpan(std::cmp::min_by_key(self.0, other.0, end_day), later)
    }

    /// This span extended to include the other's given ends, its open ends
    /// filled from them (spanExtend).
    pub fn extend(self, other: DateSpan) -> DateSpan {
        DateSpan(
            earlier_definite(self.0, other.0),
            later_definite(self.1, other.1),
        )
    }

    /// The most specific period this span is (dateSpanAsPeriod).
    pub fn as_period(&self) -> Period {
        match (self.start(), self.end()) {
            (Some(b), Some(e)) => Period::PeriodBetween(b, e).simplify(),
            (Some(b), None) => Period::PeriodFrom(b),
            (None, Some(e)) => Period::PeriodTo(e),
            (None, None) => Period::PeriodAll,
        }
    }

    /// Like its Display, but a month as its abbreviated name and a week as
    /// `W` and its number (showDateSpanAbbrev).
    pub fn show_abbrev(&self) -> String {
        self.as_period().show_abbrev()
    }

    /// Like [`show_abbrev`](DateSpan::show_abbrev), with these month
    /// abbreviations (showDateSpanAbbrevWith).
    pub fn show_abbrev_with(&self, months: &[&str]) -> String {
        self.as_period().show_abbrev_with(months)
    }

    /// `START..END`, with the inclusive end date, whatever the period; an
    /// open end is left out (showDateSpanFull).
    pub fn show_full(&self) -> String {
        let start = self.start().map(format_f).unwrap_or_default();
        let end = self.end().map(|e| format_f(prevday(e))).unwrap_or_default();
        format!("{start}..{end}")
    }

    /// A period expression for a `date:` query or a period option that
    /// parses back to this span: a standard period in its short form,
    /// otherwise the dates with the exclusive end; nothing for all time
    /// (showDateSpanForQuery).
    pub fn show_for_query(&self) -> String {
        match self.as_period() {
            Period::WeekPeriod(b) => format!("{}..{}", format_f(b), format_f(b.add_days(7))),
            Period::PeriodBetween(b, e) => format!("{}..{}", format_f(b), format_f(e)),
            Period::PeriodTo(e) => format!("..{}", format_f(e)),
            Period::PeriodAll => String::new(),
            p => p.to_string(),
        }
    }

    /// The span with its dates' exactness (showDateSpanDebug).
    pub fn show_debug(&self) -> String {
        let show = |d: Option<EfDay>| d.map_or("Nothing".to_string(), |d| format!("Just ({d:?})"));
        format!("DateSpan ({}) ({})", show(self.0), show(self.1))
    }
}

/// The span as its period shows (showDateSpan), which hides whether the
/// dates are exact.
impl fmt::Display for DateSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_period().fmt(f)
    }
}

/// As upstream shows it: `DateSpan 2024Q1`.
impl fmt::Debug for DateSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DateSpan {self}")
    }
}

/// The later of two ends, an open one counting as no end
/// (laterDefinite); the second if they are the same day.
fn later_definite(a: Option<EfDay>, b: Option<EfDay>) -> Option<EfDay> {
    match (a, b) {
        (a, None) => a,
        (None, b) => b,
        (a, b) => std::cmp::max_by_key(a, b, end_day),
    }
}

/// The earlier of two ends, an open one counting as no end
/// (earlierDefinite); the first if they are the same day.
fn earlier_definite(a: Option<EfDay>, b: Option<EfDay>) -> Option<EfDay> {
    match (a, b) {
        (a, None) => a,
        (None, b) => b,
        (a, b) => std::cmp::min_by_key(a, b, end_day),
    }
}

/// An exact span of no days, matching no date (emptydatespan).
pub fn empty_date_span() -> DateSpan {
    DateSpan::exact(Some(nextday(null_date())), Some(null_date()))
}

/// The span from the first span's start to the last span's end
/// (spansSpan).
pub fn spans_span(spans: &[DateSpan]) -> DateSpan {
    DateSpan(
        spans.first().and_then(|s| s.0),
        spans.last().and_then(|s| s.1),
    )
}

/// The intersection of these spans; all time if there are none
/// (spansIntersect).
pub fn spans_intersect(spans: &[DateSpan]) -> DateSpan {
    match spans.split_last() {
        None => DateSpan::default(),
        Some((last, init)) => init.iter().rev().fold(*last, |acc, s| s.intersect(acc)),
    }
}

/// The union of these spans; all time if there are none (spansUnion).
pub fn spans_union(spans: &[DateSpan]) -> DateSpan {
    match spans.split_last() {
        None => DateSpan::default(),
        Some((last, init)) => init.iter().rev().fold(*last, |acc, s| s.union(acc)),
    }
}

/// The smallest exact span containing these dates (daysSpan).
pub fn days_span(days: impl IntoIterator<Item = Day>) -> DateSpan {
    let (min, max) =
        days.into_iter()
            .fold((None, None), |(lo, hi): (Option<Day>, Option<Day>), d| {
                (
                    Some(lo.map_or(d, |l| l.min(d))),
                    Some(hi.map_or(d, |h| h.max(d))),
                )
            });
    DateSpan::exact(min, max.map(nextday))
}

/// A lookup of the span containing a date among these spans, the one
/// starting latest (and then ending latest) if several do; empty spans are
/// ignored (latestSpanContaining).
pub fn latest_span_containing(spans: &[DateSpan]) -> impl Fn(Day) -> Option<DateSpan> {
    let mut by_days = BTreeMap::new();
    for s in spans.iter().filter(|s| !s.is_empty()) {
        by_days.insert((s.start(), s.end()), *s);
    }
    move |d| {
        let sup = (Some(nextday(d)), None);
        by_days
            .range(..sup)
            .next_back()
            .map(|(_, s)| *s)
            .filter(|s| s.contains(d))
    }
}

/// Items grouped by the span, of these contiguous spans, their dates fall
/// in, in date order; items before the first span are dropped, and spans
/// after the last item are included, empty, only with `show_empty`
/// (groupByDateSpan).
pub fn group_by_date_span<T>(
    show_empty: bool,
    date: impl Fn(&T) -> Day,
    spans: &[DateSpan],
    items: impl IntoIterator<Item = T>,
) -> Vec<(DateSpan, Vec<T>)> {
    let mut dated: Vec<(Day, T)> = items.into_iter().map(|x| (date(&x), x)).collect();
    dated.sort_by_key(|(d, _)| *d);
    let first_start = spans.first().and_then(DateSpan::start);
    let mut rest = dated
        .into_iter()
        .skip_while(|(d, _)| first_start.is_some_and(|s| s > *d))
        .peekable();
    let mut groups = Vec::new();
    for span in spans {
        if rest.peek().is_none() && !show_empty {
            break;
        }
        let mut group = Vec::new();
        while let Some((d, _)) = rest.peek() {
            if span.end() > Some(*d) {
                group.push(rest.next().expect("peeked").1);
            } else {
                break;
            }
        }
        groups.push((*span, group));
    }
    groups
}

/// One of the standard report periods, or a span between dates (Period).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Period {
    DayPeriod(Day),
    /// The week starting on this Monday.
    WeekPeriod(Day),
    MonthPeriod(i64, u32),
    QuarterPeriod(i64, u32),
    YearPeriod(i64),
    /// From the first date to the day before the second.
    PeriodBetween(Day, Day),
    PeriodFrom(Day),
    /// Up to the day before the date.
    PeriodTo(Day),
    #[default]
    PeriodAll,
}

impl Period {
    /// The span of the period (periodAsDateSpan): flexible dates for a
    /// week, month, quarter or year.
    pub fn as_date_span(&self) -> DateSpan {
        use Period::*;
        let month = |y: i64, m: i64| Day::from_gregorian(y, m, 1);
        match *self {
            DayPeriod(d) => DateSpan::exact(Some(d), Some(nextday(d))),
            WeekPeriod(b) => DateSpan::flex(Some(b), Some(b.add_days(7))),
            MonthPeriod(y, m) => {
                let (y2, m2) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                DateSpan::flex(Some(month(y, m.into())), Some(month(y2, m2.into())))
            }
            QuarterPeriod(y, q) => {
                let (y2, q2) = if q == 4 { (y + 1, 1) } else { (y, q + 1) };
                let first = |q: u32| (i64::from(q) - 1) * 3 + 1;
                DateSpan::flex(Some(month(y, first(q))), Some(month(y2, first(q2))))
            }
            YearPeriod(y) => DateSpan::flex(Some(month(y, 1)), Some(month(y + 1, 1))),
            PeriodBetween(b, e) => DateSpan::exact(Some(b), Some(e)),
            PeriodFrom(b) => DateSpan::exact(Some(b), None),
            PeriodTo(e) => DateSpan::exact(None, Some(e)),
            PeriodAll => DateSpan::default(),
        }
    }

    /// The start date, if it has one (periodStart).
    pub fn start(&self) -> Option<Day> {
        self.as_date_span().start()
    }

    /// The day after the last date, if it has one (periodEnd).
    pub fn end(&self) -> Option<Day> {
        self.as_date_span().end()
    }

    /// Whether the period includes the date (periodContainsDate).
    pub fn contains(&self, d: Day) -> bool {
        self.as_date_span().contains(d)
    }

    /// A span between dates as the standard period it is, if it is one
    /// (simplifyPeriod).
    pub fn simplify(self) -> Period {
        use Period::*;
        let PeriodBetween(b, e) = self else {
            return self;
        };
        let ((by, bm, bd), (ey, em, ed)) = (b.ymd(), e.ymd());
        match ((bm, bd), (em, ed)) {
            ((1, 1), (1, 1)) if by + 1 == ey => YearPeriod(by),
            ((1, 1), (4, 1)) if by == ey => QuarterPeriod(by, 1),
            ((4, 1), (7, 1)) if by == ey => QuarterPeriod(by, 2),
            ((7, 1), (10, 1)) if by == ey => QuarterPeriod(by, 3),
            ((10, 1), (1, 1)) if by + 1 == ey => QuarterPeriod(by, 4),
            ((bm, 1), (em, 1)) if by == ey && bm + 1 == em => MonthPeriod(by, bm),
            ((12, 1), (1, 1)) if by + 1 == ey => MonthPeriod(by, 12),
            _ if b.weekday() == 1 && e == b.add_days(7) => WeekPeriod(b),
            _ if e == nextday(b) => DayPeriod(b),
            _ => self,
        }
    }

    /// Whether this is (or simplifies to) a day, week, month, quarter or
    /// year (isStandardPeriod).
    pub fn is_standard(&self) -> bool {
        use Period::*;
        matches!(
            self.simplify(),
            DayPeriod(_) | WeekPeriod(_) | MonthPeriod(..) | QuarterPeriod(..) | YearPeriod(_)
        )
    }

    /// The width of a period of this kind when shown (periodTextWidth).
    pub fn text_width(&self) -> usize {
        use Period::*;
        match self.simplify() {
            DayPeriod(_) => 10,
            WeekPeriod(_) => 13,
            MonthPeriod(..) => 7,
            QuarterPeriod(..) => 6,
            YearPeriod(_) => 4,
            PeriodBetween(..) => 22,
            PeriodFrom(_) | PeriodTo(_) => 12,
            PeriodAll => 2,
        }
    }

    /// Like its Display, but a month as its English abbreviation and a week
    /// as `W` and its number (showPeriodAbbrev).
    pub fn show_abbrev(&self) -> String {
        self.show_abbrev_with(&MONTH_ABBREVS_TITLE)
    }

    /// Like [`show_abbrev`](Period::show_abbrev), with these month
    /// abbreviations (showPeriodAbbrevWith).
    pub fn show_abbrev_with(&self, months: &[&str]) -> String {
        match *self {
            Period::MonthPeriod(_, m) if m > 0 && m as usize <= months.len() => {
                months[m as usize - 1].to_string()
            }
            Period::WeekPeriod(b) => format!("W{:02}", b.iso_week()),
            p => p.to_string(),
        }
    }

    /// The next period of the same kind; others are unchanged (periodNext).
    pub fn next(self) -> Period {
        use Period::*;
        match self {
            DayPeriod(b) => DayPeriod(nextday(b)),
            WeekPeriod(b) => WeekPeriod(b.add_days(7)),
            MonthPeriod(y, 12) => MonthPeriod(y + 1, 1),
            MonthPeriod(y, m) => MonthPeriod(y, m + 1),
            QuarterPeriod(y, 4) => QuarterPeriod(y + 1, 1),
            QuarterPeriod(y, q) => QuarterPeriod(y, q + 1),
            YearPeriod(y) => YearPeriod(y + 1),
            p => p,
        }
    }

    /// The previous period of the same kind; others are unchanged
    /// (periodPrevious).
    pub fn previous(self) -> Period {
        use Period::*;
        match self {
            DayPeriod(b) => DayPeriod(prevday(b)),
            WeekPeriod(b) => WeekPeriod(b.add_days(-7)),
            MonthPeriod(y, 1) => MonthPeriod(y - 1, 12),
            MonthPeriod(y, m) => MonthPeriod(y, m - 1),
            QuarterPeriod(y, 1) => QuarterPeriod(y - 1, 4),
            QuarterPeriod(y, q) => QuarterPeriod(y, q - 1),
            YearPeriod(y) => YearPeriod(y - 1),
            p => p,
        }
    }

    /// The next period, unless it starts after the span (periodNextIn).
    pub fn next_in(self, span: DateSpan) -> Period {
        let next = self.next();
        match (span.end(), next.start()) {
            (Some(e), Some(b)) if b >= e => self,
            (Some(_), None) => self,
            _ => next,
        }
    }

    /// The previous period, unless it ends before the span
    /// (periodPreviousIn).
    pub fn previous_in(self, span: DateSpan) -> Period {
        let prev = self.previous();
        match (span.start(), prev.end()) {
            (Some(b), Some(e)) if e <= b => self,
            (Some(_), None) => self,
            _ => prev,
        }
    }

    /// The period of the same kind containing the date; others are
    /// unchanged (periodMoveTo).
    pub fn move_to(self, d: Day) -> Period {
        use Period::*;
        let (y, m, _) = d.ymd();
        match self {
            DayPeriod(_) => DayPeriod(d),
            WeekPeriod(_) => WeekPeriod(startofweek(d)),
            MonthPeriod(..) => MonthPeriod(y, m),
            QuarterPeriod(..) => QuarterPeriod(y, quarter_containing_month(m)),
            YearPeriod(_) => YearPeriod(y),
            p => p,
        }
    }

    /// The next larger standard period enclosing it: a week becomes the
    /// month its Thursday is in, a year all time (periodGrow).
    pub fn grow(self) -> Period {
        use Period::*;
        match self {
            DayPeriod(b) => WeekPeriod(startofweek(b)),
            WeekPeriod(b) => {
                let (y, m) = year_month_containing_week_starting(b);
                MonthPeriod(y, m)
            }
            MonthPeriod(y, m) => QuarterPeriod(y, quarter_containing_month(m)),
            QuarterPeriod(y, _) => YearPeriod(y),
            YearPeriod(_) => PeriodAll,
            p => p,
        }
    }

    /// The next smaller standard period in it, the one containing today if
    /// it does, otherwise the first; a nonstandard period becomes this year
    /// (periodShrink). As upstream's does, a quarter shrinks to the
    /// current month if today is in that quarter of any year.
    pub fn shrink(self, today: Day) -> Period {
        use Period::*;
        let (ty, tm, _) = today.ymd();
        match self {
            DayPeriod(_) => self,
            WeekPeriod(b) => {
                let (wy, wm) = year_month_containing_week_starting(b);
                if today >= b && today.diff_days(b) < 7 {
                    DayPeriod(today)
                } else if b.ymd().1 != wm {
                    DayPeriod(Day::from_gregorian(wy, wm.into(), 1))
                } else {
                    DayPeriod(b)
                }
            }
            MonthPeriod(y, m) if (ty, tm) == (y, m) => WeekPeriod(startofweek(today)),
            MonthPeriod(y, m) => WeekPeriod(start_of_first_week_in_month(y, m)),
            QuarterPeriod(y, q) if quarter_containing_month(tm) == q => MonthPeriod(y, tm),
            QuarterPeriod(y, q) => MonthPeriod(y, first_month_of_quarter(q)),
            YearPeriod(y) if y == ty => QuarterPeriod(y, quarter_containing_month(tm)),
            YearPeriod(y) => QuarterPeriod(y, 1),
            _ => YearPeriod(ty),
        }
    }
}

/// The period as a compact text (showPeriod): `2024-01-15`, `2024-W03`,
/// `2024-01`, `2024Q1`, `2024`, and `START..END` with the inclusive end
/// date for the others.
impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Period::*;
        match *self {
            DayPeriod(b) => f.write_str(&format_f(b)),
            WeekPeriod(b) => {
                let y = startofweek(b).add_days(3).year();
                write!(f, "{y:04}-W{:02}", b.iso_week())
            }
            MonthPeriod(y, m) => write!(f, "{y:04}-{m:02}"),
            QuarterPeriod(y, q) => write!(f, "{y:04}Q{q}"),
            YearPeriod(y) => write!(f, "{y:04}"),
            PeriodBetween(b, e) => write!(f, "{}..{}", format_f(b), format_f(prevday(e))),
            PeriodFrom(b) => write!(f, "{}..", format_f(b)),
            PeriodTo(e) => write!(f, "..{}", format_f(prevday(e))),
            PeriodAll => f.write_str(".."),
        }
    }
}

/// As Haskell shows it: `MonthPeriod 2024 1`.
impl fmt::Debug for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Period::*;
        let n = show_arg;
        match *self {
            DayPeriod(d) => write!(f, "DayPeriod {d}"),
            WeekPeriod(d) => write!(f, "WeekPeriod {d}"),
            MonthPeriod(y, m) => write!(f, "MonthPeriod {} {m}", n(y)),
            QuarterPeriod(y, q) => write!(f, "QuarterPeriod {} {q}", n(y)),
            YearPeriod(y) => write!(f, "YearPeriod {}", n(y)),
            PeriodBetween(b, e) => write!(f, "PeriodBetween {b} {e}"),
            PeriodFrom(b) => write!(f, "PeriodFrom {b}"),
            PeriodTo(e) => write!(f, "PeriodTo {e}"),
            PeriodAll => f.write_str("PeriodAll"),
        }
    }
}

/// A number as Haskell shows a constructor argument: in parentheses if
/// negative.
fn show_arg(n: i64) -> String {
    if n < 0 {
        format!("({n})")
    } else {
        n.to_string()
    }
}

/// The year and month of the Thursday of the week starting on this day:
/// the month the week is mostly in (yearMonthContainingWeekStarting).
pub fn year_month_containing_week_starting(weekstart: Day) -> (i64, u32) {
    let (y, m, _) = weekstart.add_days(3).ymd();
    (y, m)
}

/// The quarter (1-4) a month (1-12) is in (quarterContainingMonth).
pub fn quarter_containing_month(m: u32) -> u32 {
    (m - 1) / 3 + 1
}

/// The first month (1-12) of a quarter (1-4) (firstMonthOfQuarter).
pub fn first_month_of_quarter(q: u32) -> u32 {
    (q - 1) * 3 + 1
}

/// The Monday starting the first week mostly in this month
/// (startOfFirstWeekInMonth).
pub fn start_of_first_week_in_month(y: i64, m: u32) -> Day {
    let month_start = Day::from_gregorian(y, m.into(), 1);
    let monday = startofweek(month_start);
    if month_start.weekday() <= 4 {
        monday
    } else {
        monday.add_days(7)
    }
}

/// A report interval, which splits a span into periods (Interval).
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Interval {
    #[default]
    NoInterval,
    Days(i64),
    Weeks(i64),
    Months(i64),
    Quarters(i64),
    Years(i64),
    /// The `n`th weekday (1-7) of each month.
    NthWeekdayOfMonth(i64, i64),
    /// This day of each month.
    MonthDay(i64),
    /// This month and day of each year.
    MonthAndDay(i64, i64),
    /// These weekdays (1-7) of each week.
    DaysOfWeek(Vec<i64>),
}

/// As Haskell shows it: `Weeks 2`, `DaysOfWeek [1,5]`.
impl fmt::Debug for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Interval::*;
        let n = |n: &i64| show_arg(*n);
        match self {
            NoInterval => f.write_str("NoInterval"),
            Days(k) => write!(f, "Days {}", n(k)),
            Weeks(k) => write!(f, "Weeks {}", n(k)),
            Months(k) => write!(f, "Months {}", n(k)),
            Quarters(k) => write!(f, "Quarters {}", n(k)),
            Years(k) => write!(f, "Years {}", n(k)),
            NthWeekdayOfMonth(k, wd) => write!(f, "NthWeekdayOfMonth {} {}", n(k), n(wd)),
            MonthDay(d) => write!(f, "MonthDay {}", n(d)),
            MonthAndDay(m, d) => write!(f, "MonthAndDay {} {}", n(m), n(d)),
            DaysOfWeek(ds) => {
                let ds: Vec<String> = ds.iter().map(i64::to_string).collect();
                write!(f, "DaysOfWeek [{}]", ds.join(","))
            }
        }
    }
}

/// A unit of a relative smart date (SmartInterval).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartInterval {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// A date as the user may write it, to be resolved against a reference
/// date as a date or a span (SmartDate): a complete date; a year and maybe
/// a month; a day of month and maybe a month, in the reference year; a
/// month; a number of days, weeks, months, quarters or years from the
/// reference date's; or the last (Less), this or next named month or
/// weekday.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SmartDate {
    CompleteDate(Day),
    AssumeStart(i64, Option<i64>),
    FromReference(Option<i64>, i64),
    Month(i64),
    Relative(i64, SmartInterval),
    RelativeMonth(Ordering, i64),
    RelativeWeekDay(Ordering, i64),
}

/// As Haskell shows it, as some error messages do:
/// `SmartAssumeStart 2018 (Just 13)`.
impl fmt::Debug for SmartDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SmartDate::*;
        let maybe = |m: &Option<i64>| {
            m.map_or("Nothing".to_string(), |m| format!("(Just {})", show_arg(m)))
        };
        let ord = |o: &Ordering| match o {
            Ordering::Less => "LT",
            Ordering::Equal => "EQ",
            Ordering::Greater => "GT",
        };
        match self {
            CompleteDate(d) => write!(f, "SmartCompleteDate {d}"),
            AssumeStart(y, m) => write!(f, "SmartAssumeStart {} {}", show_arg(*y), maybe(m)),
            FromReference(m, d) => write!(f, "SmartFromReference {} {}", maybe(m), show_arg(*d)),
            Month(m) => write!(f, "SmartMonth {}", show_arg(*m)),
            Relative(n, i) => write!(f, "SmartRelative {} {i:?}", show_arg(*n)),
            RelativeMonth(o, m) => write!(f, "SmartRelativeMonth {} {}", ord(o), show_arg(*m)),
            RelativeWeekDay(o, w) => write!(f, "SmartRelativeWeekDay {} {}", ord(o), show_arg(*w)),
        }
    }
}

/// A smart date resolved to a date against the reference date: exact if
/// it gave the day, flexible if it left out the day or less
/// (fixSmartDate). "This" and "next" both mean the next one after the
/// reference date's month or day.
pub fn fix_smart_date(refdate: Day, sdate: &SmartDate) -> EfDay {
    use SmartDate::*;
    let (ry, rm, _) = refdate.ymd();
    let rm = i64::from(rm);
    match *sdate {
        CompleteDate(d) => EfDay::exact(d),
        AssumeStart(y, m) => EfDay::flex(Day::from_gregorian(y, m.unwrap_or(1), 1)),
        FromReference(m, d) => EfDay::exact(Day::from_gregorian(ry, m.unwrap_or(rm), d)),
        Month(m) => EfDay::flex(Day::from_gregorian(ry, m, 1)),
        Relative(n, SmartInterval::Day) => EfDay::exact(refdate.add_days(n)),
        Relative(n, SmartInterval::Week) => {
            EfDay::flex(startofweek(refdate).add_days(n.saturating_mul(7)))
        }
        Relative(n, SmartInterval::Month) => EfDay::flex(startofmonth(refdate).add_months_clip(n)),
        Relative(n, SmartInterval::Quarter) => {
            EfDay::flex(startofquarter(refdate).add_months_clip(n.saturating_mul(3)))
        }
        Relative(n, SmartInterval::Year) => EfDay::flex(startofyear(refdate).add_years_clip(n)),
        RelativeMonth(Ordering::Less, m) => EfDay::flex(prevnamedmonth(m, refdate)),
        RelativeMonth(_, m) => EfDay::flex(nextnamedmonth(m, refdate)),
        RelativeWeekDay(Ordering::Less, wd) => EfDay::exact(prevnamedweekday(wd, refdate)),
        RelativeWeekDay(_, wd) => EfDay::exact(nextnamedweekday(wd, refdate)),
    }
}

/// A smart date resolved to the span it names against the reference date:
/// a day, or with a part left out, the month or year (spanFromSmartDate).
pub fn span_from_smart_date(refdate: Day, sdate: &SmartDate) -> DateSpan {
    use SmartDate::*;
    let (ry, rm, _) = refdate.ymd();
    let rm = i64::from(rm);
    let exact_day = |d: Day| DateSpan::exact(Some(d), Some(nextday(d)));
    let flex_month = |d: Day| DateSpan::flex(Some(startofmonth(d)), Some(nextmonth(d)));
    let months = |d: Day, n: i64, k: i64| {
        DateSpan::flex(
            Some(d.add_months_clip(n.saturating_mul(k))),
            Some(d.add_months_clip(n.saturating_add(1).saturating_mul(k))),
        )
    };
    match *sdate {
        CompleteDate(d) => exact_day(d),
        AssumeStart(y, None) => {
            let d = Day::from_gregorian(y, 1, 1);
            DateSpan::flex(Some(startofyear(d)), Some(nextyear(d)))
        }
        AssumeStart(y, Some(m)) => flex_month(Day::from_gregorian(y, m, 1)),
        FromReference(m, d) => exact_day(Day::from_gregorian(ry, m.unwrap_or(rm), d)),
        Month(m) => flex_month(Day::from_gregorian(ry, m, 1)),
        Relative(n, SmartInterval::Day) => DateSpan::exact(
            Some(refdate.add_days(n)),
            Some(refdate.add_days(n.saturating_add(1))),
        ),
        Relative(n, SmartInterval::Week) => {
            let d = startofweek(refdate);
            let n = n.saturating_mul(7);
            DateSpan::flex(Some(d.add_days(n)), Some(d.add_days(n.saturating_add(7))))
        }
        Relative(n, SmartInterval::Month) => months(startofmonth(refdate), n, 1),
        Relative(n, SmartInterval::Quarter) => months(startofquarter(refdate), n, 3),
        Relative(n, SmartInterval::Year) => months(startofyear(refdate), n, 12),
        RelativeMonth(Ordering::Less, m) => flex_month(prevnamedmonth(m, refdate)),
        RelativeMonth(_, m) => flex_month(nextnamedmonth(m, refdate)),
        RelativeWeekDay(Ordering::Less, wd) => exact_day(prevnamedweekday(wd, refdate)),
        RelativeWeekDay(_, wd) => exact_day(nextnamedweekday(wd, refdate)),
    }
}

/// The month names the parsers accept, in lower case (months).
pub const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The month abbreviations the parsers accept (monthabbrevs).
pub const MONTH_ABBREVS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The weekday names the parsers accept, Monday first (weekdays).
pub const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// The weekday abbreviations the parsers accept (weekdayabbrevs).
pub const WEEKDAY_ABBREVS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The month abbreviations of Data.Time's defaultTimeLocale.
const MONTH_ABBREVS_TITLE: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A period expression parsed against a reference date: an optional report
/// interval and a span (parsePeriodExpr), or the error as megaparsec shows
/// it. The text is read in lower case.
pub fn parse_period_expr(today: Day, s: &str) -> Result<(Interval, DateSpan), String> {
    let s = s.to_lowercase();
    let mut p = Parser::new(&s);
    let r = periodexprp(&mut p, today).and_then(|r| p.eof().map(|_| r));
    r.map_err(|e| e.pretty("", &s))
}

/// A smart date, alone but for trailing spaces, resolved against a
/// reference date (fixSmartDateStrEither'), or the error as megaparsec
/// shows it. The text is read in lower case.
pub fn fix_smart_date_str_ef(today: Day, s: &str) -> Result<EfDay, String> {
    let s = s.to_lowercase();
    let mut p = Parser::new(&s);
    match smartdateonly(&mut p) {
        Ok(d) => Ok(fix_smart_date(today, &d)),
        Err(e) => Err(e.pretty("", &s)),
    }
}

/// Like [`fix_smart_date_str_ef`], giving the date as text
/// (fixSmartDateStrEither).
pub fn fix_smart_date_str(today: Day, s: &str) -> Result<String, String> {
    fix_smart_date_str_ef(today, s).map(|d| d.to_string())
}

/// A date in one of the forms `YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY.MM.DD` or
/// `YYYYMMDD`, with two-digit months and days and a year of any length,
/// perhaps negative, perhaps with white space around (parsedate). Unlike
/// the smart dates, these must be valid.
pub fn parsedate(s: &str) -> Option<Day> {
    let s = s.trim_matches(crate::text::is_space);
    let (negative, t) = match s.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, s),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let two_digits = |x: &str| x.len() == 2 && digits(x);
    let separated = ['-', '/', '.'].into_iter().find_map(|sep| {
        let mut parts = t.split(sep);
        let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
        (parts.next().is_none() && digits(y) && two_digits(m) && two_digits(d)).then_some((y, m, d))
    });
    let (y, m, d) = separated.or_else(|| {
        let n = t.len();
        (n > 4 && digits(t)).then(|| (&t[..n - 4], &t[n - 4..n - 2], &t[n - 2..]))
    })?;
    let y = read_integer(y);
    from_gregorian_valid(if negative { -y } else { y }, read_int(m), read_int(d))
}

/// Digits read as Haskell reads them into an Int, wrapping at 64 bits.
fn read_int(digits: &str) -> i64 {
    digits.bytes().fold(0i64, |n, b| {
        n.wrapping_mul(10).wrapping_add(i64::from(b - b'0'))
    })
}

/// Digits read as Haskell reads them into an Integer, here saturating at
/// 64 bits.
fn read_integer(digits: &str) -> i64 {
    digits.bytes().fold(0i64, |n, b| {
        n.saturating_mul(10).saturating_add(i64::from(b - b'0'))
    })
}

/// Whether a character can separate a date's parts (isDateSepChar).
pub fn is_date_sep_char(c: char) -> bool {
    matches!(c, '-' | '/' | '.')
}

/// Digits as an Int (megaparsec's `decimal`).
fn decimal(p: &mut Parser) -> PResult<i64> {
    p.label("integer", |p| {
        p.take_while1(Some("digit"), |c| c.is_ascii_digit())
    })
    .map(read_int)
}

/// Digits as an Integer.
fn decimal_integer(p: &mut Parser) -> PResult<i64> {
    p.label("integer", |p| {
        p.take_while1(Some("digit"), |c| c.is_ascii_digit())
    })
    .map(read_integer)
}

/// Exactly `n` digits (`count n digitChar`).
fn digits<'a>(p: &mut Parser<'a>, n: usize) -> PResult<&'a str> {
    let start = p.pos;
    for _ in 0..n {
        p.satisfy(Some("digit"), |c| c.is_ascii_digit())?;
    }
    Ok(&p.src[start..p.pos])
}

/// One of these words, ignoring case, as its position in the list (a
/// `choice` of `string'`s).
fn one_of(p: &mut Parser, words: &[&str]) -> PResult<usize> {
    let mut tried = Alts::new(p);
    for (i, w) in words.iter().enumerate() {
        if let Some(r) = tried.run(p, |p| p.string_ci(w).map(|_| i)) {
            return r;
        }
    }
    Err(tried.choice_error(p))
}

/// A year of at least four digits (yearp).
pub fn yearp(p: &mut Parser) -> PResult<i64> {
    let year = p.take_while1(Some("year"), |c| c.is_ascii_digit())?;
    if year.len() < 4 {
        return p.fail(format!("Year must contain at least 4 digits: {year}"));
    }
    Ok(read_integer(year))
}

/// A month name or abbreviation, as 1-12 (`month <|> mon`).
fn month_or_mon(p: &mut Parser) -> PResult<i64> {
    let n = or(p, |p| one_of(p, &MONTHS), |p| one_of(p, &MONTH_ABBREVS))?;
    Ok(n as i64 + 1)
}

/// A weekday name or abbreviation, as 1-7 (weekday).
fn weekday(p: &mut Parser) -> PResult<i64> {
    let names: Vec<&str> = WEEKDAYS.iter().chain(&WEEKDAY_ABBREVS).copied().collect();
    Ok(one_of(p, &names)? as i64 % 7 + 1)
}

/// `last`, `this` or `next`.
fn direction(p: &mut Parser) -> PResult<Ordering> {
    let i = one_of(p, &["last", "this", "next"])?;
    Ok([Ordering::Less, Ordering::Equal, Ordering::Greater][i])
}

/// A number with an optional sign, which spaces may follow (`signed`).
fn signed_decimal(p: &mut Parser) -> PResult<i64> {
    let negative = p.optional(|p| {
        let negative = or(
            p,
            |p| p.char('+').map(|_| false),
            |p| p.char('-').map(|_| true),
        )?;
        p.skip_spaces();
        Ok(negative)
    })?;
    let n = decimal_integer(p)?;
    Ok(if negative == Some(true) {
        n.saturating_neg()
    } else {
        n
    })
}

/// The smart date, or fail with upstream's message if it names a month or
/// day that does not exist (failIfInvalidDate).
fn fail_if_invalid_date(p: &mut Parser, s: SmartDate) -> PResult<SmartDate> {
    let valid = match s {
        SmartDate::AssumeStart(_, Some(m)) | SmartDate::Month(m) => valid_month(m),
        SmartDate::FromReference(m, d) => from_gregorian_valid(2004, m.unwrap_or(1), d).is_some(),
        _ => true,
    };
    if valid {
        Ok(s)
    } else {
        p.fail(format!("bad smart date: {s:?}"))
    }
}

fn bad_date(y: i64, m: i64, d: i64) -> String {
    format!("bad smart date: {y}-{m}-{d}")
}

/// A smart date, as the user may write it (smartdate): `2004`,
/// `2004/10`, `2004/10/1`, `10/1`, `21`, `october`, `oct`, `yesterday`,
/// `today`, `tomorrow`, `last/this/next day/week/month/quarter/year`,
/// `last/this/next monday/january`, `in N days/weeks/...`,
/// `N days/weeks/... ago/ahead`, `20181201`, `201812`. The text is
/// expected to be in lower case.
pub fn smartdate(p: &mut Parser) -> PResult<SmartDate> {
    use SmartDate as S;
    let day = |n| S::Relative(n, SmartInterval::Day);
    choice_try(
        p,
        &mut [
            &mut relative_interval,
            &mut |p| {
                let dir = direction(p)?;
                p.skip_spaces();
                Ok(S::RelativeMonth(dir, month_or_mon(p)?))
            },
            &mut |p| {
                let dir = direction(p)?;
                p.skip_spaces();
                Ok(S::RelativeWeekDay(dir, weekday(p)?))
            },
            &mut yyyymmdd,
            &mut ymd,
            &mut |p| md(p).map(|(m, d)| S::FromReference(Some(m), d)),
            &mut |p| {
                let d = decimal(p)?;
                fail_if_invalid_date(p, S::FromReference(None, d))
            },
            &mut |p| month_or_mon(p).map(S::Month),
            &mut |p| p.string_ci("yesterday").map(|_| day(-1)),
            &mut |p| p.string_ci("today").map(|_| day(0)),
            &mut |p| p.string_ci("tomorrow").map(|_| day(1)),
        ],
    )
}

/// A smart date and nothing else but spaces (smartdateonly).
pub fn smartdateonly(p: &mut Parser) -> PResult<SmartDate> {
    let d = smartdate(p)?;
    p.skip_spaces();
    p.eof()?;
    Ok(d)
}

/// `[in] N|this|last|next day|week|month|quarter|year[s] [ago|ahead]`.
fn relative_interval(p: &mut Parser) -> PResult<SmartDate> {
    p.optional(|p| {
        p.string_ci("in")?;
        p.skip_spaces();
        Ok(())
    })?;
    let num = choice(
        p,
        &mut [
            &mut |p| Ok([0, -1, 1][one_of(p, &["this", "last", "next"])?]),
            &mut signed_decimal,
        ],
    )?;
    p.skip_spaces();
    let units = [
        SmartInterval::Day,
        SmartInterval::Week,
        SmartInterval::Month,
        SmartInterval::Quarter,
        SmartInterval::Year,
    ];
    let unit = units[one_of(p, &["day", "week", "month", "quarter", "year"])?];
    p.optional(|p| p.char_ci('s'))?;
    p.skip_spaces();
    let sign = choice(
        p,
        &mut [
            &mut |p| p.string_ci("ago").map(|_| -1),
            &mut |p| p.string_ci("ahead").map(|_| 1),
            &mut |_| Ok(1),
        ],
    )?;
    Ok(SmartDate::Relative(num.saturating_mul(sign), unit))
}

/// `YYYYMMDD` or `YYYYMM`.
fn yyyymmdd(p: &mut Parser) -> PResult<SmartDate> {
    let y = read_integer(digits(p, 4)?);
    let m = read_int(digits(p, 2)?);
    match p.optional(|p| digits(p, 2))? {
        None => fail_if_invalid_date(p, SmartDate::AssumeStart(y, Some(m))),
        Some(d) => {
            let d = read_int(d);
            match from_gregorian_valid(y, m, d) {
                Some(day) => Ok(SmartDate::CompleteDate(day)),
                None => p.fail(bad_date(y, m, d)),
            }
        }
    }
}

/// `YYYY`, `YYYY-M` or `YYYY-M-D`, with `-`, `/` or `.` between.
fn ymd(p: &mut Parser) -> PResult<SmartDate> {
    let y = yearp(p)?;
    let month_and_day = p.optional(|p| {
        p.attempt(|p| {
            let sep = p.satisfy(None, is_date_sep_char)?;
            let m = decimal(p)?;
            if !valid_month(m) {
                return p.fail(format!("Bad month {m}"));
            }
            let day = p.optional(|p| {
                p.attempt(|p| {
                    p.char(sep)?;
                    let d = decimal(p)?;
                    match from_gregorian_valid(y, m, d) {
                        Some(day) => Ok(day),
                        None => p.fail(bad_date(y, m, d)),
                    }
                })
            })?;
            Ok((m, day))
        })
    })?;
    Ok(match month_and_day {
        None => SmartDate::AssumeStart(y, None),
        Some((m, None)) => SmartDate::AssumeStart(y, Some(m)),
        Some((_, Some(day))) => SmartDate::CompleteDate(day),
    })
}

/// `M-D`, `M/D` or `M.D`, valid in a leap year.
fn md(p: &mut Parser) -> PResult<(i64, i64)> {
    let m = decimal(p)?;
    p.satisfy(None, is_date_sep_char)?;
    let d = decimal(p)?;
    fail_if_invalid_date(p, SmartDate::FromReference(Some(m), d))?;
    Ok((m, d))
}

/// A period expression: an optional report interval and an optional span,
/// in which relative dates are resolved against the reference date
/// (periodexprp): `monthly from 2024`, `every 2 weeks in q3`,
/// `2024-01..2024-03`, `from jan to mar`, `last year`.
pub fn periodexprp(p: &mut Parser, rdate: Day) -> PResult<(Interval, DateSpan)> {
    p.skip_spaces();
    choice_try(
        p,
        &mut [
            &mut |p| {
                let i = reportingintervalp(p)?;
                let span = p.optional(|p| {
                    p.attempt(|p| {
                        p.skip_spaces();
                        period_expr_date_span(p, rdate)
                    })
                })?;
                Ok((i, span.unwrap_or_default()))
            },
            &mut |p| Ok((Interval::NoInterval, period_expr_date_span(p, rdate)?)),
        ],
    )
}

/// A report interval (reportingintervalp): `daily`, `every N weeks`,
/// `biweekly`, `every 2nd day of month`, `every tue,thu`, `every 29th nov`
/// and so on.
pub fn reportingintervalp(p: &mut Parser) -> PResult<Interval> {
    use Interval::*;
    choice_try(
        p,
        &mut [
            &mut |p| try_interval(p, "day", "daily").map(Days),
            &mut |p| try_interval(p, "month", "monthly").map(Months),
            &mut |p| try_interval(p, "quarter", "quarterly").map(Quarters),
            &mut |p| try_interval(p, "year", "yearly").map(Years),
            &mut |p| p.string_ci("biweekly").map(|_| Weeks(2)),
            &mut |p| p.string_ci("fortnightly").map(|_| Weeks(2)),
            &mut |p| p.string_ci("bimonthly").map(|_| Months(2)),
            &mut |p| {
                p.string_ci("every")?;
                p.skip_spaces();
                every(p)
            },
            &mut |p| try_interval(p, "week", "weekly").map(Weeks),
        ],
    )
}

/// The count of an interval written as `daily`, `every day` or
/// `every N days` (tryinterval).
fn try_interval(p: &mut Parser, singular: &str, compact: &str) -> PResult<i64> {
    choice_try(
        p,
        &mut [&mut |p| p.string_ci(compact).map(|_| 1), &mut |p| {
            p.string_ci("every")?;
            p.skip_spaces();
            choice(
                p,
                &mut [&mut |p| p.string_ci(singular).map(|_| 1), &mut |p| {
                    let n = decimal(p)?;
                    p.skip_spaces();
                    p.string_ci(&format!("{singular}s"))?;
                    Ok(n)
                }],
            )
        }],
    )
}

/// What may follow `every` in a report interval other than a count of
/// units.
fn every(p: &mut Parser) -> PResult<Interval> {
    use Interval::*;
    let nth_day = |p: &mut Parser| -> PResult<i64> {
        let n = nth(p)?;
        p.skip_spaces();
        p.string_ci("day")?;
        Ok(n)
    };
    choice_try(
        p,
        &mut [
            &mut |p| {
                let n = nth_day(p)?;
                of(p, "week")?;
                Ok(DaysOfWeek(vec![n]))
            },
            &mut |p| {
                let n = nth_day(p)?;
                opt_of(p, "month")?;
                Ok(MonthDay(n))
            },
            &mut |p| {
                let n = nth(p)?;
                p.skip_spaces();
                let wd = weekday(p)?;
                opt_of(p, "month")?;
                Ok(NthWeekdayOfMonth(n, wd))
            },
            &mut |p| {
                let (m, d) = md(p)?;
                opt_of(p, "year")?;
                Ok(MonthAndDay(m, d))
            },
            &mut |p| {
                let mut days = p.sep_by1(weekday, |p| p.string_ci(","))?;
                days.sort();
                days.dedup();
                Ok(DaysOfWeek(days))
            },
            &mut |p| {
                p.string_ci("weekday")
                    .map(|_| DaysOfWeek(vec![1, 2, 3, 4, 5]))
            },
            &mut |p| p.string_ci("weekendday").map(|_| DaysOfWeek(vec![6, 7])),
            &mut |p| {
                let i = month_and_nth(p)?;
                opt_of(p, "year")?;
                Ok(i)
            },
        ],
    )
}

/// `of PERIOD`, after optional spaces (of_).
fn of(p: &mut Parser, period: &str) -> PResult<()> {
    p.skip_spaces();
    p.string_ci("of")?;
    p.skip_spaces();
    p.string_ci(period)?;
    Ok(())
}

/// An optional `of PERIOD` (optOf_).
fn opt_of(p: &mut Parser, period: &str) -> PResult<()> {
    p.optional(|p| p.attempt(|p| of(p, period)))?;
    Ok(())
}

/// An ordinal number: `1st`, `2nd`, `3rd`, `4th`... (nth).
fn nth(p: &mut Parser) -> PResult<i64> {
    let n = decimal(p)?;
    one_of(p, &["st", "nd", "rd", "th"])?;
    Ok(n)
}

/// A month and an ordinal day in either order, `nov 29th` or `29th nov`
/// (d_o_y, a permutation parser).
fn month_and_nth(p: &mut Parser) -> PResult<Interval> {
    fn month(p: &mut Parser) -> PResult<i64> {
        let m = month_or_mon(p)?;
        p.skip_spaces();
        Ok(m)
    }
    fn day(p: &mut Parser) -> PResult<i64> {
        let d = nth(p)?;
        p.skip_spaces();
        Ok(d)
    }
    // Either part, then the other; failing as `empty` where one is missing.
    let first = p.optional(|p| or(p, |p| month(p).map(Ok), |p| day(p).map(Err)))?;
    let (m, d) = match first {
        None => return Err(p.empty()),
        Some(Ok(m)) => (m, p.optional(day)?.ok_or_else(|| p.empty())?),
        Some(Err(d)) => (p.optional(month)?.ok_or_else(|| p.empty())?, d),
    };
    Ok(Interval::MonthAndDay(m, d))
}

/// The span part of a period expression (periodexprdatespanp).
fn period_expr_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
    choice_try(
        p,
        &mut [
            &mut |p| double_date_span(p, rdate),
            &mut |p| from_date_span(p, rdate),
            &mut |p| to_date_span(p, rdate),
            &mut |p| in_date_span(p, rdate),
        ],
    )
}

/// `from` or `since`.
fn from_or_since(p: &mut Parser) -> PResult<()> {
    or(p, |p| p.string_ci("from"), |p| p.string_ci("since"))?;
    Ok(())
}

/// `[from] DATE to|..|- DATE` (doubledatespanp). A lone number right
/// after an unspaced `-` is not taken as a day of this month, since
/// `2026-13` is more likely a mistyped date.
fn double_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
    p.optional(|p| {
        from_or_since(p)?;
        p.skip_spaces();
        Ok(())
    })?;
    let b = smartdate_or_quarter_start(p, rdate)?;
    let spaced_before = !p.take_while(None, is_non_newline_space).is_empty();
    let sep = choice(
        p,
        &mut [
            &mut |p| p.string_ci("to"),
            &mut |p| p.string(".."),
            &mut |p| p.string("-"),
        ],
    )?;
    let spaced_after = !p.take_while(None, is_non_newline_space).is_empty();
    let e = smartdate_or_quarter_start(p, rdate)?;
    if sep == "-"
        && !spaced_before
        && !spaced_after
        && matches!(e, SmartDate::FromReference(None, _))
    {
        return p.fail(
            "a single number after - is ambiguous here; \
             to mean a day of the current month, write .. or to instead of -",
        );
    }
    Ok(DateSpan(
        Some(fix_smart_date(rdate, &b)),
        Some(fix_smart_date(rdate, &e)),
    ))
}

/// `from|since DATE`, `DATE..` or `DATE-` (fromdatespanp).
fn from_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
    let b = choice(
        p,
        &mut [
            &mut |p| {
                from_or_since(p)?;
                p.skip_spaces();
                smartdate_or_quarter_start(p, rdate)
            },
            &mut |p| {
                let b = smartdate_or_quarter_start(p, rdate)?;
                choice(
                    p,
                    &mut [&mut |p| p.string("..").map(drop), &mut |p| {
                        p.string("-")?;
                        p.not_followed_by(|p| p.satisfy(Some("digit"), |c| c.is_ascii_digit()))
                    }],
                )?;
                Ok(b)
            },
        ],
    )?;
    Ok(DateSpan(Some(fix_smart_date(rdate, &b)), None))
}

/// `to|until|..|- DATE` (todatespanp).
fn to_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
    choice(
        p,
        &mut [
            &mut |p| p.string_ci("to"),
            &mut |p| p.string_ci("until"),
            &mut |p| p.string(".."),
            &mut |p| p.string("-"),
        ],
    )?;
    p.skip_spaces();
    let e = smartdate_or_quarter_start(p, rdate)?;
    Ok(DateSpan(None, Some(fix_smart_date(rdate, &e))))
}

/// `[in] QUARTER|DATE`: the quarter, or the span the smart date names
/// (indatespanp).
fn in_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
    p.optional(|p| {
        p.string_ci("in")?;
        p.skip_spaces();
        Ok(())
    })?;
    choice_try(
        p,
        &mut [
            &mut |p| {
                let (y, q) = quarterp(p, rdate)?;
                Ok(Period::QuarterPeriod(y, q).as_date_span())
            },
            &mut |p| Ok(span_from_smart_date(rdate, &smartdate(p)?)),
        ],
    )
}

/// `qN` or `YYYYqN`, as the year (the reference date's if not given) and
/// the quarter (quarterp).
fn quarterp(p: &mut Parser, rdate: Day) -> PResult<(i64, u32)> {
    let y = or(p, yearp, |_| Ok(rdate.year()))?;
    p.char_ci('q')?;
    let q = p.satisfy(None, |c| ('1'..='4').contains(&c))?;
    Ok((y, q as u32 - '0' as u32))
}

/// The start of a quarter as a complete date, or a smart date
/// (smartdateorquarterstartp).
fn smartdate_or_quarter_start(p: &mut Parser, rdate: Day) -> PResult<SmartDate> {
    choice_try(
        p,
        &mut [
            &mut |p| {
                let (y, q) = quarterp(p, rdate)?;
                let start = Period::QuarterPeriod(y, q).start();
                Ok(SmartDate::CompleteDate(
                    start.expect("a quarter has a start"),
                ))
            },
            &mut smartdate,
        ],
    )
}

/// A partition of time into contiguous periods, each of at least one day,
/// after a historical period with no start (DayPartition); kept as the
/// periods' start dates followed by the day after the last period.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DayPartition {
    bounds: Vec<Day>,
}

impl DayPartition {
    /// The partition with these period boundaries: start dates followed by
    /// the day after the last period, in any order; one date alone makes a
    /// one-day period (boundariesToMaybeDayPartition). None if there are
    /// none.
    pub fn from_boundaries(days: impl IntoIterator<Item = Day>) -> Option<DayPartition> {
        let mut bounds: Vec<Day> = days.into_iter().collect();
        bounds.sort();
        bounds.dedup();
        if bounds.len() == 1 {
            bounds.push(nextday(bounds[0]));
        }
        (bounds.len() > 1).then_some(DayPartition { bounds })
    }

    /// The periods' start and end dates, both inclusive
    /// (dayPartitionToList).
    pub fn periods(&self) -> impl Iterator<Item = (Day, Day)> + '_ {
        self.bounds.windows(2).map(|w| (w[0], prevday(w[1])))
    }

    /// The periods' end dates by their start dates, both inclusive, and the
    /// historical period's end (dayPartitionToPeriodData).
    pub fn to_period_data(&self) -> PeriodData<Day> {
        PeriodData::from_list(prevday(self.bounds[0]), self.periods())
    }

    /// The periods as exact spans (dayPartitionToDateSpans).
    pub fn to_date_spans(&self) -> Vec<DateSpan> {
        self.bounds
            .windows(2)
            .map(|w| DateSpan::exact(Some(w[0]), Some(w[1])))
            .collect()
    }

    /// The first period's start and the last period's end, both inclusive
    /// (dayPartitionStartEnd).
    pub fn start_end(&self) -> (Day, Day) {
        let last = self.bounds[self.bounds.len() - 1];
        (self.bounds[0], prevday(last))
    }

    /// The start and (inclusive) end of the period containing the date, as
    /// if the last period had no end; for a date before the first period,
    /// no start and the historical period's end (dayPartitionFind).
    pub fn find(&self, d: Day) -> (Option<Day>, Day) {
        let starts = &self.bounds[..self.bounds.len() - 1];
        match starts.partition_point(|s| *s <= d) {
            0 => (None, prevday(self.bounds[0])),
            i => (Some(starts[i - 1]), prevday(self.bounds[i])),
        }
    }

    /// The two partitions as one, if they agree on the periods they share
    /// and together leave no gaps (unionDayPartitions).
    pub fn union(&self, other: &DayPartition) -> Option<DayPartition> {
        let mut periods: BTreeMap<Day, Day> = self.periods().collect();
        for (s, e) in other.periods() {
            if periods.insert(s, e).is_some_and(|e0| e0 != e) {
                return None;
            }
        }
        let mut bounds = Vec::with_capacity(periods.len() + 1);
        let mut end = None;
        for (s, e) in periods {
            if end.is_some_and(|end| nextday(end) != s) {
                return None;
            }
            bounds.push(s);
            end = Some(e);
        }
        bounds.push(nextday(end?));
        Some(DayPartition { bounds })
    }
}

/// The periods of a partition, or all time if there is none
/// (maybeDayPartitionToDateSpans).
pub fn maybe_day_partition_to_date_spans(p: Option<&DayPartition>) -> Vec<DateSpan> {
    p.map_or_else(|| vec![DateSpan::default()], DayPartition::to_date_spans)
}

/// A span split into consecutive periods of an interval (splitSpan); none
/// if the span is all time or empty, or open with no interval. With
/// `adjust`, a week, month, quarter, year or nth-weekday interval starts at
/// the boundary of its kind on or before the span's start; the last period
/// always runs to the boundary on or after the span's end.
pub fn split_span(
    adjust: bool,
    i: &Interval,
    ds: DateSpan,
) -> Result<Option<DayPartition>, String> {
    Ok(split_span_boundaries(adjust, i, ds)?.and_then(|(e, bs)| spans_from_boundaries(e, bs)))
}

/// The partition made by these boundaries up to the first one not before
/// `e`; none if the first is not before it (spansFromBoundaries).
fn spans_from_boundaries(e: Day, mut bs: impl Iterator<Item = Day>) -> Option<DayPartition> {
    let first = bs.next().filter(|b| *b < e)?;
    DayPartition::from_boundaries(until(e, std::iter::once(first).chain(bs)))
}

/// The periods [`split_span`] would make, as exact spans generated one at a
/// time, so that a long span with a short interval needs no memory for
/// them (splitSpanToDateSpans).
pub fn split_span_to_date_spans(
    adjust: bool,
    i: &Interval,
    ds: DateSpan,
) -> Result<impl Iterator<Item = DateSpan>, String> {
    let bounds: Box<dyn Iterator<Item = Day>> = match split_span_boundaries(adjust, i, ds)? {
        Some((e, mut bs)) => match bs.next() {
            Some(b) if b < e => Box::new(until(e, std::iter::once(b).chain(bs))),
            _ => Box::new(std::iter::empty()),
        },
        None => Box::new(std::iter::empty()),
    };
    let mut prev = None;
    Ok(bounds.filter_map(move |b| {
        let span = prev.map(|p| DateSpan::exact(Some(p), Some(b)));
        prev = Some(b);
        span
    }))
}

/// The start and exclusive end of the periods [`split_span`] would make
/// (splitSpanStartEnd).
pub fn split_span_start_end(
    adjust: bool,
    i: &Interval,
    ds: DateSpan,
) -> Result<Option<(Day, Day)>, String> {
    let Some((e, mut bs)) = split_span_boundaries(adjust, i, ds)? else {
        return Ok(None);
    };
    let Some(b) = bs.next().filter(|b| *b < e) else {
        return Ok(None);
    };
    let last = until(e, std::iter::once(b).chain(bs)).last();
    Ok(Some((b, last.unwrap_or(b))))
}

/// The start of the period of this interval that contains the date, for
/// the intervals with natural boundaries; otherwise the date
/// (intervalBoundaryBefore).
pub fn interval_boundary_before(i: &Interval, d: Day) -> Result<Day, String> {
    let periods = split_span(true, i, DateSpan::exact(Some(d), Some(nextday(d))))?;
    Ok(periods.map_or(d, |p| p.start_end().0))
}

/// Boundaries before `end`, then the first one not before it
/// (takeUntilFails). Boundaries that stop increasing, as with a count of 0,
/// where upstream would go on forever, end them.
fn until(end: Day, mut bs: impl Iterator<Item = Day>) -> impl Iterator<Item = Day> {
    let mut last: Option<Day> = None;
    std::iter::from_fn(move || {
        if last.is_some_and(|l| l >= end) {
            return None;
        }
        let b = bs.next().filter(|b| last.is_none_or(|l| *b > l))?;
        last = Some(b);
        Some(b)
    })
}

/// Period boundaries, in an increasing and usually endless sequence.
type Boundaries = Box<dyn Iterator<Item = Day>>;

/// The day the periods must reach, and the dates the periods of an
/// interval covering a span start on, the first on or before the span's
/// start (splitSpanBoundaries). A day of month or month that does not
/// exist is an error, as upstream's when it computes the first boundary
/// (the guards of nextnthdayofmonth and nextmonthandday).
fn split_span_boundaries(
    adjust: bool,
    i: &Interval,
    ds: DateSpan,
) -> Result<Option<(Day, Boundaries)>, String> {
    if ds == DateSpan::default() || ds.is_empty() {
        return Ok(None);
    }
    match *i {
        Interval::MonthDay(n) if !valid_day(n) => {
            return Err(format!("nextnthdayofmonth: day should be 1..31, not {n}"));
        }
        Interval::MonthAndDay(m, _) if !valid_month(m) => {
            return Err(format!("nextmonthandday: month should be 1..12, not {m}"));
        }
        Interval::MonthAndDay(_, n) if !valid_day(n) => {
            return Err(format!("nextmonthandday: day should be 1..31, not {n}"));
        }
        _ => {}
    }
    Ok(valid_split_span_boundaries(adjust, i, ds))
}

/// The boundaries of a valid interval over a span that is not all time or
/// empty.
fn valid_split_span_boundaries(
    adjust: bool,
    i: &Interval,
    ds: DateSpan,
) -> Option<(Day, Boundaries)> {
    use Interval::*;
    let same = |d: Day| d;
    let adjusted = |f: fn(Day) -> Day| if adjust { f } else { same };
    match *i {
        NoInterval => match (ds.start(), ds.end()) {
            (Some(s), Some(e)) => Some((e, Box::new([s, e].into_iter()))),
            _ => None,
        },
        Days(n) => boundaries(ds, same, n, |k, d: Day| d.add_days(k)),
        Weeks(n) => boundaries(
            ds,
            adjusted(startofweek),
            n.saturating_mul(7),
            |k, d: Day| d.add_days(k),
        ),
        Months(n) => boundaries(ds, adjusted(startofmonth), n, |k, d: Day| {
            d.add_months_clip(k)
        }),
        Quarters(n) => boundaries(
            ds,
            adjusted(startofquarter),
            n.saturating_mul(3),
            |k, d: Day| d.add_months_clip(k),
        ),
        Years(n) => boundaries(ds, adjusted(startofyear), n, |k, d: Day| {
            d.add_years_clip(k)
        }),
        NthWeekdayOfMonth(n, wd) => {
            let start = move |d| {
                if adjust {
                    prev_nth_weekday_of_month(n, wd, d)
                } else {
                    next_nth_weekday_of_month(n, wd, d)
                }
            };
            let next = move |k: i64, d: Day| match k {
                0 => d,
                k => advance_to_nth_weekday(n, wd, startofmonth(d.add_months_clip(k))),
            };
            boundaries(ds, start, 1, next)
        }
        MonthDay(dom) => boundaries(
            ds,
            move |d| nextnthdayofmonth(dom, d),
            1,
            move |k, d| add_gregorian_months_to_monthday(dom, k, d),
        ),
        MonthAndDay(m, d) => boundaries(
            ds,
            move |x| nextmonthandday(m, d, x),
            1,
            |k, x: Day| x.add_years_clip(k),
        ),
        DaysOfWeek(ref days) => {
            let n = *days.first()?;
            let (s, e) = date_span_split_limits(|d| nthdayofweekcontaining(n, d), nextday, ds)?;
            let base = nthdayofweekcontaining(n, s);
            let mut starts: Vec<Day> = days.iter().map(|d| base.add_days(d - n)).collect();
            starts.sort();
            starts.dedup();
            let k = starts.len() as i64;
            let bounds = (0..).map(move |i: i64| starts[(i % k) as usize].add_days(7 * (i / k)));
            Some((e, Box::new(bounds)))
        }
    }
}

/// The boundaries `start(s)`, then `next(k, start(s))` for k = mult, 2 *
/// mult..., each computed from the first so that month ends are kept
/// (boundaries).
fn boundaries(
    ds: DateSpan,
    start: impl Fn(Day) -> Day,
    mult: i64,
    next: impl Fn(i64, Day) -> Day + 'static,
) -> Option<(Day, Boundaries)> {
    let (s, e) = date_span_split_limits(&start, |d| next(mult, d), ds)?;
    let first = start(s);
    Some((
        e,
        Box::new((0..).map(move |k: i64| next(k.saturating_mul(mult), first))),
    ))
}

/// The start (adjusted) and end of a span to split, an open end being one
/// interval after the given one (dateSpanSplitLimits).
fn date_span_split_limits(
    start: impl Fn(Day) -> Day,
    next: impl Fn(Day) -> Day,
    ds: DateSpan,
) -> Option<(Day, Day)> {
    if ds.is_empty() {
        return None;
    }
    match (ds.start(), ds.end()) {
        (Some(s), Some(e)) => Some((start(s), e)),
        (Some(d), None) | (None, Some(d)) => {
            let s = start(d);
            Some((s, next(s)))
        }
        (None, None) => None,
    }
}

// Dates read with strptime-style formats (Data.Time.Format.Parse's
// parseTimeM with defaultTimeLocale), for CSV rules' date-format and
// timezone. A format is matched in every way it can be, as ReadP does; it
// must match in exactly one.

/// A specifier's numeric padding, which a `-`, `_` or `0` after the `%`
/// sets (ParseNumericPadding).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Padding {
    No,
    Space,
    Zero,
}

/// Where spaces may pad a number: before it, or after it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PadSide {
    Pre,
    Post,
}

const MONTH_NAMES: [(&str, &str); 12] = [
    ("January", "Jan"),
    ("February", "Feb"),
    ("March", "Mar"),
    ("April", "Apr"),
    ("May", "May"),
    ("June", "Jun"),
    ("July", "Jul"),
    ("August", "Aug"),
    ("September", "Sep"),
    ("October", "Oct"),
    ("November", "Nov"),
    ("December", "Dec"),
];

const WEEKDAY_NAMES: [(&str, &str); 7] = [
    ("Sunday", "Sun"),
    ("Monday", "Mon"),
    ("Tuesday", "Tue"),
    ("Wednesday", "Wed"),
    ("Thursday", "Thu"),
    ("Friday", "Fri"),
    ("Saturday", "Sat"),
];

/// The time zones defaultTimeLocale knows by name, with their offsets in
/// minutes.
const KNOWN_TIME_ZONES: [(&str, i64); 10] = [
    ("UT", 0),
    ("GMT", 0),
    ("EST", -300),
    ("EDT", -240),
    ("CST", -360),
    ("CDT", -300),
    ("MST", -420),
    ("MDT", -360),
    ("PST", -480),
    ("PDT", -420),
];

/// What a parsed specifier found: the specifier and its text.
type Specifiers = Vec<(char, String)>;

/// A character's upper case, when that is one character (Data.Char's
/// toUpper).
fn upper_char(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// The specifiers of a format and their texts in `s`, if the format matches
/// it in exactly one way; with `accept_ws`, white space may surround it
/// (parseTimeSpecifiers).
fn time_specifiers(accept_ws: bool, fmt: &str, s: &str) -> Option<Specifiers> {
    let fmt: Vec<char> = fmt.chars().collect();
    let s: Vec<char> = s.chars().collect();
    let parses = |from: usize| {
        let mut out = Vec::new();
        match_format(&fmt, &s, from, &mut Vec::new(), &mut out);
        out
    };
    let complete: Vec<Specifiers> = if accept_ws {
        // leading spaces are skipped first, unless the format then matches in no way
        let skipped = s.iter().take_while(|c| crate::text::is_space(**c)).count();
        let mut found = parses(skipped);
        if found.is_empty() {
            found = parses(0);
        }
        found
            .into_iter()
            .filter(|(_, end)| s[*end..].iter().all(|c| crate::text::is_space(*c)))
            .map(|(pairs, _)| pairs)
            .collect()
    } else {
        parses(0)
            .into_iter()
            .filter(|(_, end)| *end == s.len())
            .map(|(pairs, _)| pairs)
            .collect()
    };
    match <[Specifiers; 1]>::try_from(complete) {
        Ok([pairs]) => Some(pairs),
        Err(_) => None,
    }
}

/// Every way the format matches a prefix of `s` from `i`: the specifiers
/// found and where the match ends (parseSpecifiers).
fn match_format(
    fmt: &[char],
    s: &[char],
    i: usize,
    pairs: &mut Specifiers,
    out: &mut Vec<(Specifiers, usize)>,
) {
    let Some(&c) = fmt.first() else {
        out.push((pairs.clone(), i));
        return;
    };
    let is_space = crate::text::is_space;
    if c == '%' {
        let mut k = 1;
        let pad = match fmt.get(1) {
            Some('-') => Some(Padding::No),
            Some('_') => Some(Padding::Space),
            Some('0') => Some(Padding::Zero),
            _ => None,
        };
        if pad.is_some() {
            k += 1;
        }
        if fmt.get(k) == Some(&'E') {
            k += 1;
        }
        let Some(&spec) = fmt.get(k) else {
            out.push((pairs.clone(), i));
            return;
        };
        let rest = &fmt[k + 1..];
        if spec == '%' {
            if s.get(i) == Some(&'%') {
                match_format(rest, s, i + 1, pairs, out);
            }
            return;
        }
        let substitute = match spec {
            'c' => Some("%a %b %e %H:%M:%S %Z %Y"),
            'R' => Some("%H:%M"),
            'T' | 'X' => Some("%H:%M:%S"),
            'r' => Some("%I:%M:%S %p"),
            'D' | 'x' => Some("%m/%d/%y"),
            'F' => Some("%Y-%m-%d"),
            'h' => Some("%b"),
            _ => None,
        };
        if let Some(sub) = substitute {
            let mut f: Vec<char> = sub.chars().collect();
            f.extend_from_slice(rest);
            match_format(&f, s, i, pairs, out);
            return;
        }
        for (value, j) in specifier(pad, spec, s, i) {
            pairs.push((spec, value));
            match_format(rest, s, j, pairs, out);
            pairs.pop();
        }
    } else if is_space(c) {
        // one space, and unless the format has another space next, any more
        if s.get(i).is_some_and(|x| is_space(*x)) {
            let mut j = i + 1;
            if !fmt.get(1).is_some_and(|n| is_space(*n)) {
                while s.get(j).is_some_and(|x| is_space(*x)) {
                    j += 1;
                }
            }
            match_format(&fmt[1..], s, j, pairs, out);
        }
    } else if s.get(i).is_some_and(|x| upper_char(*x) == upper_char(c)) {
        match_format(&fmt[1..], s, i + 1, pairs, out);
    }
}

/// The texts a specifier can match at `i`, and where each ends
/// (timeParseTimeSpecifier).
fn specifier(pad: Option<Padding>, c: char, s: &[char], i: usize) -> Vec<(String, usize)> {
    let digits = |default, n| padded_digits(PadSide::Pre, pad.unwrap_or(default), false, n, s, i);
    let negative_or = |default| {
        let p = |j| padded_digits(PadSide::Pre, pad.unwrap_or(default), false, 2, s, j);
        if s.get(i) == Some(&'-') {
            let neg: Vec<(String, usize)> = p(i + 1)
                .into_iter()
                .map(|(d, j)| (format!("-{d}"), j))
                .collect();
            if !neg.is_empty() {
                return neg;
            }
        }
        p(i)
    };
    let one_of = |names: &[&str]| -> Vec<(String, usize)> {
        names
            .iter()
            .filter(|name| {
                let n: Vec<char> = name.chars().collect();
                s.len() >= i + n.len()
                    && n.iter()
                        .zip(&s[i..])
                        .all(|(a, b)| upper_char(*a) == upper_char(*b))
            })
            .map(|name| (name.to_string(), i + name.chars().count()))
            .collect()
    };
    let run = |from: usize, f: fn(char) -> bool| {
        let n = s[from.min(s.len())..].iter().take_while(|c| f(**c)).count();
        (s[from..from + n].iter().collect::<String>(), from + n)
    };
    match c {
        'C' | 'f' => negative_or(Padding::Space),
        'Y' | 'G' => negative_or(Padding::Space),
        'y' | 'g' | 'm' | 'd' | 'V' | 'U' | 'W' | 'H' | 'I' | 'M' | 'S' => digits(Padding::Zero, 2),
        'e' | 'k' | 'l' => digits(Padding::Space, 2),
        'j' => digits(Padding::Zero, 3),
        'B' => one_of(&MONTH_NAMES.map(|m| m.0)),
        'b' => one_of(&MONTH_NAMES.map(|m| m.1)),
        'A' => one_of(&WEEKDAY_NAMES.map(|d| d.0)),
        'a' => one_of(&WEEKDAY_NAMES.map(|d| d.1)),
        'u' => one_of(&["1", "2", "3", "4", "5", "6", "7"]),
        'w' => one_of(&["0", "1", "2", "3", "4", "5", "6"]),
        'P' | 'p' => one_of(&["AM", "PM"]),
        'q' => padded_digits(PadSide::Post, pad.unwrap_or(Padding::Zero), true, 12, s, i),
        'Q' => {
            if s.get(i) == Some(&'.') {
                let found = padded_digits(
                    PadSide::Post,
                    pad.unwrap_or(Padding::No),
                    true,
                    12,
                    s,
                    i + 1,
                );
                if !found.is_empty() {
                    return found;
                }
            }
            vec![(String::new(), i)]
        }
        'z' => numeric_tz(s, i),
        'Z' => {
            let (alpha, end) = run(i, char::is_alphabetic);
            if alpha.is_empty() {
                numeric_tz(s, i)
            } else {
                vec![(alpha, end)]
            }
        }
        's' => {
            if s.get(i) == Some(&'-') {
                let (d, end) = run(i + 1, |c| c.is_ascii_digit());
                if !d.is_empty() {
                    return vec![(format!("-{d}"), end)];
                }
            }
            let (d, end) = run(i, |c| c.is_ascii_digit());
            if d.is_empty() { vec![] } else { vec![(d, end)] }
        }
        _ => vec![],
    }
}

/// The digits of a padded number, each way they can be read
/// (parsePaddedDigits): exactly `n` with zero padding, otherwise one or more
/// (or with `allow_empty`, any), after or before spaces with space padding.
fn padded_digits(
    side: PadSide,
    pad: Padding,
    allow_empty: bool,
    n: usize,
    s: &[char],
    i: usize,
) -> Vec<(String, usize)> {
    let skip_spaces = |j: usize| {
        j + s[j.min(s.len())..]
            .iter()
            .take_while(|c| crate::text::is_space(**c))
            .count()
    };
    if pad == Padding::Zero {
        let found: String = s[i.min(s.len())..].iter().take(n).collect();
        return if found.chars().count() == n && found.chars().all(|c| c.is_ascii_digit()) {
            vec![(found, i + n)]
        } else {
            vec![]
        };
    }
    let start = if pad == Padding::Space && side == PadSide::Pre {
        skip_spaces(i)
    } else {
        i
    };
    let max = s[start.min(s.len())..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    let min = if allow_empty { 0 } else { 1 };
    (min..=max)
        .map(|len| {
            let digits: String = s[start..start + len].iter().collect();
            let end = if pad == Padding::Space && side == PadSide::Post {
                skip_spaces(start + len)
            } else {
                start + len
            };
            (digits, end)
        })
        .collect()
}

/// A numeric time zone, `+HHMM` or `+HH:MM`, as its sign and four digits.
fn numeric_tz(s: &[char], i: usize) -> Vec<(String, usize)> {
    let digits2 = |j: usize| -> Option<String> {
        let d: String = s.get(j..j + 2)?.iter().collect();
        d.chars().all(|c| c.is_ascii_digit()).then_some(d)
    };
    let Some(&sign @ ('+' | '-')) = s.get(i) else {
        return vec![];
    };
    let Some(h) = digits2(i + 1) else {
        return vec![];
    };
    let mut found = Vec::new();
    for j in [i + 3, i + 4] {
        if j == i + 4 && s.get(i + 3) != Some(&':') {
            continue;
        }
        if let Some(m) = digits2(j) {
            found.push((format!("{sign}{h}{m}"), j + 2));
        }
    }
    found
}

/// A number, as Haskell's read reads an Integer from digits, perhaps negative.
fn read_time_number(s: &str) -> Option<i128> {
    s.parse().ok()
}

/// A value if it is within bounds (clipValid).
fn clip_valid(lo: i128, hi: i128, x: i128) -> Option<i128> {
    (lo..=hi).contains(&x).then_some(x)
}

/// The date that specifiers give: the year from the century and the year of
/// the century (1970 by default, a two-digit year 69 or later being in the
/// 1900s), and the first of a month, a day of the year or a week found, with
/// the last day of month or day of week (buildTime for Day).
fn build_day(pairs: &[(char, String)]) -> Option<Day> {
    enum Week {
        Iso,
        Sunday,
        Monday,
    }
    enum Comp {
        Century(i128),
        CenturyYear(i128),
        Month(i128),
        MonthDay(i128),
        YearDay(i128),
        WeekDay(i128),
        Week(Week, i128),
    }
    let index_of = |names: &[&str], x: &str| {
        let x = x.to_uppercase();
        names.iter().position(|n| n.to_uppercase() == x)
    };
    let mut cs = Vec::new();
    for (c, x) in pairs {
        let ra = || read_time_number(x);
        match c {
            'C' | 'f' => cs.push(Comp::Century(ra()?)),
            'Y' | 'G' => {
                let a = ra()?;
                cs.push(Comp::Century(a.div_euclid(100)));
                cs.push(Comp::CenturyYear(a.rem_euclid(100)));
            }
            'y' | 'g' => cs.push(Comp::CenturyYear(ra()?)),
            'B' => cs.push(Comp::Month(
                index_of(&MONTH_NAMES.map(|m| m.0), x)? as i128 + 1,
            )),
            'b' => cs.push(Comp::Month(
                index_of(&MONTH_NAMES.map(|m| m.1), x)? as i128 + 1,
            )),
            'm' => cs.push(Comp::Month(clip_valid(1, 12, ra()?)?)),
            'd' | 'e' => cs.push(Comp::MonthDay(clip_valid(1, 31, ra()?)?)),
            'V' => cs.push(Comp::Week(Week::Iso, clip_valid(1, 53, ra()?)?)),
            'U' => cs.push(Comp::Week(Week::Sunday, clip_valid(0, 53, ra()?)?)),
            'W' => cs.push(Comp::Week(Week::Monday, clip_valid(0, 53, ra()?)?)),
            'u' => cs.push(Comp::WeekDay(clip_valid(1, 7, ra()?)?)),
            'a' | 'A' | 'w' => {
                let d = match c {
                    'a' => index_of(&WEEKDAY_NAMES.map(|d| d.1), x)? as i128,
                    'A' => index_of(&WEEKDAY_NAMES.map(|d| d.0), x)? as i128,
                    _ => clip_valid(0, 6, ra()?)?,
                };
                cs.push(Comp::WeekDay(if d == 0 { 7 } else { d }));
            }
            'j' => cs.push(Comp::YearDay(clip_valid(1, 366, ra()?)?)),
            _ => {}
        }
    }
    let last = |f: fn(&Comp) -> Option<i128>| cs.iter().filter_map(f).next_back();
    let d = last(|c| match c {
        Comp::CenturyYear(x) => Some(*x),
        _ => None,
    })
    .unwrap_or(70);
    let century = last(|c| match c {
        Comp::Century(x) => Some(*x),
        _ => None,
    })
    .unwrap_or(if d >= 69 { 19 } else { 20 });
    let y = i64::try_from(100 * century + d).ok()?;
    let month_day = last(|c| match c {
        Comp::MonthDay(x) => Some(*x),
        _ => None,
    })
    .unwrap_or(1);
    let week_day = last(|c| match c {
        Comp::WeekDay(x) => Some(*x),
        _ => None,
    })
    .unwrap_or(4);
    let jan1 = Day::from_gregorian(y, 1, 1);
    let year_len = if is_leap_year(y) { 366 } else { 365 };
    for c in &cs {
        match c {
            Comp::Month(m) => return Day::from_ymd(y, *m as u32, month_day as u32),
            Comp::YearDay(yd) => {
                return (*yd <= year_len).then(|| jan1.add_days(*yd as i64 - 1));
            }
            Comp::Week(wt, w) => {
                let (w, wd) = (*w as i64, week_day as i64);
                return match wt {
                    Week::Iso => {
                        // weeks start on Monday, week 1 having the year's first Thursday
                        let week1 = |y: i64| {
                            let d = Day::from_gregorian(y, 1, 1).add_days(-3);
                            d.add_days((8 - i64::from(d.weekday())) % 7)
                        };
                        let d = week1(y).add_days((w - 1) * 7 + wd - 1);
                        (w >= 1 && d < week1(y + 1)).then_some(d)
                    }
                    Week::Sunday | Week::Monday => {
                        // week 0 is the days before the year's first Sunday (Monday)
                        let first = jan1.diff_days(Day::from_gregorian(1858, 11, 17));
                        let (zb_first, zb_day) = match wt {
                            Week::Sunday => ((4 - first).rem_euclid(7), wd % 7),
                            _ => ((5 - first).rem_euclid(7), wd - 1),
                        };
                        let yd = zb_first + 7 * (w - 1) + zb_day;
                        ((0..year_len as i64).contains(&yd)).then(|| jan1.add_days(yd))
                    }
                };
            }
            _ => {}
        }
    }
    Day::from_ymd(y, 1, month_day as u32)
}

/// The time of day that specifiers give, as hours, minutes and seconds
/// (buildTime for TimeOfDay). Fractions of a second are not kept.
fn build_time_of_day(pairs: &[(char, String)]) -> Option<(i64, i64, i64)> {
    let (mut h, mut m, mut sec) = (0, 0, 0);
    for (c, x) in pairs {
        let ra = || read_time_number(x);
        match c {
            'P' | 'p' => match x.to_uppercase().as_str() {
                "AM" => h %= 12,
                "PM" if h < 12 => h += 12,
                "PM" => {}
                _ => return None,
            },
            'H' | 'k' => h = clip_valid(0, 23, ra()?)? as i64,
            'I' | 'l' => h = clip_valid(1, 12, ra()?)? as i64,
            'M' => m = clip_valid(0, 59, ra()?)? as i64,
            'S' => sec = clip_valid(0, 60, ra()?)? as i64,
            _ => {}
        }
    }
    Some((h, m, sec))
}

/// A time zone offset, `+HHMM` or `+HH:MM`, in minutes (readTzOffset).
fn read_tz_offset(s: &str) -> Option<i64> {
    let cs: Vec<char> = s.chars().collect();
    let digits: String = match cs.len() {
        5 => cs[1..].iter().collect(),
        6 if cs[3] == ':' => cs[1..3].iter().chain(&cs[4..]).collect(),
        _ => return None,
    };
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let sign = match cs[0] {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = digits[2..].parse().ok()?;
    Some(sign * (60 * h + m))
}

/// The time zone that specifiers give, as minutes east of UTC; UTC by
/// default (buildTime for TimeZone).
fn build_time_zone(pairs: &[(char, String)]) -> Option<i64> {
    let mut tz = 0;
    for (c, x) in pairs {
        match c {
            'z' => tz = read_tz_offset(x)?,
            'Z' => {
                let known = || {
                    let x = x.to_uppercase();
                    KNOWN_TIME_ZONES
                        .iter()
                        .find(|(name, _)| **name == x)
                        .map(|(_, m)| *m)
                };
                let military = || {
                    let mut cs = x.chars();
                    let c = upper_char(cs.next()?);
                    if cs.next().is_some() {
                        return None;
                    }
                    let hours = match c {
                        'A'..='I' => c as i64 - 'A' as i64 + 1,
                        'K'..='M' => c as i64 - 'K' as i64 + 10,
                        'N'..='Y' => 'N' as i64 - c as i64 - 1,
                        'Z' => 0,
                        _ => return None,
                    };
                    Some(hours * 60)
                };
                tz = read_tz_offset(x)
                    .or_else(known)
                    .or_else(|| (x == "UTC").then_some(0))
                    .or_else(military)?;
            }
            _ => {}
        }
    }
    Some(tz)
}

/// A time zone written as `%Z` reads it, in minutes east of UTC, for the
/// CSV timezone rule (parseTimeM False defaultTimeLocale "%Z").
pub fn parse_time_zone(s: &str) -> Option<i64> {
    build_time_zone(&time_specifiers(false, "%Z", s)?)
}

/// A date read with a CSV rules date-format, or else with the formats
/// YYYY/M/D, YYYY-M-D or YYYY.M.D, as a local date
/// (parseDateWithCustomOrDefaultFormats). A format with a time zone (`%Z`,
/// `%z`; `zoned`) gives a moment, which is moved to the local time zone
/// `tz_out`; otherwise the date and time are in the timezone rule's zone
/// `tz_in`, or else the local one. Offsets are in minutes.
pub fn parse_date_with_format(
    zoned: bool,
    tz_in: Option<i64>,
    tz_out: i64,
    format: Option<&str>,
    s: &str,
) -> Option<Day> {
    let defaults = ["%Y/%-m/%-d", "%Y-%-m-%-d", "%Y.%-m.%-d"];
    let formats: Vec<&str> = match format {
        Some(f) => vec![f],
        None => defaults.to_vec(),
    };
    let epoch = Day::from_gregorian(1970, 1, 1);
    formats.into_iter().find_map(|fmt| {
        let pairs = time_specifiers(true, fmt, s)?;
        let day = build_day(&pairs)?;
        let (h, m, sec) = build_time_of_day(&pairs)?;
        let local = i128::from(day.diff_days(epoch)) * 86400 + i128::from(h * 3600 + m * 60 + sec);
        let utc = if zoned {
            let tz = build_time_zone(&pairs)?;
            let mut utc = local - i128::from(tz) * 60;
            for (c, x) in &pairs {
                if *c == 's' {
                    utc = read_time_number(x)?;
                }
            }
            utc
        } else {
            local - i128::from(tz_in.unwrap_or(tz_out)) * 60
        };
        let days = (utc + i128::from(tz_out) * 60).div_euclid(86400);
        Some(epoch.add_days(i64::try_from(days).ok()?))
    })
}

/// A value for each of some contiguous report periods, known by their
/// start dates, and one for the time before them (PeriodData).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeriodData<T> {
    pub pre: T,
    pub periods: BTreeMap<Day, T>,
}

impl<T> PeriodData<T> {
    /// The value before the periods, and the periods' start dates with
    /// their values (periodDataFromList).
    pub fn from_list(pre: T, periods: impl IntoIterator<Item = (Day, T)>) -> PeriodData<T> {
        PeriodData {
            pre,
            periods: periods.into_iter().collect(),
        }
    }

    /// Each value transformed (fmap).
    pub fn map<U>(&self, mut f: impl FnMut(&T) -> U) -> PeriodData<U> {
        PeriodData {
            pre: f(&self.pre),
            periods: self.periods.iter().map(|(d, v)| (*d, f(v))).collect(),
        }
    }

    /// The values of the periods both have, combined (opPeriodData).
    pub fn op<U, V>(&self, other: &PeriodData<U>, f: impl Fn(&T, &U) -> V) -> PeriodData<V> {
        PeriodData {
            pre: f(&self.pre, &other.pre),
            periods: (self.periods.iter())
                .filter_map(|(d, a)| other.periods.get(d).map(|b| (*d, f(a, b))))
                .collect(),
        }
    }

    /// The values of the periods either has: those only in this one, only in
    /// the other, or in both, combined by the three functions
    /// (mergePeriodData).
    pub fn merge<U, V>(
        self,
        other: PeriodData<U>,
        only1: impl Fn(T) -> V,
        only2: impl Fn(U) -> V,
        both: impl Fn(T, U) -> V,
    ) -> PeriodData<V> {
        let mut periods = BTreeMap::new();
        let mut others = other.periods;
        for (d, a) in self.periods {
            let v = match others.remove(&d) {
                Some(b) => both(a, b),
                None => only1(a),
            };
            periods.insert(d, v);
        }
        for (d, b) in others {
            periods.insert(d, only2(b));
        }
        PeriodData {
            pre: both(self.pre, other.pre),
            periods,
        }
    }

    /// With `x` for each period of `pad` this has no value for
    /// (padPeriodData).
    pub fn pad<U>(&mut self, x: &T, pad: &PeriodData<U>)
    where
        T: Clone,
    {
        for d in pad.periods.keys() {
            self.periods.entry(*d).or_insert_with(|| x.clone());
        }
    }

    /// The value of the last period, if any.
    pub fn last(&self) -> Option<&T> {
        self.periods.values().next_back()
    }
}

pub(crate) mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Day {
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

    fn ex(y: i64, m: u32, d: u32) -> Option<EfDay> {
        Some(EfDay::exact(day(y, m, d)))
    }

    fn fl(y: i64, m: u32, d: u32) -> Option<EfDay> {
        Some(EfDay::flex(day(y, m, d)))
    }

    /// A parser run over the whole text, as `parsewith (p <* eof)`.
    fn parse_all<T>(s: &str, f: impl FnOnce(&mut Parser) -> PResult<T>) -> Result<T, ParseError> {
        let mut p = Parser::new(s);
        let r = f(&mut p)?;
        p.eof()?;
        Ok(r)
    }

    /// Haskell's show of an interval and span pair.
    fn show_pair((i, s): &(Interval, DateSpan)) -> String {
        format!("({i:?},{s:?})")
    }

    use crate::parse::ParseError;

    // The doctests of showDateSpanForQuery.
    #[cfg_attr(test, test)]
    pub(crate) fn spans_for_queries() {
        let s = |b, e| DateSpan(b, e).show_for_query();
        assert_eq!(s(ex(2025, 1, 1), ex(2026, 1, 1)), "2025");
        assert_eq!(
            s(ex(2025, 1, 13), ex(2025, 1, 20)),
            "2025-01-13..2025-01-20"
        );
        assert_eq!(
            s(ex(2025, 1, 15), ex(2025, 2, 15)),
            "2025-01-15..2025-02-15"
        );
        assert_eq!(s(ex(2025, 1, 1), None), "2025-01-01..");
        assert_eq!(s(None, ex(2025, 2, 1)), "..2025-02-01");
        assert_eq!(DateSpan::default().show_for_query(), "");
    }

    // The doctests of spanIntersect, spanDefaultsFrom,
    // spanValidDefaultsFrom, spanUnion and spanExtend.
    #[cfg_attr(test, test)]
    pub(crate) fn span_operations() {
        let a = DateSpan(fl(2018, 1, 1), fl(2018, 1, 3));
        let b = DateSpan(fl(2018, 1, 3), fl(2018, 1, 5));
        assert_eq!(
            format!("{:?}", a.intersect(b)),
            "DateSpan 2018-01-03..2018-01-02"
        );
        let from2024 = DateSpan(ex(2024, 1, 1), None);
        let from2025 = DateSpan(ex(2025, 1, 1), None);
        let jan1 = DateSpan(ex(2024, 1, 1), ex(2024, 1, 2));
        assert_eq!(
            format!("{:?}", from2024.defaults_from(jan1)),
            "DateSpan 2024-01-01"
        );
        assert_eq!(
            format!("{:?}", from2025.defaults_from(jan1)),
            "DateSpan 2025-01-01..2024-01-01"
        );
        assert_eq!(
            format!("{:?}", from2025.valid_defaults_from(jan1)),
            "DateSpan 2025-01-01.."
        );
        let to2024 = DateSpan(None, ex(2024, 1, 1));
        let in2024 = DateSpan(ex(2024, 1, 1), ex(2025, 1, 1));
        assert_eq!(
            format!("{:?}", to2024.union(in2024)),
            "DateSpan ..2024-12-31"
        );
        assert_eq!(
            format!("{:?}", in2024.union(to2024)),
            "DateSpan ..2024-12-31"
        );
        let partof2024 = DateSpan(ex(2024, 3, 1), ex(2024, 9, 1));
        for (x, y) in [
            (to2024, in2024),
            (in2024, to2024),
            (partof2024, in2024),
            (in2024, partof2024),
        ] {
            assert_eq!(format!("{:?}", x.extend(y)), "DateSpan 2024");
        }
    }

    // The doctests of fixSmartDate.
    #[cfg_attr(test, test)]
    pub(crate) fn fixing_smart_dates() {
        let t = |s| fix_smart_date_str(day(2008, 11, 26), s).unwrap();
        for (s, d) in [
            ("0000-01-01", "0000-01-01"),
            ("1999-12-02", "1999-12-02"),
            ("1999.12.02", "1999-12-02"),
            ("1999/3/2", "1999-03-02"),
            ("19990302", "1999-03-02"),
            ("2008/2", "2008-02-01"),
            ("0020/2", "0020-02-01"),
            ("1000", "1000-01-01"),
            ("4/2", "2008-04-02"),
            ("2", "2008-11-02"),
            ("January", "2008-01-01"),
            ("feb", "2008-02-01"),
            ("today", "2008-11-26"),
            ("yesterday", "2008-11-25"),
            ("tomorrow", "2008-11-27"),
            ("this day", "2008-11-26"),
            ("last day", "2008-11-25"),
            ("next day", "2008-11-27"),
            ("this week", "2008-11-24"),
            ("last week", "2008-11-17"),
            ("next week", "2008-12-01"),
            ("this month", "2008-11-01"),
            ("last month", "2008-10-01"),
            ("next month", "2008-12-01"),
            ("this quarter", "2008-10-01"),
            ("last quarter", "2008-07-01"),
            ("next quarter", "2009-01-01"),
            ("this year", "2008-01-01"),
            ("last year", "2007-01-01"),
            ("next year", "2009-01-01"),
            ("last wednesday", "2008-11-19"),
            ("this wednesday", "2008-12-03"),
            ("next wednesday", "2008-12-03"),
            ("last january", "2008-01-01"),
            ("this january", "2009-01-01"),
            ("next january", "2009-01-01"),
            ("last november", "2007-11-01"),
            ("this november", "2009-11-01"),
            ("next november", "2009-11-01"),
            ("in 5 days", "2008-12-01"),
            ("in 7 months", "2009-06-01"),
            ("in -2 weeks", "2008-11-10"),
            ("1 quarter ago", "2008-07-01"),
            ("1 week ahead", "2008-12-01"),
        ] {
            assert_eq!(t(s), d, "{s}");
        }
    }

    // The doctests of nextmonthandday, nextnthdayofmonth,
    // nthdayofweekcontaining, nextNthWeekdayOfMonth and
    // prevNthWeekdayOfMonth.
    #[cfg_attr(test, test)]
    pub(crate) fn next_and_previous_dates() {
        let wed22nd = day(2017, 11, 22);
        assert_eq!(nextmonthandday(11, 21, wed22nd), day(2018, 11, 21));
        assert_eq!(nextmonthandday(11, 22, wed22nd), day(2017, 11, 22));
        assert_eq!(nextmonthandday(11, 23, wed22nd), day(2017, 11, 23));
        assert_eq!(nextnthdayofmonth(21, wed22nd), day(2017, 12, 21));
        assert_eq!(nextnthdayofmonth(22, wed22nd), day(2017, 11, 22));
        assert_eq!(nextnthdayofmonth(23, wed22nd), day(2017, 11, 23));
        for (n, d) in [(1, 20), (2, 21), (3, 22), (4, 16), (5, 17)] {
            assert_eq!(nthdayofweekcontaining(n, wed22nd), day(2017, 11, d));
        }
        assert_eq!(next_nth_weekday_of_month(3, 3, wed22nd), day(2017, 12, 20));
        assert_eq!(next_nth_weekday_of_month(4, 3, wed22nd), day(2017, 11, 22));
        assert_eq!(next_nth_weekday_of_month(5, 3, wed22nd), day(2017, 11, 29));
        assert_eq!(prev_nth_weekday_of_month(4, 3, wed22nd), day(2017, 11, 22));
        assert_eq!(prev_nth_weekday_of_month(5, 2, wed22nd), day(2017, 10, 31));
    }

    // The doctests of parsedate, and edge cases checked in GHCi.
    #[cfg_attr(test, test)]
    pub(crate) fn simple_dates() {
        assert_eq!(parsedate("2008/02/03"), Some(day(2008, 2, 3)));
        assert_eq!(parsedate("2008/02/03/"), None);
        assert_eq!(parsedate("2008/02/30"), None);
        assert_eq!(parsedate("2025-01-01"), Some(day(2025, 1, 1)));
        assert_eq!(parsedate("2025.01.01"), Some(day(2025, 1, 1)));
        assert_eq!(parsedate("20250101"), Some(day(2025, 1, 1)));
        assert_eq!(parsedate("00101"), Some(day(0, 1, 1)));
        assert_eq!(parsedate("2008-1-3"), None);
        assert_eq!(parsedate(" 2008-01-03 "), Some(day(2008, 1, 3)));
        assert_eq!(parsedate("-5-01-01"), Some(day(-5, 1, 1)));
        assert_eq!(parsedate("+2008-01-03"), None);
        assert_eq!(parsedate("12008-01-03"), Some(day(12008, 1, 3)));
        assert_eq!(parsedate("2008-01-3"), None);
        assert_eq!(parsedate("200801030"), Some(day(20080, 10, 30)));
        assert_eq!(parsedate("2008-13-01"), None);
    }

    // The doctests of smartdate.
    #[cfg_attr(test, test)]
    pub(crate) fn smart_dates() {
        let sd = |s| parse_all(s, smartdate);
        assert_eq!(
            sd("20181201"),
            Ok(SmartDate::CompleteDate(day(2018, 12, 1)))
        );
        assert_eq!(sd("201804"), Ok(SmartDate::AssumeStart(2018, Some(4))));
        assert_eq!(sd("201813"), Ok(SmartDate::AssumeStart(201813, None)));
        assert!(sd("201801012").is_err());
        assert_eq!(sd("201813012"), Ok(SmartDate::AssumeStart(201813012, None)));
    }

    // The doctests of periodexprp.
    #[cfg_attr(test, test)]
    pub(crate) fn period_expressions() {
        let p = |s| show_pair(&parse_period_expr(day(2008, 11, 26), s).unwrap());
        for (s, r) in [
            (
                "from Aug to Oct",
                "(NoInterval,DateSpan 2008-08-01..2008-09-30)",
            ),
            ("aug to oct", "(NoInterval,DateSpan 2008-08-01..2008-09-30)"),
            ("2009q2", "(NoInterval,DateSpan 2009Q2)"),
            ("Q3", "(NoInterval,DateSpan 2008Q3)"),
            ("every 3 days in Aug", "(Days 3,DateSpan 2008-08)"),
            ("daily from aug", "(Days 1,DateSpan 2008-08-01..)"),
            ("every week to 2009", "(Weeks 1,DateSpan ..2008-12-31)"),
            ("every 2nd day of month", "(MonthDay 2,DateSpan ..)"),
            ("every 2nd day", "(MonthDay 2,DateSpan ..)"),
            ("every 2nd day 2009..", "(MonthDay 2,DateSpan 2009-01-01..)"),
            ("every 2nd day 2009-", "(MonthDay 2,DateSpan 2009-01-01..)"),
            ("every 29th Nov", "(MonthAndDay 11 29,DateSpan ..)"),
            (
                "every 29th nov ..2009",
                "(MonthAndDay 11 29,DateSpan ..2008-12-31)",
            ),
            ("every nov 29th", "(MonthAndDay 11 29,DateSpan ..)"),
            (
                "every Nov 29th 2009..",
                "(MonthAndDay 11 29,DateSpan 2009-01-01..)",
            ),
            (
                "every 11/29 from 2009",
                "(MonthAndDay 11 29,DateSpan 2009-01-01..)",
            ),
            (
                "every 11/29 since 2009",
                "(MonthAndDay 11 29,DateSpan 2009-01-01..)",
            ),
            (
                "every 2nd Thursday of month to 2009",
                "(NthWeekdayOfMonth 2 4,DateSpan ..2008-12-31)",
            ),
            (
                "every 1st monday of month to 2009",
                "(NthWeekdayOfMonth 1 1,DateSpan ..2008-12-31)",
            ),
            ("every tue", "(DaysOfWeek [2],DateSpan ..)"),
            ("every 2nd day of week", "(DaysOfWeek [2],DateSpan ..)"),
            ("every 2nd day of month", "(MonthDay 2,DateSpan ..)"),
            ("every 2nd day", "(MonthDay 2,DateSpan ..)"),
            ("every 2nd day 2009..", "(MonthDay 2,DateSpan 2009-01-01..)"),
            (
                "every 2nd day of month 2009..",
                "(MonthDay 2,DateSpan 2009-01-01..)",
            ),
        ] {
            assert_eq!(p(s), r, "{s}");
        }
    }

    // The doctests of doubledatespanp, fromdatespanp, todatespanp,
    // indatespanp, quarterspanp and quarterstartp.
    #[cfg_attr(test, test)]
    pub(crate) fn span_forms() {
        let d =
            |s| parse_all(s, |p| double_date_span(p, day(2018, 1, 1))).map(|s| format!("{s:?}"));
        assert_eq!(d("20180101-201804").unwrap(), "DateSpan 2018Q1");
        assert_eq!(d("2017..2018").unwrap(), "DateSpan 2017");
        assert_eq!(d("2017-2018").unwrap(), "DateSpan 2017");
        assert_eq!(d("2017-01-2018").unwrap(), "DateSpan 2017");
        assert_eq!(d("2017-01-01-2018").unwrap(), "DateSpan 2017");
        assert!(d("2008-13").is_err());
        assert_eq!(d("2008..13").unwrap(), "DateSpan 2008-01-01..2018-01-12");
        let r = day(2024, 2, 2);
        let f = |s| format!("{:?}", parse_all(s, |p| from_date_span(p, r)).unwrap());
        assert_eq!(f("2025-01-01.."), "DateSpan 2025-01-01..");
        assert_eq!(f("2025Q1.."), "DateSpan 2025-01-01..");
        assert_eq!(f("from q2"), "DateSpan 2024-04-01..");
        let t = |s| format!("{:?}", parse_all(s, |p| to_date_span(p, r)).unwrap());
        assert_eq!(t("..2025-01-01"), "DateSpan ..2024-12-31");
        assert_eq!(t("..2025Q1"), "DateSpan ..2024-12-31");
        assert_eq!(t("to q2"), "DateSpan ..2024-03-31");
        let i = |s| format!("{:?}", parse_all(s, |p| in_date_span(p, r)).unwrap());
        assert_eq!(i("2025-01-01"), "DateSpan 2025-01-01");
        assert_eq!(i("2025q1"), "DateSpan 2025Q1");
        assert_eq!(i("in Q2"), "DateSpan 2024Q2");
        let q = |s, r| {
            let (y, q) = parse_all(s, |p| quarterp(p, r)).unwrap();
            Period::QuarterPeriod(y, q)
        };
        let r = day(2018, 1, 1);
        assert_eq!(
            format!("{:?}", q("q1", r).as_date_span()),
            "DateSpan 2018Q1"
        );
        assert_eq!(
            format!("{:?}", q("Q1", r).as_date_span()),
            "DateSpan 2018Q1"
        );
        assert_eq!(
            format!("{:?}", q("2020q4", r).as_date_span()),
            "DateSpan 2020Q4"
        );
        let r = day(2025, 2, 2);
        assert_eq!(q("q1", r).start(), Some(day(2025, 1, 1)));
        assert_eq!(q("Q2", r).start(), Some(day(2025, 4, 1)));
        assert_eq!(q("2025q4", r).start(), Some(day(2025, 10, 1)));
    }

    // Errors as the hledger binary shows them for `-p EXPR`, each needing
    // megaparsec's hints, merging or backtracking to come out the same.
    #[test]
    fn period_expression_errors() {
        let e = |s| parse_period_expr(day(2024, 5, 15), s).unwrap_err();
        assert_eq!(
            e("2026-13"),
            "1:5:\n  |\n1 | 2026-13\n  |     ^\nunexpected '-'\nexpecting end of input or year\n"
        );
        assert_eq!(
            e("2024-02-30"),
            "1:8:\n  |\n1 | 2024-02-30\n  |        ^\nunexpected '-'\nexpecting digit or end of input\n"
        );
        assert_eq!(
            e("20160801-foobar"),
            "1:10:\n  |\n1 | 20160801-foobar\n  |          ^\nunexpected 'f'\nexpecting end of input\n"
        );
        assert_eq!(
            e("foo bar"),
            "1:1:\n  |\n1 | foo bar\n  | ^^^^^^^\nunexpected \"foo bar\"\n\
             expecting \"..\", \"apr\", \"april\", \"aug\", \"august\", \"bimonthly\", \
             \"biweekly\", '+', '-', 'Q', or other valid values (41 more)\n"
        );
        assert_eq!(
            e("every 2nd thursday of foo"),
            "1:19:\n  |\n1 | every 2nd thursday of foo\n  |                   ^\n\
             unexpected space\nexpecting end of input\n"
        );
        let p = |s| show_pair(&parse_period_expr(day(2024, 5, 15), s).unwrap());
        assert_eq!(p("5 days"), "(NoInterval,DateSpan 2024-05-20)");
        assert_eq!(p("20181301"), "(NoInterval,DateSpan 20181301)");
        assert_eq!(
            p("every mon,wed,fri from 2024-07"),
            "(DaysOfWeek [1,3,5],DateSpan 2024-07-01..)"
        );
    }

    // Shown forms checked in GHCi: %F leaves the year unpadded, printf pads
    // it to four columns with the sign, and show keeps a negative argument
    // in parentheses.
    #[test]
    fn shown_forms() {
        assert_eq!(Period::DayPeriod(day(987, 1, 2)).to_string(), "987-01-02");
        assert_eq!(Period::DayPeriod(day(-5, 1, 2)).to_string(), "-5-01-02");
        assert_eq!(Period::MonthPeriod(-5, 3).to_string(), "-005-03");
        assert_eq!(Period::YearPeriod(5).to_string(), "0005");
        assert_eq!(Period::YearPeriod(12345).to_string(), "12345");
        assert_eq!(Period::WeekPeriod(day(987, 1, 3)).to_string(), "0987-W01");
        assert_eq!(Period::MonthPeriod(2024, 3).show_abbrev(), "Mar");
        assert_eq!(format!("{:?}", Period::YearPeriod(-5)), "YearPeriod (-5)");
        assert_eq!(
            DateSpan(ex(2024, 1, 1), None).show_debug(),
            "DateSpan (Just (Exact 2024-01-01)) (Nothing)"
        );
        assert_eq!(
            format!("{:?}", SmartDate::Relative(-1, SmartInterval::Day)),
            "SmartRelative (-1) Day"
        );
        assert_eq!(
            format!("{:?}", SmartDate::FromReference(Some(-3), 5)),
            "SmartFromReference (Just (-3)) 5"
        );
        assert_eq!(
            format!("{:?}", SmartDate::RelativeMonth(Ordering::Less, 3)),
            "SmartRelativeMonth LT 3"
        );
        assert_eq!(
            format!("{:?}", Interval::NthWeekdayOfMonth(2, 4)),
            "NthWeekdayOfMonth 2 4"
        );
        assert_eq!(
            format!("{:?}", Interval::DaysOfWeek(vec![1, 2])),
            "DaysOfWeek [1,2]"
        );
        let r = day(2023, 3, 15);
        assert_eq!(
            fix_smart_date(r, &SmartDate::FromReference(None, 31)),
            EfDay::exact(day(2023, 3, 31))
        );
        let r = day(2023, 2, 15);
        assert_eq!(
            fix_smart_date(r, &SmartDate::FromReference(None, 31)),
            EfDay::exact(day(2023, 2, 28))
        );
        assert_eq!(
            fix_smart_date(r, &SmartDate::FromReference(Some(2), 29)),
            EfDay::exact(day(2023, 2, 28))
        );
        assert_eq!(
            DateSpan(ex(2024, 1, 2), ex(2024, 2, 1)).show_full(),
            "2024-01-02..2024-01-31"
        );
        assert_eq!(DateSpan(None, ex(2024, 2, 1)).show_full(), "..2024-01-31");
        assert_eq!(Period::MonthPeriod(2024, 2).text_width(), 7);
        assert_eq!(
            Period::PeriodBetween(day(2024, 1, 1), day(2024, 2, 1)).text_width(),
            7
        );
    }

    // Moving, growing and shrinking periods, and the other span helpers.
    #[test]
    fn period_navigation() {
        use Period::*;
        let today = day(2024, 5, 15);
        assert_eq!(MonthPeriod(2024, 12).next(), MonthPeriod(2025, 1));
        assert_eq!(QuarterPeriod(2024, 1).previous(), QuarterPeriod(2023, 4));
        assert_eq!(PeriodAll.next(), PeriodAll);
        let in2024 = DateSpan(ex(2024, 1, 1), ex(2025, 1, 1));
        assert_eq!(MonthPeriod(2024, 12).next_in(in2024), MonthPeriod(2024, 12));
        assert_eq!(MonthPeriod(2024, 11).next_in(in2024), MonthPeriod(2024, 12));
        assert_eq!(
            MonthPeriod(2024, 1).previous_in(in2024),
            MonthPeriod(2024, 1)
        );
        assert_eq!(YearPeriod(2020).move_to(today), YearPeriod(2024));
        assert_eq!(
            WeekPeriod(day(2020, 1, 6)).move_to(today),
            WeekPeriod(day(2024, 5, 13))
        );
        assert_eq!(DayPeriod(today).grow(), WeekPeriod(day(2024, 5, 13)));
        assert_eq!(WeekPeriod(day(2024, 4, 29)).grow(), MonthPeriod(2024, 5));
        assert_eq!(MonthPeriod(2024, 5).grow(), QuarterPeriod(2024, 2));
        assert_eq!(YearPeriod(2024).grow(), PeriodAll);
        assert_eq!(PeriodAll.shrink(today), YearPeriod(2024));
        assert_eq!(YearPeriod(2024).shrink(today), QuarterPeriod(2024, 2));
        assert_eq!(YearPeriod(2023).shrink(today), QuarterPeriod(2023, 1));
        assert_eq!(QuarterPeriod(2024, 2).shrink(today), MonthPeriod(2024, 5));
        assert_eq!(QuarterPeriod(2023, 2).shrink(today), MonthPeriod(2023, 5));
        assert_eq!(
            MonthPeriod(2024, 5).shrink(today),
            WeekPeriod(day(2024, 5, 13))
        );
        assert_eq!(
            MonthPeriod(2024, 6).shrink(today),
            WeekPeriod(day(2024, 6, 3))
        );
        assert_eq!(WeekPeriod(day(2024, 5, 13)).shrink(today), DayPeriod(today));
        assert_eq!(
            WeekPeriod(day(2024, 4, 29)).shrink(today),
            DayPeriod(day(2024, 5, 1))
        );
        assert!(MonthPeriod(2024, 5).contains(today));
        assert!(PeriodBetween(day(2024, 4, 1), day(2024, 7, 1)).is_standard());
        assert!(!PeriodFrom(today).is_standard());
        let spans = [
            DateSpan(ex(2024, 1, 1), ex(2024, 2, 1)),
            DateSpan(ex(2024, 2, 1), ex(2024, 3, 1)),
        ];
        let find = latest_span_containing(&spans);
        assert_eq!(find(day(2024, 2, 10)), Some(spans[1]));
        assert_eq!(find(day(2024, 3, 1)), None);
        assert_eq!(spans_span(&spans), DateSpan(ex(2024, 1, 1), ex(2024, 3, 1)));
        assert_eq!(
            days_span([day(2024, 3, 5), day(2024, 1, 2)]),
            DateSpan(ex(2024, 1, 2), ex(2024, 3, 6))
        );
        let groups = group_by_date_span(
            true,
            |d: &Day| *d,
            &spans,
            [day(2024, 2, 5), day(2023, 1, 1), day(2024, 1, 9)],
        );
        assert_eq!(
            groups,
            [
                (spans[0], vec![day(2024, 1, 9)]),
                (spans[1], vec![day(2024, 2, 5)])
            ]
        );
        assert!(empty_date_span().is_empty());
        assert_eq!(
            interval_boundary_before(&Interval::Months(1), today).unwrap(),
            day(2024, 5, 1)
        );
        assert_eq!(
            interval_boundary_before(&Interval::Days(1), today).unwrap(),
            today
        );
        assert_eq!(
            split_span(
                true,
                &Interval::Days(0),
                DateSpan(ex(2024, 1, 1), ex(2024, 1, 5))
            )
            .unwrap()
            .map(|p| p.start_end()),
            Some((day(2024, 1, 1), day(2024, 1, 1)))
        );
    }

    // The doctests of Period.hs.
    #[cfg_attr(test, test)]
    pub(crate) fn periods() {
        assert_eq!(
            Period::MonthPeriod(2000, 1).as_date_span(),
            DateSpan(fl(2000, 1, 1), fl(2000, 2, 1))
        );
        assert_eq!(
            DateSpan(ex(2000, 1, 1), ex(2000, 2, 1)).as_period(),
            Period::MonthPeriod(2000, 1)
        );
        let s = |b: Day, e: Day| format!("{:?}", Period::PeriodBetween(b, e).simplify());
        assert_eq!(s(day(1, 1, 1), day(2, 1, 1)), "YearPeriod 1");
        assert_eq!(s(day(2000, 10, 1), day(2001, 1, 1)), "QuarterPeriod 2000 4");
        assert_eq!(s(day(2000, 2, 1), day(2000, 3, 1)), "MonthPeriod 2000 2");
        assert_eq!(
            s(day(2016, 7, 25), day(2016, 8, 1)),
            "WeekPeriod 2016-07-25"
        );
        assert_eq!(s(day(2000, 1, 1), day(2000, 1, 2)), "DayPeriod 2000-01-01");
        assert_eq!(
            s(day(2000, 2, 28), day(2000, 3, 1)),
            "PeriodBetween 2000-02-28 2000-03-01"
        );
        assert_eq!(s(day(2000, 2, 29), day(2000, 3, 1)), "DayPeriod 2000-02-29");
        assert_eq!(
            s(day(2000, 12, 31), day(2001, 1, 1)),
            "DayPeriod 2000-12-31"
        );
        assert_eq!(Period::WeekPeriod(day(2016, 7, 25)).to_string(), "2016-W30");
        assert_eq!(
            Period::WeekPeriod(day(2024, 12, 30)).to_string(),
            "2025-W01"
        );
        assert_eq!(Period::WeekPeriod(day(2016, 7, 25)).show_abbrev(), "W30");
        assert_eq!(Period::WeekPeriod(day(2024, 12, 30)).show_abbrev(), "W01");
    }

    // The doctest of boundariesToDayPartition.
    #[cfg_attr(test, test)]
    pub(crate) fn day_partitions() {
        let p = DayPartition::from_boundaries([day(2025, 1, 1), day(2025, 2, 1)]).unwrap();
        assert_eq!(
            p.to_period_data(),
            PeriodData::from_list(day(2024, 12, 31), [(day(2025, 1, 1), day(2025, 1, 31))])
        );
        assert_eq!(p.find(day(2024, 6, 1)), (None, day(2024, 12, 31)));
        assert_eq!(
            p.periods().collect::<Vec<_>>(),
            [(day(2025, 1, 1), day(2025, 1, 31))]
        );
        assert_eq!(
            p.find(day(2025, 1, 15)),
            (Some(day(2025, 1, 1)), day(2025, 1, 31))
        );
        assert_eq!(
            p.find(day(2026, 1, 15)),
            (Some(day(2025, 1, 1)), day(2025, 1, 31))
        );
        let one = DayPartition::from_boundaries([day(2025, 1, 1)]).unwrap();
        assert_eq!(one.start_end(), (day(2025, 1, 1), day(2025, 1, 1)));
        assert_eq!(DayPartition::from_boundaries([]), None);
        let q = DayPartition::from_boundaries([day(2025, 2, 1), day(2025, 3, 1)]).unwrap();
        let pq = p.union(&q).unwrap();
        assert_eq!(pq.start_end(), (day(2025, 1, 1), day(2025, 2, 28)));
        assert_eq!(p.union(&p), Some(p.clone()));
        let gap = DayPartition::from_boundaries([day(2025, 3, 1), day(2025, 4, 1)]).unwrap();
        assert_eq!(p.union(&gap), None);
        let other = DayPartition::from_boundaries([day(2025, 1, 1), day(2025, 1, 15)]).unwrap();
        assert_eq!(p.union(&other), None);
    }

    /// The periods of a split, as `fmap dayPartitionToNonEmpty . splitSpan`.
    fn split(
        adjust: bool,
        i: Interval,
        b: Option<EfDay>,
        e: Option<EfDay>,
    ) -> Option<Vec<(Day, Day)>> {
        split_span(adjust, &i, DateSpan(b, e))
            .unwrap()
            .map(|p| p.periods().collect())
    }

    // A day of month or month that does not exist is an error when a span
    // is split, as upstream's, but not for all time, which is never split.
    #[test]
    fn invalid_interval_days() {
        let year = DateSpan(
            Day::from_ymd(2024, 1, 1).map(EfDay::flex),
            Day::from_ymd(2024, 12, 31).map(EfDay::flex),
        );
        let err = "nextnthdayofmonth: day should be 1..31, not 32";
        assert_eq!(
            split_span(true, &Interval::MonthDay(32), year).unwrap_err(),
            err
        );
        assert_eq!(
            split_span_start_end(true, &Interval::MonthDay(32), year).unwrap_err(),
            err
        );
        assert!(split_span_to_date_spans(true, &Interval::MonthDay(32), year).is_err());
        assert_eq!(
            split_span(true, &Interval::MonthDay(32), DateSpan::default()),
            Ok(None)
        );
        assert_eq!(
            split_span(true, &Interval::MonthAndDay(13, 1), year),
            Err("nextmonthandday: month should be 1..12, not 13".to_string())
        );
        assert_eq!(
            split_span(true, &Interval::MonthAndDay(12, 0), year),
            Err("nextmonthandday: day should be 1..31, not 0".to_string())
        );
    }

    // The doctests of splitSpan and splitSpanToDateSpans.
    #[cfg_attr(test, test)]
    pub(crate) fn splitting_spans() {
        use Interval::*;
        let t = |i, b, e| split(true, i, b, e);
        let d = |y, m, dd| day(y, m, dd);
        assert_eq!(
            t(NoInterval, fl(2008, 1, 1), fl(2009, 1, 1)),
            Some(vec![(d(2008, 1, 1), d(2008, 12, 31))])
        );
        assert_eq!(
            t(Quarters(1), fl(2008, 1, 1), fl(2009, 1, 1)),
            Some(vec![
                (d(2008, 1, 1), d(2008, 3, 31)),
                (d(2008, 4, 1), d(2008, 6, 30)),
                (d(2008, 7, 1), d(2008, 9, 30)),
                (d(2008, 10, 1), d(2008, 12, 31))
            ])
        );
        assert_eq!(
            split_span(true, &Quarters(1), DateSpan::default()).unwrap(),
            None
        );
        assert_eq!(t(Days(1), fl(2008, 1, 1), fl(2008, 1, 1)), None);
        assert_eq!(t(Quarters(1), fl(2008, 1, 1), fl(2008, 1, 1)), None);
        assert_eq!(
            t(Months(1), fl(2008, 1, 1), fl(2008, 4, 1)),
            Some(vec![
                (d(2008, 1, 1), d(2008, 1, 31)),
                (d(2008, 2, 1), d(2008, 2, 29)),
                (d(2008, 3, 1), d(2008, 3, 31))
            ])
        );
        assert_eq!(
            t(Months(2), fl(2008, 1, 1), fl(2008, 4, 1)),
            Some(vec![
                (d(2008, 1, 1), d(2008, 2, 29)),
                (d(2008, 3, 1), d(2008, 4, 30))
            ])
        );
        assert_eq!(
            t(Weeks(1), fl(2008, 1, 1), fl(2008, 1, 15)),
            Some(vec![
                (d(2007, 12, 31), d(2008, 1, 6)),
                (d(2008, 1, 7), d(2008, 1, 13)),
                (d(2008, 1, 14), d(2008, 1, 20))
            ])
        );
        assert_eq!(
            t(Weeks(2), fl(2008, 1, 1), fl(2008, 1, 15)),
            Some(vec![
                (d(2007, 12, 31), d(2008, 1, 13)),
                (d(2008, 1, 14), d(2008, 1, 27))
            ])
        );
        assert_eq!(
            t(MonthDay(2), fl(2008, 1, 1), fl(2008, 4, 1)),
            Some(vec![
                (d(2008, 1, 2), d(2008, 2, 1)),
                (d(2008, 2, 2), d(2008, 3, 1)),
                (d(2008, 3, 2), d(2008, 4, 1))
            ])
        );
        assert_eq!(
            t(NthWeekdayOfMonth(2, 4), fl(2011, 1, 1), fl(2011, 2, 15)),
            Some(vec![
                (d(2010, 12, 9), d(2011, 1, 12)),
                (d(2011, 1, 13), d(2011, 2, 9)),
                (d(2011, 2, 10), d(2011, 3, 9))
            ])
        );
        assert_eq!(
            t(DaysOfWeek(vec![2]), fl(2011, 1, 1), fl(2011, 1, 15)),
            Some(vec![
                (d(2010, 12, 28), d(2011, 1, 3)),
                (d(2011, 1, 4), d(2011, 1, 10)),
                (d(2011, 1, 11), d(2011, 1, 17))
            ])
        );
        assert_eq!(
            t(MonthAndDay(11, 29), fl(2012, 10, 1), fl(2013, 10, 15)),
            Some(vec![(d(2012, 11, 29), d(2013, 11, 28))])
        );
        let spans = |i, b, e| {
            split_span_to_date_spans(true, &i, DateSpan(b, e))
                .unwrap()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            spans(Months(1), fl(2008, 1, 1), fl(2008, 4, 1)),
            ["DateSpan 2008-01", "DateSpan 2008-02", "DateSpan 2008-03"]
        );
        assert!(spans(Days(1), fl(2008, 1, 1), fl(2008, 1, 1)).is_empty());
    }

    // tests_DayPartition "weekday".
    #[cfg_attr(test, test)]
    pub(crate) fn splitting_by_weekdays() {
        let d = |m, dd| day(2021, m, dd);
        assert_eq!(
            split(
                false,
                Interval::DaysOfWeek(vec![1, 2, 3, 4, 5]),
                ex(2021, 7, 1),
                ex(2021, 7, 8)
            ),
            Some(vec![
                (d(6, 28), d(6, 28)),
                (d(6, 29), d(6, 29)),
                (d(6, 30), d(6, 30)),
                (d(7, 1), d(7, 1)),
                (d(7, 2), d(7, 4)),
                (d(7, 5), d(7, 5)),
                (d(7, 6), d(7, 6)),
                (d(7, 7), d(7, 7))
            ])
        );
        assert_eq!(
            split(
                false,
                Interval::DaysOfWeek(vec![1, 5]),
                ex(2021, 7, 1),
                ex(2021, 7, 8)
            ),
            Some(vec![
                (d(6, 28), d(7, 1)),
                (d(7, 2), d(7, 4)),
                (d(7, 5), d(7, 8))
            ])
        );
    }

    // tests_DayPartition "match dayOfWeek".
    #[cfg_attr(test, test)]
    pub(crate) fn splitting_by_one_weekday() {
        let dayofweek = |n: i64, ds| {
            let next = move |w: i64, d: Day| {
                if w == 0 {
                    d
                } else {
                    (1..n).fold((0..w).fold(d, |d, _| nextweek(d)), |d, _| nextday(d))
                }
            };
            let (e, bs) = boundaries(ds, |d| nthdayofweekcontaining(n, d), 1, next)?;
            spans_from_boundaries(e, bs)
        };
        let (ys2021, ye2021, ys2022) = (day(2021, 1, 1), day(2021, 12, 31), day(2022, 1, 1));
        for ds in [
            DateSpan::exact(Some(ys2021), Some(ye2021)),
            DateSpan::exact(Some(ys2021), Some(ys2022)),
            DateSpan::exact(Some(ye2021), Some(ys2022)),
            DateSpan::exact(Some(ye2021), None),
            DateSpan::exact(Some(ys2022), None),
            DateSpan::exact(None, Some(ye2021)),
            DateSpan::exact(None, Some(ys2022)),
        ] {
            for n in 1..=7 {
                assert_eq!(
                    split_span(false, &Interval::DaysOfWeek(vec![n]), ds).unwrap(),
                    dayofweek(n, ds),
                    "{ds:?} {n}"
                );
            }
        }
    }

    // tests_DayPartition "splitSpanToDateSpans and splitSpanStartEnd agree
    // with splitSpan".
    #[cfg_attr(test, test)]
    pub(crate) fn split_forms_agree() {
        use Interval::*;
        let intervals = [
            NoInterval,
            Days(1),
            Days(3),
            Weeks(1),
            Weeks(2),
            Months(1),
            Months(5),
            Quarters(1),
            Years(1),
            NthWeekdayOfMonth(2, 4),
            NthWeekdayOfMonth(5, 7),
            MonthDay(2),
            MonthDay(31),
            MonthAndDay(11, 29),
            DaysOfWeek(vec![1, 2, 3, 4, 5]),
            DaysOfWeek(vec![5, 1]),
            DaysOfWeek(vec![3, 3, 7]),
        ];
        let (d1, d2) = (day(2021, 1, 1), day(2024, 3, 15));
        let spans = [
            DateSpan::exact(Some(d1), Some(d2)),
            DateSpan::flex(Some(d1), Some(d2)),
            DateSpan::exact(Some(d2), Some(nextday(d2))),
            DateSpan::exact(Some(d1), None),
            DateSpan::exact(None, Some(d2)),
            DateSpan::exact(Some(d2), Some(d1)),
            DateSpan::default(),
        ];
        for adjust in [false, true] {
            for i in &intervals {
                for ds in spans {
                    let partition = split_span(adjust, i, ds).unwrap();
                    assert_eq!(
                        split_span_to_date_spans(adjust, i, ds)
                            .unwrap()
                            .collect::<Vec<_>>(),
                        partition
                            .as_ref()
                            .map_or(vec![], DayPartition::to_date_spans),
                        "{adjust} {i:?} {ds:?}"
                    );
                    assert_eq!(
                        split_span_start_end(adjust, i, ds).unwrap(),
                        partition.map(|p| {
                            let (s, e) = p.start_end();
                            (s, nextday(e))
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn dates_read_with_strptime_formats() {
        let d = |y, m, dd| Some(Day::from_ymd(y, m, dd).unwrap());
        let parse = |fmt: Option<&str>, s: &str| parse_date_with_format(false, None, 0, fmt, s);
        assert_eq!(parse(Some("%d/%Y/%m"), "10/2009/09"), d(2009, 9, 10));
        assert_eq!(parse(None, "2009/9/10"), d(2009, 9, 10));
        assert_eq!(parse(None, "2009-09-10"), d(2009, 9, 10));
        assert_eq!(parse(None, "2009.9.10"), d(2009, 9, 10));
        assert_eq!(parse(None, "10/9/2009"), None);
        assert_eq!(parse(Some("%m/%d/%Y"), "12/22/2018"), d(2018, 12, 22));
        assert_eq!(parse(Some("%-d/%-m/%Y"), "1/2/2023"), d(2023, 2, 1));
        assert_eq!(parse(Some("%Y%m%d"), "20230102"), d(2023, 1, 2));
        assert_eq!(parse(Some("%Y-%m-%d"), " 2023-01-02 "), d(2023, 1, 2));
        assert_eq!(parse(Some("%d %b %Y"), "02 jan 2023"), d(2023, 1, 2));
        assert_eq!(parse(Some("%Y %j"), "2023 032"), d(2023, 2, 1));
        assert_eq!(parse(Some("%G-W%V-%u"), "2023-W01-1"), d(2023, 1, 2));
        assert_eq!(parse(Some("%y-%m-%d"), "69-01-02"), d(1969, 1, 2));
        assert_eq!(parse(Some("%y-%m-%d"), "68-01-02"), d(2068, 1, 2));
        // invalid dates, and formats matching in more than one way, fail
        assert_eq!(parse(Some("%Y-%m-%d"), "2023-02-30"), None);
        assert_eq!(parse(Some("%Y%-m%-d"), "2023111"), None);
        // times in another zone than the local one (here UTC) can change the date
        let zoned = parse_date_with_format(
            true,
            None,
            0,
            Some("%Y-%m-%d %H:%M %z"),
            "2023-01-02 00:30 +0100",
        );
        assert_eq!(zoned, d(2023, 1, 1));
        assert_eq!(
            parse_date_with_format(false, Some(60), 0, None, "2023-01-02"),
            d(2023, 1, 1)
        );
        assert_eq!(
            parse_date_with_format(false, Some(-300), 0, None, "2023-01-02"),
            d(2023, 1, 2)
        );
        assert_eq!(parse_time_zone("EST"), Some(-300));
        assert_eq!(parse_time_zone("+05:30"), Some(330));
        assert_eq!(parse_time_zone("Z"), Some(0));
        assert_eq!(parse_time_zone("UTC"), Some(0));
        assert_eq!(parse_time_zone("J"), None);
        assert_eq!(parse_time_zone(" EST"), None);
    }
}
