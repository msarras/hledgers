//! A port of hledger's tools/generatejournal.hs, producing the same output
//! byte for byte:
//!
//! generatejournal NUMTXNS NUMACCTS ACCTDEPTH [--chinese|--mixed] [--start=YYYY-MM-DD] [--days=N]
//!
//! NUMTXNS transactions, one per day from 2000-01-01 (or spread evenly over
//! --days days from --start), between NUMACCTS account names with depths up to
//! ACCTDEPTH, every third one with a `@` unit cost and every third with a `@@`
//! total cost; then NUMTXNS market prices on the same dates. Names are hex
//! numbers, or wide CJK characters with --chinese, or both with --mixed.

use std::fmt::Write;

pub struct Options {
    txns: usize,
    accounts: usize,
    depth: usize,
    names: Names,
    start: i64,
    days: Option<i64>,
}

#[derive(Clone, Copy, PartialEq)]
enum Names {
    Hex,
    Wide,
    Mixed,
}

impl Options {
    /// Parse generatejournal.hs's arguments: words starting with `-` are
    /// options, the others the three numbers.
    pub fn parse(args: &[String]) -> Result<Options, String> {
        let (opts, nums): (Vec<&String>, Vec<&String>) =
            args.iter().partition(|a| a.starts_with('-'));
        let value = |name: &str| opts.iter().find_map(|o| o.strip_prefix(name));
        let [txns, accounts, depth] = nums[..] else {
            return Err("expected NUMTXNS NUMACCTS ACCTDEPTH".into());
        };
        let num = |s: &str| s.parse::<usize>().map_err(|_| format!("not a number: {s}"));
        let start = match value("--start=") {
            Some(d) => parse_day(d).ok_or_else(|| format!("could not parse date: {d}"))?,
            None => days_from_civil(2000, 1, 1),
        };
        let days = value("--days=")
            .map(|n| n.parse().map_err(|_| format!("not a number: {n}")))
            .transpose()?;
        let names = if opts.iter().any(|o| *o == "--mixed") {
            Names::Mixed
        } else if opts.iter().any(|o| *o == "--chinese") {
            Names::Wide
        } else {
            Names::Hex
        };
        let depth = num(depth)?;
        if depth == 0 {
            return Err("ACCTDEPTH must be at least 1".into());
        }
        Ok(Options {
            txns: num(txns)?,
            accounts: num(accounts)?,
            depth,
            names,
            start,
            days,
        })
    }
}

/// The journal text.
pub fn journal(o: &Options) -> String {
    let date = |i: usize| match o.days {
        None => o.start + i as i64,
        Some(n) => o.start + (i as i64 * n).div_euclid(o.txns as i64),
    };
    let names = account_names(o.names, o.depth, o.accounts);
    let letter = |i: usize| (b'A' + (i % 26) as u8) as char;
    let mut out = String::with_capacity(o.txns * 200);
    for i in 0..o.txns {
        let n = i + 1;
        let acct1 = &names[(2 * i) % names.len()];
        let acct2 = &names[(2 * i + 1) % names.len()];
        let (comm, pricecomm) = (letter(i), letter(i + 1));
        let rate = Decimal(1, 7).add(Decimal(2, 1).mul(Decimal(0, (n % 60) as i64)));
        let (amt2, amt2comm, price) = match n % 3 {
            0 => (Decimal(0, -(n as i64)), comm, String::new()),
            1 => (
                Decimal(0, -(n as i64)).mul(rate),
                pricecomm,
                format!(" @ {} {pricecomm}", rate.show()),
            ),
            _ => (
                Decimal(0, -(n as i64)),
                pricecomm,
                format!(" @@ {n} {pricecomm}"),
            ),
        };
        let _ = write!(
            out,
            "{} transaction {n}\n  {acct1:<40}  {n:>2} {comm}{price}\n  {acct2:<40}  {} {amt2comm}\n\n",
            show_day(date(i)),
            amt2.show()
        );
    }
    let rates = rates();
    for i in 0..o.txns {
        let _ = writeln!(
            out,
            "P {} A  {:.2} B",
            show_day(date(i)),
            rates[i % rates.len()]
        );
    }
    out
}

/// `[0.70, 0.71 .. 1.3]` as Haskell enumerates Doubles (start + k * step while
/// <= limit + step/2), then back down, without repeating either end.
fn rates() -> Vec<f64> {
    let step = 0.71 - 0.70;
    let up: Vec<f64> = (0..)
        .map(|k| 0.70 + k as f64 * step)
        .take_while(|&x| x <= 1.3 + step / 2.0)
        .collect();
    let down = up.iter().rev().skip(1).take(up.len() - 2).copied();
    up.iter().copied().chain(down).collect()
}

/// The first `count` of the account names: runs of `depth` unique names, each
/// run giving `a`, `a:b`, `a:b:c`, ...
fn account_names(kind: Names, depth: usize, count: usize) -> Vec<String> {
    let mut unique = unique_names(kind);
    let mut names = Vec::with_capacity(count);
    while names.len() < count {
        let mut path = String::new();
        for name in unique.by_ref().take(depth) {
            if !path.is_empty() {
                path.push(':');
            }
            path.push_str(&name);
            names.push(path.clone());
        }
    }
    names.truncate(count);
    names
}

fn unique_names(kind: Names) -> Box<dyn Iterator<Item = String>> {
    let hex = (1u64..).map(|n| format!("{n:x}"));
    // Every n-character window of the wide characters, cycling, for n = 1, 2, ...
    let wide = (1usize..).flat_map(|n| {
        let chars: Vec<char> = (0x3400..=0x4db0).filter_map(char::from_u32).collect();
        (0..chars.len()).map(move |i| (0..n).map(|j| chars[(i + j) % chars.len()]).collect())
    });
    match kind {
        Names::Hex => Box::new(hex),
        Names::Wide => Box::new(wide),
        Names::Mixed => Box::new(hex.zip(wide).flat_map(|(h, w)| [h, w])),
    }
}

/// The part of Data.Decimal the generator uses: (decimal places, mantissa).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Decimal(u32, i64);

impl Decimal {
    /// `+`: aligned to the larger number of places, not normalised.
    fn add(self, other: Decimal) -> Decimal {
        match (self, other) {
            (Decimal(_, 0), d) | (d, Decimal(_, 0)) => d,
            (Decimal(e1, n1), Decimal(e2, n2)) => {
                let e = e1.max(e2);
                Decimal(e, n1 * 10i64.pow(e - e1) + n2 * 10i64.pow(e - e2))
            }
        }
    }

    /// `*`: exact, then normalised to the fewest places.
    fn mul(self, other: Decimal) -> Decimal {
        if self.1 == 0 || other.1 == 0 {
            return Decimal(0, 0);
        }
        let Decimal(mut e, mut n) = Decimal(self.0 + other.0, self.1 * other.1);
        while e > 0 && n % 10 == 0 {
            (e, n) = (e - 1, n / 10);
        }
        Decimal(e, n)
    }

    /// Data.Decimal's `show`: all the places, and at least one integer digit.
    fn show(self) -> String {
        let Decimal(e, n) = self;
        let sign = if n < 0 { "-" } else { "" };
        let digits = n.unsigned_abs().to_string();
        if e == 0 {
            return format!("{sign}{digits}");
        }
        let len = digits.len();
        let padded = "0".repeat((e as usize + 1).saturating_sub(len)) + &digits;
        let (int, frac) = padded.split_at(1.max(len as i64 - e as i64) as usize);
        format!("{sign}{int}.{frac}")
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `show` of a Day: YYYY-MM-DD.
pub fn show_day(days: i64) -> String {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn parse_day(s: &str) -> Option<i64> {
    let mut parts = s.trim().splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    let days = days_from_civil(y, m, d);
    (show_day(days) == format!("{y:04}-{m:02}-{d:02}")).then_some(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate(args: &str) -> String {
        let args: Vec<String> = args.split_whitespace().map(String::from).collect();
        journal(&Options::parse(&args).unwrap())
    }

    /// Derived by hand from generatejournal.hs: transaction 1 (1 mod 3) has rate
    /// 0.70 + 0.01*1 = 0.71 and amount -1 * 0.71; transaction 2 a total cost of 2;
    /// transaction 3 no cost. Data.Decimal's `0.70` literal is 0.7, one place.
    #[test]
    fn first_transactions_and_prices() {
        let j = generate("10 10 10");
        let pad = |s: &str| format!("{s:<40}");
        let expected = format!(
            "2000-01-01 transaction 1\n  {}   1 A @ 0.71 B\n  {}  -0.71 B\n\n\
             2000-01-02 transaction 2\n  {}   2 B @@ 2 C\n  {}  -2 C\n\n\
             2000-01-03 transaction 3\n  {}   3 C\n  {}  -3 C\n\n",
            pad("1"),
            pad("1:2"),
            pad("1:2:3"),
            pad("1:2:3:4"),
            pad("1:2:3:4:5"),
            pad("1:2:3:4:5:6")
        );
        assert!(j.starts_with(&expected), "{j}");
        let prices: Vec<&str> = j.lines().filter(|l| l.starts_with("P ")).take(3).collect();
        assert_eq!(
            prices,
            [
                "P 2000-01-01 A  0.70 B",
                "P 2000-01-02 A  0.71 B",
                "P 2000-01-03 A  0.72 B"
            ]
        );
        assert_eq!(j.lines().filter(|l| l.starts_with("P ")).count(), 10);
    }

    /// Upstream committed one generated journal; the port must reproduce it.
    #[test]
    fn matches_upstreams_generated_example() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../hledger/examples/1ktxns-100accts.journal"
        );
        let upstream = std::fs::read_to_string(path).unwrap();
        assert!(generate("1000 100 10") == upstream, "differs from {path}");
    }
    #[test]
    fn decimals_render_as_data_decimal_does() {
        let rate = |n: i64| Decimal(1, 7).add(Decimal(2, 1).mul(Decimal(0, n)));
        let shown: Vec<String> = [0, 1, 10, 30, 31, 59]
            .iter()
            .map(|&n| rate(n).show())
            .collect();
        assert_eq!(shown, ["0.7", "0.71", "0.8", "1.0", "1.01", "1.29"]);
        assert_eq!(Decimal(0, -10).mul(rate(10)).show(), "-8");
        assert_eq!(Decimal(0, -4).mul(rate(4)).show(), "-2.96");
        assert_eq!(Decimal(2, 5).show(), "0.05");
        assert_eq!(Decimal(1, -5).show(), "-0.5");
    }

    #[test]
    fn rates_rise_and_fall_by_hundredths() {
        let r: Vec<String> = rates().iter().map(|x| format!("{x:.2}")).collect();
        assert_eq!(r.len(), 120);
        assert_eq!(
            (
                r[0].as_str(),
                r[60].as_str(),
                r[61].as_str(),
                r[119].as_str()
            ),
            ("0.70", "1.30", "1.29", "0.71")
        );
    }

    #[test]
    fn names_and_dates() {
        assert_eq!(
            account_names(Names::Hex, 3, 5),
            ["1", "1:2", "1:2:3", "4", "4:5"]
        );
        let mixed = account_names(Names::Mixed, 2, 4);
        assert_eq!(mixed, ["1", "1:\u{3400}", "2", "2:\u{3401}"]);
        assert_eq!(
            account_names(Names::Wide, 1, 6578)[6577],
            "\u{3400}\u{3401}"
        );
        assert_eq!(show_day(days_from_civil(2000, 1, 1)), "2000-01-01");
        assert_eq!(show_day(days_from_civil(2000, 1, 1) + 366), "2001-01-01");
        assert_eq!(parse_day("2024-02-29"), Some(days_from_civil(2024, 2, 29)));
        assert_eq!(parse_day("2023-02-29"), None);
        let spread = generate("4 2 1 --start=2020-01-01 --days=2");
        let dates: Vec<&str> = spread
            .lines()
            .filter(|l| l.contains("transaction"))
            .map(|l| &l[..10])
            .collect();
        assert_eq!(
            dates,
            ["2020-01-01", "2020-01-01", "2020-01-02", "2020-01-02"]
        );
    }
}
