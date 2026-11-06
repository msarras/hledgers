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

/// A command (or the main program), as a cmdargs Mode: its names, its own
/// flags, which general flags it accepts (shown as named groups in its help)
/// and which hidden ones, what its positional arguments are called in help
/// (none if it takes none), and its help text file (hledgerCommandMode).
#[derive(Clone, Copy)]
pub struct Mode {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// The command's own flags (the unnamed group); hidden ones follow the
    /// common hidden flags in the hidden group.
    pub flags: &'static [Flag],
    pub general: General,
    pub hidden: Hidden,
    pub args: Option<&'static str>,
    /// The text file of the command's help (`Commands/NAME.txt`): its name,
    /// its official abbreviation in parentheses, the preamble, a snapshot of
    /// its flags help, and the postamble (parseCommandHelp).
    pub doc: &'static str,
}

impl Mode {
    /// A command's mode as most are: the general input, output and help
    /// flags, the common hidden flags, and a `[QUERY]` argument.
    pub const DEFAULT: Mode = Mode {
        name: "",
        aliases: &[],
        flags: &[],
        general: General::Groups1,
        hidden: Hidden::All,
        args: Some("[QUERY]"),
        doc: "",
    };

    /// Its flags in cmdargs' order (modeFlags): its own, the hidden ones,
    /// then the general ones group by group.
    pub fn all_flags(&self) -> impl Iterator<Item = &'static Flag> + use<> {
        let (flags, hidden) = (self.flags, self.hidden.flags());
        (flags.iter().filter(|f| !f.hidden))
            .chain(hidden.iter().flat_map(|fs| fs.iter()))
            .chain(flags.iter().filter(|f| f.hidden))
            .chain(
                self.general
                    .groups()
                    .iter()
                    .flat_map(|(_, fs)| fs.iter().flat_map(|g| g.iter())),
            )
    }

    /// Its names: the official one, then the abbreviations.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + use<> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }
}

/// The text file of a builtin command's help, embedded from the reference
/// source.
#[macro_export]
macro_rules! command_doc {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/hledger/hledger/Hledger/Cli/Commands/",
            $name,
            ".txt"
        ))
    };
}

/// Which general flags a command accepts, shown in its help as named groups
/// after its own flags: one of upstream's group lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum General {
    /// Input, output and help flags, in three groups (cligeneralflagsgroups1).
    Groups1,
    /// Input and help flags, in two groups (cligeneralflagsgroups2).
    Groups2,
    /// Help flags (cligeneralflagsgroups3).
    Groups3,
    /// Input and help flags, in one group (generalflagsgroup2).
    Group2,
    /// Help flags, in one group titled General flags (generalflagsgroup3).
    Group3,
}

/// A named group of flags: its title, as cmdargs shows it, and its flags.
type FlagGroup = (&'static str, &'static [&'static [Flag]]);

impl General {
    /// The named groups.
    pub fn groups(self) -> &'static [FlagGroup] {
        use super::opts::{HELP_FLAGS, INPUT_FLAGS, REPORT_FLAGS};
        const INPUT: &str = "\nGeneral input flags";
        const OUTPUT: &str = "\nGeneral output flags (affecting some commands)";
        const HELP: &str = "\nGeneral help flags";
        const GENERAL: &str = "\nGeneral flags";
        match self {
            General::Groups1 => &[
                (INPUT, &[INPUT_FLAGS]),
                (OUTPUT, &[REPORT_FLAGS]),
                (HELP, &[HELP_FLAGS]),
            ],
            General::Groups2 => &[(INPUT, &[INPUT_FLAGS]), (HELP, &[HELP_FLAGS])],
            General::Groups3 => &[(HELP, &[HELP_FLAGS])],
            General::Group2 => &[(GENERAL, &[INPUT_FLAGS, HELP_FLAGS])],
            General::Group3 => &[(GENERAL, &[HELP_FLAGS])],
        }
    }
}

/// Which of the common hidden flags a command accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hidden {
    /// The legacy flags and the config file flags (hiddenflags).
    All,
    /// The config file flags (confflags).
    Conf,
    /// The legacy flags (hiddenflagsformainmode).
    Legacy,
    /// None.
    None,
}

impl Hidden {
    fn flags(self) -> &'static [&'static [Flag]] {
        use super::opts::{CONF_FLAGS, LEGACY_FLAGS};
        match self {
            Hidden::All => &[LEGACY_FLAGS, CONF_FLAGS],
            Hidden::Conf => &[CONF_FLAGS],
            Hidden::Legacy => &[LEGACY_FLAGS],
            Hidden::None => &[],
        }
    }
}

/// The flags accepted by every command: the general input, output and help
/// flags, and the hidden ones (cligeneralflagsgroups1 and hiddenflags).
pub fn general_flags() -> impl Iterator<Item = &'static Flag> {
    super::opts::INPUT_FLAGS
        .iter()
        .chain(super::opts::REPORT_FLAGS)
        .chain(super::opts::HELP_FLAGS)
        .chain(super::opts::LEGACY_FLAGS)
        .chain(super::opts::CONF_FLAGS)
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

/// The flag a long name selects (cmdargs' lookupName): the one with that
/// name, or else the one with a name it is the start of; if several are,
/// an error naming them.
fn lookup_long(mode: &'static Mode, name: &str) -> Result<&'static Flag, String> {
    let matching = |m: &dyn Fn(&str) -> bool| -> Vec<(&'static str, &'static Flag)> {
        (mode.all_flags())
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

/// The flag a short name selects (lookupName): the one with that name; if
/// several have it, an error.
fn lookup_short(mode: &'static Mode, c: char) -> Result<&'static Flag, String> {
    let found: Vec<&Flag> = mode.all_flags().filter(|f| f.shorts.contains(&c)).collect();
    match found.as_slice() {
        [] => Err(format!("Unknown flag: -{c}")),
        [f] => Ok(f),
        _ => Err(format!(
            "Ambiguous flag '-{c}', could be any of: {}",
            vec![c.to_string(); found.len()].join(" ")
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
    general: General::Group3,
    hidden: Hidden::None,
    ..Mode::DEFAULT
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
                let flag = lookup_short(mode, c)?;
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

/// A mode's help, laid out as cmdargs does (showModeUsage without its
/// highlighting: helpText HelpFormatDefault and showText (Wrap 80)): the
/// usage line, the preamble, the flags (the command's own, then the general
/// ones group by group), the postamble, and a blank line.
/// Where the main mode's name and preamble name the program, they use its
/// name as invoked.
pub fn mode_usage(mode: &Mode, progname: &str) -> String {
    let (mut preamble, postamble) = parse_command_help(mode.doc);
    let name = match mode.name.strip_prefix("hledger ") {
        Some(rest) => {
            preamble = preamble.replace("`hledger ", &format!("`{progname} "));
            format!("{progname} {rest}")
        }
        None => mode.name.to_string(),
    };
    let mut usage = format!("{name} [OPTIONS]");
    if let Some(a) = mode.args {
        usage.push(' ');
        usage.push_str(a);
    }
    let mut text = vec![Text::Line(usage)];
    if !preamble.is_empty() {
        text.push(Text::Line(format!("  {preamble}")));
    }
    let own: Vec<&Flag> = mode.flags.iter().filter(|f| !f.hidden).collect();
    let groups = mode.general.groups();
    let mut flags = Vec::new();
    if !own.is_empty() && !groups.is_empty() {
        flags.push(Text::Line("Flags:".to_string()));
    }
    flags.extend(own.into_iter().map(help_flag));
    for (title, fs) in groups {
        flags.push(Text::Line(format!("{title}:")));
        flags.extend(fs.iter().flat_map(|g| g.iter()).map(help_flag));
    }
    let space = |text: &mut Vec<Text>, xs: Vec<Text>| {
        if !xs.is_empty() {
            text.push(Text::Line(String::new()));
            text.extend(xs);
        }
    };
    space(&mut text, flags);
    let postamble = postamble.into_iter().map(|l| Text::Line(l.to_string()));
    space(&mut text, postamble.collect());
    show_wrap(80, &text) + "\n"
}

/// A command help file's preamble, and its postamble lines
/// (parseCommandHelp): the name line, a blank line, the official
/// abbreviation in parentheses if any, the preamble, a `Flags:` block ending
/// at a blank line, and the postamble.
fn parse_command_help(doc: &str) -> (String, Vec<&str>) {
    let lines = hs_lines(doc);
    if lines.len() < 3 {
        return (String::new(), Vec::new());
    }
    let l3 = lines[2].trim_end_matches(crate::text::is_space);
    let rest = if l3.starts_with('(') && l3.ends_with(')') {
        &lines[3..]
    } else {
        &lines[2..]
    };
    let rest: Vec<&str> = rest.iter().copied().skip_while(|l| l.is_empty()).collect();
    let flags_at = rest
        .iter()
        .position(|l| *l == "Flags:")
        .unwrap_or(rest.len());
    let mut pre = &rest[..flags_at];
    while let [init @ .., ""] = pre {
        pre = init;
    }
    let preamble = pre.iter().map(|l| format!("{l}\n")).collect();
    let post = (rest[flags_at..].iter().copied())
        .skip_while(|l| !l.is_empty())
        .skip_while(|l| l.is_empty())
        .collect();
    (preamble, post)
}

/// Text with basic formatting (cmdargs' Text): a paragraph, wrapped with its
/// leading space as indent, or a row of columns, aligned with the other rows
/// of as many columns, the last column wrapped.
enum Text {
    Line(String),
    Cols(Vec<String>),
}

/// A flag's row in help (helpFlag): its short names, its long names with
/// the value's name after the first long (or short) one, and its help.
fn help_flag(f: &Flag) -> Text {
    let value = match f.arity {
        Arity::None => String::new(),
        Arity::Req => format!("={}", value_name(f)),
        Arity::Opt => format!("[={}]", value_name(f)),
    };
    let mut shorts: Vec<String> = f.shorts.iter().map(|c| format!("-{c}")).collect();
    let mut longs: Vec<String> = f.longs.iter().map(|l| format!("--{l}")).collect();
    if let Some(first) = longs.first_mut().or(shorts.first_mut()) {
        first.push_str(&value);
    }
    Text::Cols(vec![
        format!("  {}", shorts.join(" ")),
        format!(" {}", longs.join(" ")),
        format!("  {}", f.help),
    ])
}

/// The name of a flag's value in help: ITEM if it has none.
fn value_name(f: &Flag) -> &str {
    if f.value_name.is_empty() {
        "ITEM"
    } else {
        f.value_name
    }
}

/// Text laid out at this width (showWrap).
fn show_wrap(width: usize, xs: &[Text]) -> String {
    let len = |s: &str| s.chars().count();
    // the widths of every column but the last, for rows of each length
    let mut widths: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for x in xs {
        if let Text::Cols(cs) = x {
            let ws = widths
                .entry(cs.len())
                .or_insert_with(|| vec![0; cs.len() - 1]);
            for (w, c) in ws.iter_mut().zip(cs) {
                *w = (*w).max(len(c));
            }
        }
    }
    let width = width as isize;
    let mut out = String::new();
    for x in xs {
        match x {
            Text::Line(x) => {
                let b = x.trim_start_matches(crate::text::is_space);
                let a = &x[..x.len() - b.len()];
                for l in wrap1(width - len(a) as isize, b) {
                    out.push_str(a);
                    out.push_str(&l);
                    out.push('\n');
                }
            }
            Text::Cols(cs) => {
                let ws = &widths[&cs.len()];
                let last = &cs[cs.len() - 1];
                let indent = len(last) - len(last.trim_start_matches(crate::text::is_space));
                let n = ws.iter().sum::<usize>() + indent;
                for (i, l) in wrap1(width - n as isize, last).into_iter().enumerate() {
                    if i == 0 {
                        for (c, w) in cs.iter().zip(ws) {
                            out.push_str(c);
                            out.extend(std::iter::repeat_n(' ', w - len(c)));
                        }
                    } else {
                        out.extend(std::iter::repeat_n(' ', n));
                    }
                    out.push_str(&l);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Text split into lines of at most this width, or one empty line (wrap1).
fn wrap1(width: isize, x: &str) -> Vec<String> {
    let res = wrap(width, x);
    if res.is_empty() {
        vec![String::new()]
    } else {
        res
    }
}

/// Each line of the text split into strips of at most this width, at
/// spaces, keeping a line's leading space; a word longer than that is a
/// strip of its own (wrap).
fn wrap(width: isize, x: &str) -> Vec<String> {
    let is_space = crate::text::is_space;
    let mut out = Vec::new();
    for line in hs_lines(x) {
        // words, each with the number of spaces after it
        let mut chunks: Vec<(String, usize)> = Vec::new();
        let mut rest = line;
        while !rest.is_empty() {
            let word_end = rest.find(is_space).unwrap_or(rest.len());
            let after = &rest[word_end..];
            let next = after.trim_start_matches(is_space);
            let spaces = after[..after.len() - next.len()].chars().count();
            chunks.push((rest[..word_end].to_string(), spaces));
            rest = next;
        }
        let mut chunks = chunks.into_iter();
        let Some((mut a, mut b)) = chunks.next() else {
            continue;
        };
        for (c, d) in chunks {
            if ((a.chars().count() + b + c.chars().count()) as isize) < width {
                a.extend(std::iter::repeat_n(' ', b));
                a.push_str(&c);
            } else {
                out.push(std::mem::replace(&mut a, c));
            }
            b = d;
        }
        out.push(a);
    }
    out
}

/// The lines of a text, as Haskell's `lines` splits them: at each newline,
/// with no empty last line for a final newline.
fn hs_lines(s: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Help made bold on its first line and its headings (highlightHelp).
pub fn highlight_help(help: &str) -> String {
    let mut out = String::with_capacity(help.len() + 200);
    for (i, l) in hs_lines(help).into_iter().enumerate() {
        let heading = l.chars().next().is_some_and(char::is_alphanumeric) && l.ends_with(':');
        if i == 0 || heading {
            out.push_str(&format!("\x1b[1m{l}\x1b[22m\n"));
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
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
    #[test]
    fn show_wrap_lays_out_lines_and_columns() {
        // the example in cmdargs' System.Console.CmdArgs.Text
        let line = |s: &str| Text::Line(s.to_string());
        let cols = |cs: &[&str]| Text::Cols(cs.iter().map(|c| c.to_string()).collect());
        let demo = [
            line("Cooking for hungry people."),
            line("Welcome to my cookery recipe program, I sure hope you enjoy using it!"),
            line(""),
            cols(&["Omlette", "  A tasty eggy treat."]),
            cols(&[
                "  -m",
                " --mushrooms",
                "  Some mushrooms, or in fact any other ingredients you have in the cupboards",
            ]),
            cols(&["  -e", " --eggs", "  But always you need eggs"]),
            line(""),
            cols(&["Spagetti Bolognaise", "  An Italian delight."]),
            cols(&["  -s", " --spagetti", "  The first word in the name"]),
            cols(&["  -b", " --bolognaise", "  The second word in the name"]),
            cols(&["  -d", " --dolmio", "  The magic ingredient!"]),
            line(""),
            line(
                "    The author of this program explicitly disclaims any liability for poisoning people who get their recipes off the internet.",
            ),
        ];
        assert_eq!(
            show_wrap(50, &demo),
            "Cooking for hungry people.
Welcome to my cookery recipe program, I sure hope
you enjoy using it!

Omlette              A tasty eggy treat.
  -m --mushrooms   Some mushrooms, or in fact
                   any other ingredients you have
                   in the cupboards
  -e --eggs        But always you need eggs

Spagetti Bolognaise  An Italian delight.
  -s --spagetti    The first word in the name
  -b --bolognaise  The second word in the name
  -d --dolmio      The magic ingredient!

    The author of this program explicitly
    disclaims any liability for poisoning people
    who get their recipes off the internet.
"
        );
    }

    #[test]
    fn command_names_are_those_in_their_help_files() {
        for m in crate::cli::commands::BUILTIN_COMMANDS {
            let lines = hs_lines(m.doc);
            assert_eq!(lines[0], m.name);
            let l3 = lines[2].trim_end();
            let alias = l3.strip_prefix('(').and_then(|a| a.strip_suffix(')'));
            assert_eq!(
                alias.into_iter().collect::<Vec<_>>(),
                m.aliases,
                "{}",
                m.name
            );
        }
    }

    #[test]
    fn command_help_files_parse() {
        let (pre, post) = parse_command_help(crate::cli::commands::help::MODE.doc);
        assert_eq!(pre, "Show some part of hledger's documentation.\n");
        assert_eq!(post[0], "The part shown is selected by arguments:");
        let usage = mode_usage(&crate::cli::commands::codes::MODE, "hledger");
        assert!(usage.starts_with(
            "codes [OPTIONS] [QUERY]\n  List the codes seen in transactions, in the order parsed.\n\n\nGeneral input flags:\n  -f --file=[FMT:]FILE      Use this as the journal file (- means stdin). If\n"
        ));
        assert!(usage.contains("\n                            ru, uk, zh, or auto\n\n"));
        assert!(usage.ends_with("\n\n"));
    }
}
