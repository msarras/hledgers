//! Dates read with strptime-style formats (Data.Time.Format.Parse's
//! parseTimeM with defaultTimeLocale), for CSV rules' date-format and
//! timezone. A format is matched in every way it can be, as ReadP does; it
//! must match in exactly one.

use super::*;

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
