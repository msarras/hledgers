//! The timeclock reader (Hledger.Read.TimeclockReader), and the conversion of
//! its clock-in and clock-out entries to transactions (Hledger.Data.Timeclock).
//!
//! A clock-in (`i DATE TIME ACCOUNT[  DESCRIPTION][;COMMENT]`) and the
//! clock-out that closes it (`o DATE TIME [ACCOUNT][;COMMENT]`) make a session,
//! which becomes a transaction with one virtual posting of the hours between
//! them. A clock-out closes the session clocked in to its account, or without
//! one the latest session; sessions are split at midnight, and those still
//! open end now. `b`, `h` and `O` entries are read and ignored. With
//! `--old-timeclock`, entries must alternate between `i` and `o`, the
//! account is optional, and the clock-out's text is ignored.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use super::InputOpts;
use super::common::{
    datep, empty_or_comment_linep, noncomment_textp, source_pos, timeofdayp, transaction_commentp,
};
use super::journal::{Failure, ParseState, R, modified_account_namep};
use crate::account::Realness;
use crate::amount::{MixedAmount, hrs};
use crate::dates::Day;
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, SourcePos, Status, Tag, Transaction};
use crate::parse::{Cursor, Item};
use crate::text::is_space;

/// A date and a time of day, in nanoseconds since midnight (LocalTime).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime {
    pub day: Day,
    pub nanos: u64,
}

const NANOS_PER_DAY: u64 = 86_400_000_000_000;

impl LocalTime {
    /// The current time in the local time zone (getCurrentLocalTime).
    pub fn now() -> LocalTime {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = since.as_secs() as i64;
        let local = i128::from(secs + crate::dates::local_utc_offset_seconds(secs)) * 1_000_000_000
            + i128::from(since.subsec_nanos());
        let day_nanos = NANOS_PER_DAY as i128;
        LocalTime {
            day: Day::from_ymd(1970, 1, 1)
                .expect("a valid date")
                .add_days(local.div_euclid(day_nanos) as i64),
            nanos: local.rem_euclid(day_nanos) as u64,
        }
    }

    /// The start of the next day.
    fn next_midnight(self) -> LocalTime {
        LocalTime {
            day: self.day.add_days(1),
            nanos: 0,
        }
    }

    /// The nanoseconds from `earlier` to this time, counting both as UTC.
    fn nanos_since(self, earlier: LocalTime) -> i128 {
        i128::from(self.day.diff_days(earlier.day)) * NANOS_PER_DAY as i128 + i128::from(self.nanos)
            - i128::from(earlier.nanos)
    }
}

/// As Haskell shows it: `2020-01-02 08:30:00`, with any fraction of a
/// second after the seconds.
impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.nanos / 1_000_000_000;
        let frac = self.nanos % 1_000_000_000;
        write!(
            f,
            "{} {:02}:{:02}:{:02}",
            self.day,
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        )?;
        if frac > 0 {
            write!(f, ".{}", format!("{frac:09}").trim_end_matches('0'))?;
        }
        Ok(())
    }
}

/// What a timeclock entry does, in Haskell's order (TimeclockCode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Code {
    SetBalance,
    SetRequiredHours,
    In,
    Out,
    FinalOut,
}

impl Code {
    fn from_char(c: char) -> Option<Code> {
        Some(match c {
            'b' => Code::SetBalance,
            'h' => Code::SetRequiredHours,
            'i' => Code::In,
            'o' => Code::Out,
            'O' => Code::FinalOut,
            _ => return None,
        })
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Code::SetBalance => "b",
            Code::SetRequiredHours => "h",
            Code::In => "i",
            Code::Out => "o",
            Code::FinalOut => "O",
        })
    }
}

/// A timeclock entry, ordered as upstream's derived Ord orders it: by
/// position first (TimeclockEntry).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    pos: SourcePos,
    code: Code,
    time: LocalTime,
    account: String,
    description: String,
    comment: String,
    tags: Vec<Tag>,
}

/// `CODE DATE TIME ACCOUNT  DESCRIPTION`, as error messages show an entry.
impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}  {}",
            self.code, self.time, self.account, self.description
        )
    }
}

/// A timeclock file's text read into a finalised journal, the --alias
/// options applied after the transactions are made (TimeclockReader's parse).
pub fn read_timeclock(iopts: &InputOpts, f: &str, text: &str) -> Result<Journal, String> {
    let st = ParseState {
        year: Some(iopts.today.year()),
        ..ParseState::default()
    };
    let txns = timeclock_transactions(iopts, &st, f, text).map_err(|e| e.message(f, text))?;
    let aliases = super::journal::aliases_from_opts(iopts)?;
    let j = super::csv::journal_apply_aliases(
        &iopts.prog_args,
        &aliases,
        Journal {
            txns,
            ..Journal::default()
        },
    )?;
    super::finalise::journal_finalise(iopts, f, text, j, st)
}

/// A timeclock file's entries, read with this parse state, made into
/// transactions, the sessions still open ending now (timeclockfilep).
pub(super) fn timeclock_transactions(
    iopts: &InputOpts,
    st: &ParseState,
    file: &str,
    text: &str,
) -> R<Vec<Transaction>> {
    let entries = entriesp(&mut Cursor::new(text), st, file, iopts.old_timeclock)?;
    let now = LocalTime::now();
    let txns = if iopts.old_timeclock {
        timeclock_to_transactions_old(now, &entries)
    } else {
        timeclock_to_transactions(now, entries)
    };
    txns.map_err(Failure::Unlocated)
}

/// Entries, comment lines and empty lines, to the end of the input.
fn entriesp(c: &mut Cursor, st: &ParseState, file: &str, old: bool) -> R<Vec<Entry>> {
    let mut entries = Vec::new();
    // what else could have been at the cursor, when an entry ended before
    // its line did: the end of the line, and what could have continued the
    // entry's last part
    let mut hints = Vec::new();
    let file: Arc<str> = Arc::from(file);
    let mut counted = (0, 1);
    loop {
        let start = c.pos;
        match empty_or_comment_linep(c) {
            Ok(()) => {
                hints.clear();
                continue;
            }
            Err(e) if c.pos != start => return Err(e.into()),
            Err(_) => {}
        }
        let Some(code) = c.peek().and_then(Code::from_char) else {
            if c.at_end() {
                return Ok(entries);
            }
            hints.push(Item::EndOfInput);
            hints.push(Item::label("timeclock entry, comment line, or empty line"));
            return Err(c.unexpected(&hints).into());
        };
        let pos = source_pos(c, &file, &mut counted);
        c.bump();
        let entry;
        (entry, hints) = if old {
            (old_entryp(c, st, pos, code)?, Vec::new())
        } else {
            entryp(c, st, pos, code)?
        };
        entries.push(entry);
    }
}

/// One or more spaces, or an error saying what was found and what else
/// could have been there (skipNonNewlineSpaces1).
fn spaces1(c: &mut Cursor, hints: &[Item]) -> R<()> {
    if c.skip_spaces() {
        Ok(())
    } else {
        Err(c.unexpected(hints).into())
    }
}

/// A date, spaces and a time (datetimep); and what could have continued
/// the time: its seconds, if it has none and no time zone.
fn datetimep(c: &mut Cursor, st: &ParseState) -> R<(LocalTime, Vec<Item>)> {
    let day = datep(c, st.year)?;
    spaces1(c, &[Item::label("digit")])?;
    let start = c.pos;
    let secs = timeofdayp(c)?;
    let hints = if c.pos - start == 5 {
        vec![Item::label("':' (minute-second separator)")]
    } else {
        Vec::new()
    };
    let time = LocalTime {
        day,
        nanos: u64::from(secs) * 1_000_000_000,
    };
    Ok((time, hints))
}

/// The rest of an entry after its code (timeclockentryp); and if it ended
/// before its line did, a comment not following, what else could have been
/// there.
fn entryp(c: &mut Cursor, st: &ParseState, pos: SourcePos, code: Code) -> R<(Entry, Vec<Item>)> {
    spaces1(c, &[])?;
    let (time, mut hints) = datetimep(c, st)?;
    let (account, description) = match code {
        Code::In => {
            spaces1(c, &hints)?;
            hints.clear();
            let a = modified_account_namep(c, st, false)?;
            let d = if c.skip_spaces() {
                noncomment_textp(c).trim_matches(is_space).to_string()
            } else {
                String::new()
            };
            (a, d)
        }
        Code::Out => {
            let save = *c;
            let a = if c.skip_spaces() {
                match modified_account_namep(c, st, false) {
                    Ok(a) => Some(a),
                    Err(Failure::Here(_)) => None,
                    Err(e) => return Err(e),
                }
            } else {
                None
            };
            match a {
                Some(a) => {
                    hints.clear();
                    (a, String::new())
                }
                None => {
                    *c = save;
                    (String::new(), String::new())
                }
            }
        }
        _ => (String::new(), String::new()),
    };
    if c.skip_spaces() {
        hints.clear();
    }
    let start = c.pos;
    let (comment, tags) = match transaction_commentp(c, &[]) {
        Ok(ct) => {
            hints.clear();
            ct
        }
        Err(e) if c.pos != start => return Err(e.into()),
        Err(_) => {
            c.pos = start;
            hints.push(Item::label("newline"));
            (String::new(), Vec::new())
        }
    };
    let entry = Entry {
        pos,
        code,
        time,
        account,
        description,
        comment,
        tags,
    };
    Ok((entry, hints))
}

/// The rest of an entry after its code, in the pre-1.50 format, whose
/// account is optional and may contain semicolons (oldtimeclockentryp).
fn old_entryp(c: &mut Cursor, st: &ParseState, pos: SourcePos, code: Code) -> R<Entry> {
    spaces1(c, &[])?;
    let (time, mut hints) = datetimep(c, st)?;
    let after_time = c.pos;
    let account = if c.skip_spaces() {
        modified_account_namep(c, st, true)?
    } else {
        String::new()
    };
    let description = if c.skip_spaces() {
        noncomment_textp(c).trim_matches(is_space).to_string()
    } else {
        String::new()
    };
    if c.pos != after_time {
        hints.clear();
    }
    let (comment, tags) = transaction_commentp(c, &hints)?;
    Ok(Entry {
        pos,
        code,
        time,
        account,
        description,
        comment,
        tags,
    })
}

/// A clock-in and the clock-out that ends it.
struct Session {
    inn: Entry,
    out: Entry,
}

/// Entries made into transactions, a session at a time, in the order of
/// their clock-ins; sessions crossing midnight split into days, and those
/// with no clock-out ending now, or at their start if that is later
/// (timeclockToTransactions). Clock-ins and clock-outs may come in any
/// order: a clock-out closes the session in its account, or without one,
/// the latest. A clock-out with no session to close, and a clock-in to an
/// account with a session already open, are errors.
fn timeclock_to_transactions(
    now: LocalTime,
    entries: Vec<Entry>,
) -> Result<Vec<Transaction>, String> {
    let (mut sessions, open) = pair_clock_entries(entries)?;
    let outs: Vec<Entry> = open
        .iter()
        .map(|e| Entry {
            pos: e.pos.clone(),
            code: Code::Out,
            time: now.max(e.time),
            account: e.account.clone(),
            description: String::new(),
            comment: String::new(),
            tags: Vec::new(),
        })
        .collect();
    let (closed, _) = pair_clock_entries(open.into_iter().chain(outs).collect())?;
    sessions.extend(closed);
    sessions.sort_by(|a, b| a.inn.cmp(&b.inn));
    sessions
        .iter()
        .map(|s| entry_from_timeclock_in_out(false, &s.inn, &s.out))
        .collect()
}

/// The sessions the entries make, and the clock-ins left open, the latest
/// first (pairClockEntries).
fn pair_clock_entries(entries: Vec<Entry>) -> Result<(Vec<Session>, Vec<Entry>), String> {
    let mut queue: VecDeque<Entry> = entries.into();
    let mut actives: Vec<Entry> = Vec::new();
    let mut sessions = Vec::new();
    while let Some(entry) = queue.pop_front() {
        match entry.code {
            Code::In => {
                let same: Vec<&Entry> = actives
                    .iter()
                    .filter(|e| e.account == entry.account)
                    .collect();
                if !same.is_empty() {
                    let lines: Vec<String> = same.iter().map(|e| e.pos.line.to_string()).collect();
                    return Err(timeclock_entry_error(
                        &same,
                        &entry,
                        &format!(
                            "This clockin overlaps the session in the same account which began on line {}.\n\
                             Overlapping sessions with the same account name are not supported.",
                            lines.join(", ")
                        ),
                    ));
                }
                actives.insert(0, entry);
            }
            Code::Out => {
                let (mut matching, rest): (Vec<Entry>, Vec<Entry>) =
                    actives.drain(..).partition(|e| e.account == entry.account);
                let (inn, others) = if !matching.is_empty() {
                    let inn = matching.remove(0);
                    matching.extend(rest);
                    (inn, matching)
                } else if entry.account.is_empty() && !rest.is_empty() {
                    let mut rest = rest;
                    (rest.remove(0), rest)
                } else {
                    return Err(could_not_find_clockin(&entry));
                };
                actives = others;
                if entry.time.day > inn.time.day {
                    let midnight = inn.time.next_midnight();
                    let out = Entry {
                        time: midnight,
                        ..entry.clone()
                    };
                    actives.insert(
                        0,
                        Entry {
                            time: midnight,
                            ..inn.clone()
                        },
                    );
                    sessions.push(Session { inn, out });
                    queue.push_front(entry);
                } else {
                    sessions.push(Session { inn, out: entry });
                }
            }
            _ => {}
        }
    }
    Ok((sessions, actives))
}

/// The error for a clock-out with no session to close.
fn could_not_find_clockin(o: &Entry) -> String {
    let l = o.pos.line.to_string();
    format!(
        "{}:\n{l} | {o}\n{} |{}^\n\nCould not find previous clockin to match this clockout.",
        o.pos,
        " ".repeat(l.len()),
        " ".repeat(o.pos.col)
    )
}

/// An error about an entry in hledger's standard form: its position; an
/// excerpt showing it after any related entries, marked with `^`; and the
/// explanation (timeclockEntryError).
fn timeclock_entry_error(related: &[&Entry], e: &Entry, msg: &str) -> String {
    let mut es: Vec<&Entry> = related.to_vec();
    es.sort_by_key(|x| x.pos.line);
    es.push(e);
    let mut lines = vec![format!("{}:", e.pos)];
    for (i, x) in es.iter().enumerate() {
        if i > 0 && x.pos.line > es[i - 1].pos.line + 1 {
            lines.push(String::new());
        }
        lines.push(format!(
            "{} | {}",
            x.pos.line,
            x.to_string().trim_end_matches(is_space)
        ));
    }
    lines.push(format!(
        "{} |{}^",
        " ".repeat(e.pos.line.to_string().len()),
        " ".repeat(e.pos.col)
    ));
    lines.push(String::new());
    lines.push(msg.to_string());
    lines.join("\n")
}

/// Entries made into transactions as hledger before 1.43 made them, from
/// strictly alternating clock-ins and clock-outs, beginning with a
/// clock-in; a last clock-in ends now, or at its start if that is later
/// (timeclockToTransactionsOld). Every alternation error comes before any
/// error in a session, as upstream reverses the list before using it.
fn timeclock_to_transactions_old(
    now: LocalTime,
    entries: &[Entry],
) -> Result<Vec<Transaction>, String> {
    let mut sessions = Vec::new();
    let mut rest: VecDeque<Entry> = entries.iter().cloned().collect();
    while let Some(inn) = rest.pop_front() {
        if inn.code != Code::In {
            return Err(error_expected_code_but_got(Code::In, &inn));
        }
        let out = match rest.pop_front() {
            Some(o) if o.code != Code::Out => {
                return Err(error_expected_code_but_got(Code::Out, &o));
            }
            Some(o) => o,
            None => Entry {
                pos: inn.pos.clone(),
                code: Code::Out,
                time: if inn.time > now { inn.time } else { now },
                account: String::new(),
                description: String::new(),
                comment: String::new(),
                tags: Vec::new(),
            },
        };
        if out.time.day > inn.time.day {
            let midnight = inn.time.next_midnight();
            let split = Entry {
                time: midnight,
                ..out.clone()
            };
            rest.push_front(out);
            rest.push_front(Entry {
                time: midnight,
                ..inn.clone()
            });
            sessions.push(Session { inn, out: split });
        } else {
            sessions.push(Session { inn, out });
        }
    }
    sessions
        .iter()
        .map(|s| entry_from_timeclock_in_out(true, &s.inn, &s.out))
        .collect()
}

/// The error for an old-format time log that does not alternate clock-ins
/// and clock-outs (errorExpectedCodeButGot).
fn error_expected_code_but_got(expected: Code, actual: &Entry) -> String {
    let l = actual.pos.line.to_string();
    format!(
        "{}:\n{l} | {actual}\n{} |{}^\n\nExpected a timeclock {expected} entry but got {}.\n\
         Please alternate i and o, beginning with i.",
        actual.pos,
        " ".repeat(l.len()),
        " ".repeat(actual.pos.col),
        actual.code
    )
}

/// A session made into a transaction with one virtual posting of its hours
/// (entryFromTimeclockInOut). It is described by its clock-in's
/// description, or else by its times (a session ending at the next midnight
/// ending at 23:59). With `require_time_ordered` (the old format), a
/// clock-out earlier than its clock-in has its own error.
fn entry_from_timeclock_in_out(
    require_time_ordered: bool,
    i: &Entry,
    o: &Entry,
) -> Result<Transaction, String> {
    if require_time_ordered && o.time < i.time {
        let l = o.pos.line.to_string();
        let pad = " ".repeat(l.len());
        return Err(format!(
            "{}:\n{pad} | {i}\n{l} | {o}\n{pad} |{}{}\n\n\
             This clockout time ({}) is earlier than the previous clockin.\n\
             Please adjust it to be later than {}.",
            o.pos,
            " ".repeat(o.pos.col + 2),
            "^".repeat(19),
            o.time,
            i.time
        ));
    }
    let hhmm = |nanos: u64| {
        let mins = nanos / 60_000_000_000;
        format!("{:02}:{:02}", mins / 60, mins % 60)
    };
    let out_tod = if o.time.day > i.time.day {
        hhmm(23 * 3_600_000_000_000 + 59 * 60_000_000_000)
    } else {
        hhmm(o.time.nanos)
    };
    let description = if i.description.is_empty() {
        format!("{}-{out_tod}", hhmm(i.time.nanos))
    } else {
        i.description.clone()
    };
    let nanos = o.time.nanos_since(i.time);
    let seconds = Decimal::new(nanos.into(), 9).normalize();
    let hours = &seconds / &Decimal::from_i64(3600);
    if hours.is_negative() {
        return Err(timeclock_entry_error(
            &[i],
            o,
            &format!(
                "This clockout is earlier than its clockin, on line {}.",
                i.pos.line
            ),
        ));
    }
    Ok(Transaction {
        pos: (i.pos.clone(), i.pos.clone()),
        date: i.time.day,
        status: Status::Cleared,
        description,
        comment: format!("{}{}", i.comment, o.comment),
        tags: i.tags.iter().chain(&o.tags).cloned().collect(),
        postings: vec![Posting {
            account: i.account.as_str().into(),
            amount: MixedAmount::from_amount(hrs(hours)),
            realness: Realness::Virtual,
            ..Posting::default()
        }],
        ..Transaction::default()
    })
}

pub(crate) mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn at(d: Day, h: u64, m: u64) -> LocalTime {
        LocalTime {
            day: d,
            nanos: (h * 3600 + m * 60) * 1_000_000_000,
        }
    }

    fn entry(code: Code, time: LocalTime, account: &str) -> Entry {
        Entry {
            pos: SourcePos::null(),
            code,
            time,
            account: account.to_string(),
            description: String::new(),
            comment: String::new(),
            tags: Vec::new(),
        }
    }

    fn descriptions(now: LocalTime, entries: Vec<Entry>) -> Vec<String> {
        timeclock_to_transactions(now, entries)
            .unwrap()
            .into_iter()
            .map(|t| t.description)
            .collect()
    }

    /// tests_Timeclock: "timeclockToTransactions tests", with a fixed now.
    #[cfg_attr(test, test)]
    pub(crate) fn timeclock_to_transactions_tests() {
        let today = day(2024, 3, 15);
        let now = LocalTime {
            day: today,
            nanos: (13 * 3600 + 37 * 60 + 5) * 1_000_000_000 + 123,
        };
        let nowstr = "13:37";
        let yesterday = today.add_days(-1);
        // started yesterday, split session at midnight
        assert_eq!(
            descriptions(now, vec![entry(Code::In, at(yesterday, 23, 0), "")]),
            ["23:00-23:59".to_string(), format!("00:00-{nowstr}")]
        );
        // split multi-day sessions at each midnight
        assert_eq!(
            descriptions(
                now,
                vec![entry(Code::In, at(today.add_days(-2), 23, 0), "")]
            ),
            [
                "23:00-23:59".to_string(),
                "00:00-23:59".to_string(),
                format!("00:00-{nowstr}")
            ]
        );
        // auto-clock-out if needed
        assert_eq!(
            descriptions(now, vec![entry(Code::In, at(today, 0, 0), "")]),
            [format!("00:00-{nowstr}")]
        );
        // use the clockin time for auto-clockout if it's in the future
        let future = LocalTime {
            nanos: now.nanos + 100_000_000_000,
            ..now
        };
        assert_eq!(
            descriptions(now, vec![entry(Code::In, future, "")]),
            ["13:38-13:38".to_string()]
        );
        // multiple open sessions
        let mut entries = vec![
            entry(Code::In, at(today, 0, 0), "a"),
            entry(Code::In, at(today, 1, 0), "b"),
            entry(Code::In, at(today, 2, 0), "c"),
            entry(Code::Out, at(today, 3, 0), "b"),
            entry(Code::Out, at(today, 4, 0), "a"),
            entry(Code::Out, at(today, 5, 0), "c"),
        ];
        for (i, e) in entries.iter_mut().enumerate() {
            e.pos.line = i + 1;
        }
        assert_eq!(
            descriptions(now, entries),
            ["00:00-04:00", "01:00-03:00", "02:00-05:00"]
        );
    }

    #[test]
    fn hours_are_exact() {
        let d = day(2020, 1, 30);
        let t = entry_from_timeclock_in_out(
            false,
            &entry(Code::In, at(d, 8, 38), "acct"),
            &entry(Code::Out, at(d, 9, 3), "acct"),
        )
        .unwrap();
        let q = &t.postings[0].amount.amounts_raw()[0].quantity;
        assert_eq!(q.places(), 255);
        assert_eq!(
            &(q * &Decimal::from_i64(3600)).round_to(0),
            &Decimal::from_i64(1500)
        );
    }

    #[test]
    fn local_times_show_as_haskell_shows_them() {
        let t = LocalTime {
            day: day(2020, 1, 2),
            nanos: (8 * 3600 + 5 * 60 + 7) * 1_000_000_000 + 500_000_000,
        };
        assert_eq!(t.to_string(), "2020-01-02 08:05:07.5");
        assert_eq!(at(day(2020, 1, 2), 8, 0).to_string(), "2020-01-02 08:00:00");
    }
}
