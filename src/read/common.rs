//! Parsers shared by the journal reader and the others (the lexical parsers of
//! Hledger.Read.Common): dates.
//!
//! These work on a [`Cursor`]. One that fails without consuming input leaves
//! the cursor where it started, so its caller can treat the part as absent;
//! errors are reported as megaparsec reports them where hledger's tests look.

use crate::dates::Day;
use crate::parse::{Cursor, Item, PResult, ParseError};

/// Whether a character can separate a date's parts (isDateSepChar).
pub fn is_date_sep_char(c: char) -> bool {
    matches!(c, '-' | '/' | '.')
}

/// A run of digits as a number, saturating rather than overflowing.
pub fn read_digits(s: &str) -> i64 {
    s.bytes().fold(0i64, |n, b| {
        n.saturating_mul(10).saturating_add(i64::from(b - b'0'))
    })
}

/// A date: YYYY-MM-DD, with `/` or `.` also allowed as separators and leading
/// zeros optional; or without the year, MM-DD, if a default year is known
/// (datep').
pub fn datep(c: &mut Cursor, year: Option<i64>) -> PResult<Day> {
    let start = c.pos;
    let d1 = c.take_while(|ch| ch.is_ascii_digit());
    if d1.is_empty() {
        return Err(c.unexpected(&[Item::label("full or partial date")]));
    }
    let Some(sep) = c.peek().filter(|&ch| is_date_sep_char(ch)) else {
        return Err(c.unexpected(&[Item::label("date separator"), Item::label("digit")]));
    };
    c.bump();
    let d2 = c.take_while(|ch| ch.is_ascii_digit());
    if d2.is_empty() {
        return Err(c.unexpected(&[Item::label("month or day")]));
    }
    let invalid = |c: &Cursor| {
        ParseError::at_region(
            c.src,
            start,
            c.pos,
            "This date has different separators, please use consistent separators.",
        )
    };
    let not_a_date = |c: &Cursor| {
        ParseError::at_region(
            c.src,
            start,
            c.pos,
            "This is not a valid date, please fix it.",
        )
    };
    if d1.len() >= 4 {
        let Some(sep2) = c.peek().filter(|&ch| is_date_sep_char(ch)) else {
            return Err(c.unexpected(&[Item::label("date separator"), Item::label("digit")]));
        };
        c.bump();
        let d3 = c.take_while(|ch| ch.is_ascii_digit());
        if d3.is_empty() {
            return Err(c.unexpected(&[Item::label("day")]));
        }
        if sep != sep2 {
            return Err(invalid(c));
        }
        valid_date(read_digits(d1), read_digits(d2), read_digits(d3)).ok_or_else(|| not_a_date(c))
    } else {
        match year {
            Some(y) => valid_date(y, read_digits(d1), read_digits(d2)).ok_or_else(|| not_a_date(c)),
            None => Err(ParseError::at_region(
                c.src,
                start,
                c.pos,
                "This partial date can not be parsed because the current year is unknown.\n\
                 Please make it a full date, or add a default year directive.",
            )),
        }
    }
}

/// The date with this year, month and day, if there is one (fromGregorianValid).
fn valid_date(y: i64, m: i64, d: i64) -> Option<Day> {
    Day::from_ymd(y, u32::try_from(m).ok()?, u32::try_from(d).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str, year: Option<i64>) -> Result<Day, String> {
        let mut c = Cursor::new(s);
        datep(&mut c, year).map_err(|e| e.pretty("", s))
    }

    // tests_JournalReader "datep".
    #[test]
    fn dates() {
        assert_eq!(
            date("2018/01/01", None),
            Ok(Day::from_ymd(2018, 1, 1).unwrap())
        );
        assert!(date("2018-01-01", None).is_ok());
        assert!(date("2018.01.01", None).is_ok());
        assert!(
            date("1/1", None)
                .unwrap_err()
                .contains("current year is unknown")
        );
        assert_eq!(
            date("1/1", Some(2018)),
            Ok(Day::from_ymd(2018, 1, 1).unwrap())
        );
        assert!(date("2018/1/1", None).is_ok());
        assert!(
            date("2018/1-1", None)
                .unwrap_err()
                .contains("different separators")
        );
        assert!(
            date("2022/1/32", None)
                .unwrap_err()
                .contains("not a valid date")
        );
        assert_eq!(
            date("2018\n", None).unwrap_err(),
            "1:5:\n  |\n1 | 2018\n  |     ^\nunexpected newline\nexpecting date separator or digit\n"
        );
    }
}
