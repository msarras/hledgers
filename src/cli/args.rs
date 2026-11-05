//! Flag tables and the command-line parser that reads them (the cmdargs part of
//! Hledger.Cli.CliOptions).

/// Whether a flag takes a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    /// No value: `--version`.
    None,
    /// A required value, joined or separate: `-fFILE`, `-f FILE`, `--file=FILE`, `--file FILE`.
    Req,
    /// An optional value, which must be joined: `--debug`, `--debug=2`.
    Opt,
}

/// A command-line flag. Its first long name is its name in [`RawOpts`].
pub struct Flag {
    pub longs: &'static [&'static str],
    pub shorts: &'static [char],
    pub arity: Arity,
    pub value_name: &'static str,
    pub help: &'static str,
    pub hidden: bool,
}

impl Flag {
    fn name(&self) -> &'static str {
        self.longs[0]
    }
}

/// A command (or the main program), with its flags beyond the general ones.
pub struct Mode {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub flags: &'static [Flag],
}

/// The flags accepted by every command.
pub static GENERAL_FLAGS: &[Flag] = &[
    Flag {
        longs: &["file"],
        shorts: &['f'],
        arity: Arity::Req,
        value_name: "[FMT:]FILE",
        help: "Use this as the journal file (- means stdin). If not specified, $LEDGER_FILE or ~/.hledger.journal will be used. If specified more than once, the files will be read in order. Each file's format (journal, csv, timeclock, timedot, rules..) is inferred from the file extension or a FMT: prefix. Some commands (add, import) write to the (first) file, and expect it to be in journal format.",
        hidden: false,
    },
    Flag {
        longs: &["version"],
        shorts: &[],
        arity: Arity::None,
        value_name: "",
        help: "show version information",
        hidden: false,
    },
];

/// Parsed options as (name, value) pairs in command-line order; positional
/// arguments have the name `args`. Where a flag repeats, the rightmost wins.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RawOpts(pub Vec<(String, String)>);

impl RawOpts {
    pub fn bool(&self, name: &str) -> bool {
        self.0.iter().any(|(n, _)| n == name)
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn args(&self) -> impl Iterator<Item = &str> {
        self.list("args")
    }

    /// The values of every option with this name, in order
    /// (listofstringopt).
    pub fn list<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the option appears an odd number of times (toggleopt).
    pub fn toggle(&self, name: &str) -> bool {
        self.list(name).count() % 2 == 1
    }

    /// What `f` makes of the last option name it accepts (choiceopt).
    pub fn choice<T>(&self, f: impl Fn(&str) -> Option<T>) -> Option<T> {
        self.0.iter().filter_map(|(n, _)| f(n)).next_back()
    }

    /// The option's value as an integer from 0 to the largest Int
    /// (maybeposintopt).
    pub fn posint(&self, name: &str) -> Result<Option<usize>, String> {
        let Some(v) = self.string(name) else {
            return Ok(None);
        };
        let usage = crate::reports::opts::usage_error;
        let n: i128 = v
            .trim()
            .parse()
            .map_err(|_| usage(&format!("could not parse {name} number: {v}")))?;
        if !(0..=i128::from(i64::MAX)).contains(&n) {
            return Err(usage(&format!(
                "argument to {name} must lie in the range 0 to {}, but is {n}",
                i64::MAX
            )));
        }
        Ok(Some(n as usize))
    }

    fn set(&mut self, name: &str, value: &str) {
        self.0.push((name.to_string(), value.to_string()));
    }
}

fn find_long(mode: &Mode, name: &str) -> Option<&'static Flag> {
    GENERAL_FLAGS
        .iter()
        .chain(mode.flags)
        .find(|f| f.longs.contains(&name))
}

fn find_short(mode: &Mode, c: char) -> Option<&'static Flag> {
    GENERAL_FLAGS
        .iter()
        .chain(mode.flags)
        .find(|f| f.shorts.contains(&c))
}

/// The position of the command name: the first argument that is neither a
/// general flag nor the separate value of one.
pub fn command_index(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return None;
        }
        let separate_value = if let Some(long) = a.strip_prefix("--") {
            !long.contains('=') && find_long(&NO_MODE, long).is_some_and(|f| f.arity == Arity::Req)
        } else if let Some(shorts) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            let mut cs = shorts.chars();
            let first = cs.next().and_then(|c| find_short(&NO_MODE, c));
            cs.next().is_none() && first.is_some_and(|f| f.arity == Arity::Req)
        } else {
            return Some(i);
        };
        i += if separate_value { 2 } else { 1 };
    }
    None
}

static NO_MODE: Mode = Mode {
    name: "",
    aliases: &[],
    flags: &[],
};

/// Parse arguments (without the command name) against a mode's flags.
pub fn process(mode: &Mode, args: &[String]) -> Result<RawOpts, String> {
    let mut opts = RawOpts::default();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if a == "--" {
            rest.by_ref().for_each(|a| opts.set("args", a));
        } else if let Some(long) = a.strip_prefix("--") {
            let (name, joined) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let flag = find_long(mode, name).ok_or_else(|| format!("Unknown flag: --{name}"))?;
            let value = match (flag.arity, joined) {
                (Arity::None, Some(_)) => {
                    return Err(format!("Unhandled argument to flag, none expected: {a}"));
                }
                (Arity::None | Arity::Opt, None) => "",
                (_, Some(v)) => v,
                (Arity::Req, None) => rest
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value, none provided"))?,
            };
            opts.set(flag.name(), value);
        } else if let Some(shorts) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (i, c) in shorts.char_indices() {
                let flag = find_short(mode, c).ok_or_else(|| format!("Unknown flag: -{c}"))?;
                let joined = &shorts[i + c.len_utf8()..];
                match flag.arity {
                    Arity::None => opts.set(flag.name(), ""),
                    Arity::Opt => {
                        opts.set(flag.name(), joined);
                        break;
                    }
                    Arity::Req if !joined.is_empty() => {
                        opts.set(flag.name(), joined);
                        break;
                    }
                    Arity::Req => {
                        let v = rest
                            .next()
                            .ok_or_else(|| format!("-{c} needs a value, none provided"))?;
                        opts.set(flag.name(), v);
                    }
                }
            }
        } else {
            opts.set("args", a);
        }
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn command_is_the_first_non_flag_argument() {
        assert_eq!(command_index(&strings(&["-f", "x", "bal", "a"])), Some(2));
        assert_eq!(command_index(&strings(&["-fx", "bal"])), Some(1));
        assert_eq!(command_index(&strings(&["--file", "x", "--version"])), None);
        assert_eq!(command_index(&strings(&["--file=x", "reg"])), Some(1));
        assert_eq!(command_index(&strings(&[])), None);
    }

    #[test]
    fn values_joined_or_separate() {
        let mode = &crate::cli::commands::MAIN_MODE;
        for args in [
            &["-fa", "-f", "b", "--file=c", "--file", "d"][..],
            &["-fa", "-fb", "-fc", "-fd"][..],
        ] {
            let opts = process(mode, &strings(args)).unwrap();
            assert_eq!(opts.string("file"), Some("d"));
            assert_eq!(opts.0.len(), 4);
        }
        let opts = process(mode, &strings(&["x", "--version", "--", "--file"])).unwrap();
        assert!(opts.bool("version"));
        assert_eq!(opts.args().collect::<Vec<_>>(), ["x", "--file"]);
    }

    #[test]
    fn flag_errors() {
        let mode = &crate::cli::commands::MAIN_MODE;
        let err = |args: &[&str]| process(mode, &strings(args)).unwrap_err();
        assert_eq!(err(&["-m"]), "Unknown flag: -m");
        assert_eq!(err(&["--nosuch"]), "Unknown flag: --nosuch");
        assert_eq!(err(&["-f"]), "-f needs a value, none provided");
    }
}
