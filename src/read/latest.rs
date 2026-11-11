//! The `.latest` mechanism (Hledger.Read.LatestDates): for an input file
//! `F`, the latest transaction date(s) seen when it was last read with
//! `--new` (or imported), kept in `.latest.F` beside it, so that the next
//! read keeps only the transactions it has not seen.

use crate::dates::{Day, parsedate};
use crate::model::Journal;

/// An input file and its latest dates (LatestDatesForFile).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestDatesForFile {
    pub file: String,
    pub dates: Vec<Day>,
}

/// Every instance of the latest of these dates: one if it appears once,
/// three if it appears three times (latestDates).
pub fn latest_dates(dates: impl IntoIterator<Item = Day>) -> Vec<Day> {
    let dates: Vec<Day> = dates.into_iter().collect();
    match dates.iter().max() {
        Some(&max) => dates.into_iter().filter(|&d| d == max).collect(),
        None => Vec::new(),
    }
}

/// Where a file's latest dates are kept: `.latest.FILE` in its directory,
/// which for a file named without one is `./` (latestDatesFileFor).
pub fn latest_dates_file_for(f: &str) -> String {
    match f.rfind('/') {
        Some(i) => format!("{}.latest.{}", &f[..=i], &f[i + 1..]),
        None => format!("./.latest.{f}"),
    }
}

/// Save a file's latest dates in its `.latest` file, one per line, unless
/// there are none (saveLatestDates).
pub fn save_latest_dates(dates: &[Day], f: &str) -> Result<(), String> {
    if dates.is_empty() {
        return Ok(());
    }
    let path = latest_dates_file_for(f);
    let text: String = dates.iter().map(|d| format!("{d}\n")).collect();
    std::fs::write(&path, text)
        .map_err(|e| format!("{path}: withFile: {}", super::io_write_error_text(&e)))
}

/// Save each file's latest dates (saveLatestDatesForFiles).
pub fn save_latest_dates_for_files(files: &[LatestDatesForFile]) -> Result<(), String> {
    files
        .iter()
        .try_for_each(|l| save_latest_dates(&l.dates, &l.file))
}

/// The latest dates seen the last time this file was read, from its
/// `.latest` file if it has one; blank lines are ignored, any other line
/// must be a date (previousLatestDates).
pub fn previous_latest_dates(f: &str) -> Result<Vec<Day>, String> {
    let path = latest_dates_file_for(f);
    if !super::file_exists(&path) {
        return Ok(Vec::new());
    }
    let text = super::read_file_portably(&path)?;
    let mut dates = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let s = line.trim_matches(crate::text::is_space);
        if s.is_empty() {
            continue;
        }
        match parsedate(s) {
            Some(d) => dates.push(d),
            None => return Err(format!("{path}:{}: invalid date: \"{s}\"", n + 1)),
        }
    }
    Ok(dates)
}

/// The journal without the transactions seen last time, given that
/// time's latest dates: those dated before them, and as many on their
/// date as there were then; its transactions are sorted by date. Also
/// the latest dates of the transactions on or after that date
/// (journalFilterSinceLatestDates).
pub fn journal_filter_since_latest_dates(seen: &[Day], mut j: Journal) -> (Journal, Vec<Day>) {
    let Some(&d) = seen.first() else {
        let dates = latest_dates(j.txns.iter().map(|t| t.date));
        return (j, dates);
    };
    let mut txns: Vec<_> = std::mem::take(&mut j.txns)
        .into_iter()
        .filter(|t| t.date >= d)
        .collect();
    txns.sort_by_key(|t| t.date);
    let dates = latest_dates(txns.iter().map(|t| t.date));
    let same_date = txns.iter().take_while(|t| t.date == d).count();
    j.txns = txns.into_iter().skip(same_date.min(seen.len())).collect();
    (j, dates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Transaction;

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn journal(dates: &[Day]) -> Journal {
        Journal {
            txns: dates
                .iter()
                .enumerate()
                .map(|(i, &date)| Transaction {
                    date,
                    description: i.to_string(),
                    ..Transaction::default()
                })
                .collect(),
            ..Journal::default()
        }
    }

    #[test]
    fn latest_dates_are_every_instance_of_the_latest() {
        let (a, b) = (day(2020, 1, 1), day(2020, 1, 2));
        assert_eq!(latest_dates([b, a, b, a]), [b, b]);
        assert_eq!(latest_dates([]), Vec::<Day>::new());
    }

    #[test]
    fn latest_dates_files() {
        assert_eq!(latest_dates_file_for("a.csv"), "./.latest.a.csv");
        assert_eq!(latest_dates_file_for("/x/y/a.csv"), "/x/y/.latest.a.csv");
        assert_eq!(
            latest_dates_file_for("rules/b.rules"),
            "rules/.latest.b.rules"
        );
    }

    #[test]
    fn filtering_since_the_latest_dates() {
        let (d1, d2, d3) = (day(2020, 1, 1), day(2020, 1, 2), day(2020, 1, 3));
        // none seen: everything is new
        let (j, ds) = journal_filter_since_latest_dates(&[], journal(&[d2, d1, d2]));
        assert_eq!(j.txns.len(), 3);
        assert_eq!(ds, [d2, d2]);
        // two seen on d2: the earlier date and two of the three on d2 are dropped
        let (j, ds) = journal_filter_since_latest_dates(&[d2, d2], journal(&[d3, d2, d1, d2, d2]));
        let kept: Vec<(Day, &str)> = j.txns.iter().map(|t| (t.date, &*t.description)).collect();
        assert_eq!(kept, [(d2, "4"), (d3, "0")]);
        assert_eq!(ds, [d3]);
        // a backfilled transaction before the latest date is lost
        let (j, ds) = journal_filter_since_latest_dates(&[d2], journal(&[d1, d2]));
        assert!(j.txns.is_empty());
        assert_eq!(ds, [d2]);
    }
}
