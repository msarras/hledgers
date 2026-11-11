//! The parsers of smart dates and period expressions (Hledger.Data.Dates),
//! and the month and weekday names they accept.

use super::*;

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
pub(super) fn double_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
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
pub(super) fn from_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
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
pub(super) fn to_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
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
pub(super) fn in_date_span(p: &mut Parser, rdate: Day) -> PResult<DateSpan> {
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
pub(super) fn quarterp(p: &mut Parser, rdate: Day) -> PResult<(i64, u32)> {
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
