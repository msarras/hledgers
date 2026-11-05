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

/// A command-line flag (a cmdargs Flag): its long and short names, whether
/// it takes a value, and the options it sets in [`RawOpts`]: by default one
/// named as its first long name (or its short name), set to its value. A
/// flag without a value sets them to `default` (usually empty), and a flag
/// whose value is optional uses `default` when none is given.
pub struct Flag {
    pub longs: &'static [&'static str],
    pub shorts: &'static [char],
    pub arity: Arity,
    pub sets: &'static [&'static str],
    pub default: &'static str,
    pub value_name: &'static str,
    pub help: &'static str,
    pub hidden: bool,
    /// A check of the flag's value as it is read, whose error is a
    /// command-line error (a cmdargs flag update function's Left).
    pub check: Option<FlagCheck>,
}

/// A check of a flag's value.
pub type FlagCheck = fn(&str) -> Result<(), String>;

impl Flag {
    /// A flag with these long names and arity, setting the option of its
    /// first name.
    pub const fn new(
        longs: &'static [&'static str],
        arity: Arity,
        value_name: &'static str,
        help: &'static str,
    ) -> Flag {
        Flag {
            longs,
            shorts: &[],
            arity,
            sets: &[],
            default: "",
            value_name,
            help,
            hidden: false,
            check: None,
        }
    }

    /// This flag with this check of its value.
    pub const fn check(self, check: FlagCheck) -> Flag {
        Flag {
            check: Some(check),
            ..self
        }
    }

    /// This flag with these short names.
    pub const fn short(self, shorts: &'static [char]) -> Flag {
        Flag { shorts, ..self }
    }

    /// This flag setting these options instead.
    pub const fn sets(self, sets: &'static [&'static str]) -> Flag {
        Flag { sets, ..self }
    }

    /// This flag with this default value.
    pub const fn default(self, default: &'static str) -> Flag {
        Flag { default, ..self }
    }

    /// This flag, not shown in help.
    pub const fn hidden(self) -> Flag {
        Flag {
            hidden: true,
            ..self
        }
    }

    /// The options it sets.
    fn names(&self) -> Vec<&'static str> {
        if !self.sets.is_empty() {
            self.sets.to_vec()
        } else if let Some(l) = self.longs.first() {
            vec![*l]
        } else {
            vec![short_name(self.shorts[0])]
        }
    }
}

/// A short flag's name as an option name.
fn short_name(c: char) -> &'static str {
    const NAMES: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz?";
    NAMES.find(c).map_or("?", |i| &NAMES[i..i + c.len_utf8()])
}

/// A command (or the main program), with its flags beyond the general ones.
pub struct Mode {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub flags: &'static [Flag],
}

/// The flags accepted by every command: the general input, output and help
/// flags, and the hidden ones (cligeneralflagsgroups1 and hiddenflags).
pub fn general_flags() -> impl Iterator<Item = &'static Flag> {
    super::opts::INPUT_FLAGS
        .iter()
        .chain(super::opts::REPORT_FLAGS)
        .chain(super::opts::HELP_FLAGS)
        .chain(super::opts::HIDDEN_FLAGS)
}

/// Parsed options as (name, value) pairs in command-line order; positional
/// arguments have the name `args`. Where a flag repeats, the rightmost wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

    fn set_all(&mut self, flag: &Flag, value: &str) -> Result<(), String> {
        if let Some(check) = flag.check {
            check(value)?;
        }
        for n in flag.names() {
            self.set(n, value);
        }
        Ok(())
    }
}

/// The flags of a mode in cmdargs' order: the command's own, the hidden
/// ones (the general ones first), then the general input, output and
/// help flags.
fn cmdargs_flags(mode: &'static Mode) -> impl Iterator<Item = &'static Flag> {
    use super::opts::{HELP_FLAGS, HIDDEN_FLAGS, INPUT_FLAGS, REPORT_FLAGS};
    (mode.flags.iter().filter(|f| !f.hidden))
        .chain(HIDDEN_FLAGS)
        .chain(mode.flags.iter().filter(|f| f.hidden))
        .chain(INPUT_FLAGS)
        .chain(REPORT_FLAGS)
        .chain(HELP_FLAGS)
}

/// The flag a long name selects (cmdargs' lookupName): the one with that
/// name, or else the one with a name it is the start of; if several are,
/// an error naming them.
fn lookup_long(mode: &'static Mode, name: &str) -> Result<&'static Flag, String> {
    let matching = |m: &dyn Fn(&str) -> bool| -> Vec<(&'static str, &'static Flag)> {
        (cmdargs_flags(mode))
            .filter_map(|f| f.longs.iter().find(|l| m(l)).map(|l| (*l, f)))
            .collect()
    };
    let exact = matching(&|l| l == name);
    let found = if exact.is_empty() {
        matching(&|l| l.starts_with(name))
    } else {
        exact
    };
    match found.as_slice() {
        [] => Err(format!("Unknown flag: --{name}")),
        [(_, f)] => Ok(f),
        _ => Err(format!(
            "Ambiguous flag '--{name}', could be any of: {}",
            found.iter().map(|(l, _)| *l).collect::<Vec<_>>().join(" ")
        )),
    }
}

fn find_long(mode: &'static Mode, name: &str) -> Option<&'static Flag> {
    mode.flags
        .iter()
        .chain(general_flags())
        .find(|f| f.longs.contains(&name))
}

fn find_short(mode: &'static Mode, c: char) -> Option<&'static Flag> {
    mode.flags
        .iter()
        .chain(general_flags())
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
pub fn process(mode: &'static Mode, args: &[String]) -> Result<RawOpts, String> {
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
            let flag = lookup_long(mode, name)?;
            let value = match (flag.arity, joined) {
                (Arity::None, Some(_)) => {
                    return Err(format!("Unhandled argument to flag, none expected: {a}"));
                }
                (Arity::None | Arity::Opt, None) => flag.default,
                (_, Some(v)) => v,
                (Arity::Req, None) => {
                    let v = rest
                        .next()
                        .ok_or_else(|| format!("--{name} needs a value, none provided"))?;
                    if flag.longs.contains(&name) {
                        check_value_is_not_a_flag(&format!("--{name}"), v)?;
                    }
                    v
                }
            };
            opts.set_all(flag, value)?;
        } else if let Some(shorts) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (i, c) in shorts.char_indices() {
                let flag = find_short(mode, c).ok_or_else(|| format!("Unknown flag: -{c}"))?;
                let joined = &shorts[i + c.len_utf8()..];
                match flag.arity {
                    Arity::None => opts.set_all(flag, flag.default)?,
                    Arity::Opt => {
                        opts.set_all(
                            flag,
                            if joined.is_empty() {
                                flag.default
                            } else {
                                joined
                            },
                        )?;
                        break;
                    }
                    Arity::Req if !joined.is_empty() => {
                        opts.set_all(flag, joined)?;
                        break;
                    }
                    Arity::Req => {
                        let v = rest
                            .next()
                            .ok_or_else(|| format!("-{c} needs a value, none provided"))?;
                        if shorts.len() == c.len_utf8() {
                            check_value_is_not_a_flag(a, v)?;
                        }
                        opts.set_all(flag, v)?;
                    }
                }
            }
        } else {
            opts.set("args", a);
        }
    }
    Ok(opts)
}

/// An error if a required-value flag, written in full, is followed by what
/// looks like another of hledger's flags instead of its value
/// (checkReqValFlagArgsHaveValues). --debug is not checked, nor are flags
/// that need a value in some commands and none in others.
fn check_value_is_not_a_flag(written: &str, next: &str) -> Result<(), String> {
    let has_arity =
        |arity| all_flags().any(|f| f.arity == arity && flag_args(f).any(|x| x == written));
    let ambiguous = has_arity(Arity::Req) && has_arity(Arity::None);
    let next_is_flag = next.starts_with("--debug=")
        || all_flags().any(|f| {
            flag_args(f).any(|x| {
                x == next
                    || (f.arity != Arity::None
                        && x.starts_with("--")
                        && next
                            .strip_prefix(x.as_str())
                            .is_some_and(|r| r.starts_with('=')))
            })
        });
    if written == "--debug" || ambiguous || !next_is_flag {
        return Ok(());
    }
    Err(format!(
        "{written} needs a value, but the next argument is another flag: {next}"
    ))
}

/// The general flags and every builtin command's flags.
fn all_flags() -> impl Iterator<Item = &'static Flag> {
    general_flags().chain(
        super::commands::BUILTIN_COMMANDS
            .iter()
            .flat_map(|m| m.flags.iter()),
    )
}

/// A flag's names as written in full: `--NAME`, `-C` (toFlagArg).
fn flag_args(f: &Flag) -> impl Iterator<Item = String> + '_ {
    (f.longs.iter().map(|l| format!("--{l}"))).chain(f.shorts.iter().map(|c| format!("-{c}")))
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
        // long flags may be abbreviated to an unambiguous prefix
        let raw = process(mode, &strings(&["--vers", "--fi=x"])).unwrap();
        assert!(raw.bool("version"));
        assert_eq!(raw.string("file"), Some("x"));
        assert_eq!(
            err(&["--ex"]),
            "Ambiguous flag '--ex', could be any of: exchange examples"
        );
        assert_eq!(err(&["-f"]), "-f needs a value, none provided");
        assert_eq!(
            err(&["-f", "--pretty"]),
            "-f needs a value, but the next argument is another flag: --pretty"
        );
        assert_eq!(
            err(&["--file", "--begin=2020"]),
            "--file needs a value, but the next argument is another flag: --begin=2020"
        );
    }
}
