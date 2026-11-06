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
    /// None.
    None,
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
            General::None => &[],
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

/// A mode with just the config file flags, for finding them
/// (confflagsmode).
pub static CONF_FLAGS_MODE: Mode = Mode {
    flags: super::opts::CONF_FLAGS,
    general: General::None,
    hidden: Hidden::None,
    args: Some(""),
    ..Mode::DEFAULT
};

/// A mode with just the help flags, for finding them (helpflagsmode).
pub static HELP_FLAGS_MODE: Mode = Mode {
    general: General::Groups3,
    hidden: Hidden::None,
    args: Some(""),
    ..Mode::DEFAULT
};

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

/// The builtin command, or else the add-on command, that an argument names
/// exactly or by a unique prefix of one of its names; an error if it could
/// be several (processMode's lookupName on the main mode's commands).
pub fn lookup_mode(addons: &[String], name: &str) -> Result<Option<&'static Mode>, String> {
    let builtins = super::commands::BUILTIN_COMMANDS
        .iter()
        .map(|m| (m.names().collect::<Vec<_>>(), Choice::Builtin(m)));
    let addon_modes = addons.iter().map(|a| (vec![a.as_str()], Choice::Addon(a)));
    let modes: Vec<(Vec<&str>, Choice)> = builtins.chain(addon_modes).collect();
    let matching = |m: &dyn Fn(&str) -> bool| -> Vec<(&str, &Choice)> {
        (modes.iter())
            .filter_map(|(ns, c)| ns.iter().find(|n| m(n)).map(|n| (*n, c)))
            .collect()
    };
    let exact = matching(&|n| n == name);
    let found = if exact.is_empty() {
        matching(&|n| n.starts_with(name))
    } else {
        exact
    };
    match found.as_slice() {
        [] => Ok(None),
        [(_, Choice::Builtin(m))] => Ok(Some(m)),
        [(_, Choice::Addon(a))] => Ok(Some(addon_mode(a))),
        _ => Err(format!(
            "Ambiguous mode '{name}', could be any of: {}",
            found.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" ")
        )),
    }
}

enum Choice<'a> {
    Builtin(&'static Mode),
    Addon(&'a String),
}

/// An add-on command's mode: no flags of its own, the general input, output
/// and help flags, and the hidden ones (addonCommandMode).
fn addon_mode(name: &str) -> &'static Mode {
    Box::leak(Box::new(Mode {
        name: Box::leak(name.to_string().into_boxed_str()),
        doc: "",
        ..Mode::DEFAULT
    }))
}

/// Parse arguments with the main mode, as cmdargs does: if the first names a
/// command (see [`lookup_mode`]), the rest with that command's flags, the
/// options then starting with its name as `command`; otherwise all with the
/// main mode's flags (process (mainmode addons)).
pub fn process_main(addons: &[String], args: &[String]) -> Result<RawOpts, String> {
    if let Some(first) = args.first()
        && let Some(mode) = lookup_mode(addons, first)?
    {
        let mut raw = process(mode, &args[1..])?;
        raw.0
            .insert(0, ("command".to_string(), mode.name.to_string()));
        return Ok(raw);
    }
    process(&super::commands::MAIN_MODE, args)
}

/// Parse arguments (without the command name) against a mode's flags, as
/// cmdargs' processFlags does: `--NAME[=VALUE]` with long names abbreviated
/// to a unique prefix, `-C[VALUE]` with valueless short flags joinable,
/// separate values, and positional arguments, all of them after `--`.
pub fn process(mode: &'static Mode, args: &[String]) -> Result<RawOpts, String> {
    let mut opts = RawOpts::default();
    let mut queue: std::collections::VecDeque<String> = args.iter().cloned().collect();
    let positional = |opts: &mut RawOpts, a: &str| match mode.args {
        Some(_) => {
            opts.set("args", a);
            Ok(())
        }
        None => Err(format!("Unhandled argument, none expected: {a}")),
    };
    while let Some(a) = queue.pop_front() {
        if a == "--" {
            while let Some(a) = queue.pop_front() {
                positional(&mut opts, &a)?;
            }
        } else if let Some(xs) = a.strip_prefix("--").filter(|x| !x.is_empty()) {
            let (name, joined) = match xs.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (xs, None),
            };
            let flag = lookup_long(mode, name)?;
            let value = match (flag.arity, joined) {
                (Arity::None, Some(_)) => {
                    return Err(format!("Unhandled argument to flag, none expected: --{xs}"));
                }
                (Arity::None | Arity::Opt, None) => flag.default.to_string(),
                (_, Some(v)) => v.to_string(),
                (Arity::Req, None) => queue
                    .pop_front()
                    .ok_or_else(|| format!("Flag requires argument: --{xs}"))?,
            };
            opts.set_all(flag, &value)?;
        } else if let Some(rest) = a
            .strip_prefix('-')
            .filter(|r| !r.is_empty() && !r.starts_with('-'))
        {
            let c = rest.chars().next().unwrap_or('-');
            let xs = &rest[c.len_utf8()..];
            let flag = lookup_short(mode, c)?;
            let unjoined = |v: &str| v.strip_prefix('=').unwrap_or(v).to_string();
            match flag.arity {
                Arity::None if xs.starts_with('=') => {
                    return Err(format!("Unhandled argument to flag, none expected: -{c}"));
                }
                Arity::None => {
                    opts.set_all(flag, flag.default)?;
                    if !xs.is_empty() {
                        queue.push_front(format!("-{xs}"));
                    }
                }
                Arity::Req if xs.is_empty() => {
                    let v = queue
                        .pop_front()
                        .ok_or_else(|| format!("Flag requires argument: -{c}"))?;
                    opts.set_all(flag, &v)?;
                }
                Arity::Req => opts.set_all(flag, &unjoined(xs))?,
                Arity::Opt if xs.is_empty() => opts.set_all(flag, flag.default)?,
                Arity::Opt => opts.set_all(flag, &unjoined(xs))?,
            }
        } else {
            positional(&mut opts, &a)?;
        }
    }
    Ok(opts)
}

/// Parse arguments with the main mode after checking that the given
/// required-value flags have values and giving --debug a value; a parse
/// error is shown with the arguments and a description of this parse
/// (cmdargsParseWith).
pub fn cmdargs_parse_with(
    req_val_flag_args: &[String],
    desc: &str,
    addons: &[String],
    args: &[String],
) -> Result<RawOpts, String> {
    check_req_val_flag_args_have_values(req_val_flag_args, args)?;
    process_main(addons, &ensure_debug_flag_has_value(args)).map_err(|e| {
        let shown: Vec<String> = args.iter().map(|a| quote_if_needed(a)).collect();
        format!(
            "{e}\n* while parsing the following args, {desc}:\n*  {}",
            shown.join(" ")
        )
    })
}

/// Like [`cmdargs_parse_with`] with a mode other than the main one, checking
/// every required-value flag (cmdargsParse).
pub fn cmdargs_parse_mode(
    desc: &str,
    mode: &'static Mode,
    args: &[String],
) -> Result<RawOpts, String> {
    check_req_val_flag_args_have_values(&flag_args().req_val, args)?;
    process(mode, &ensure_debug_flag_has_value(args)).map_err(|e| {
        let shown: Vec<String> = args.iter().map(|a| quote_if_needed(a)).collect();
        format!(
            "{e}\n* while parsing the following args, {desc}:\n*  {}",
            shown.join(" ")
        )
    })
}

/// The string in double quotes, with its double quotes escaped, if it has
/// quotes, white space or redirection characters (quoteIfNeeded).
fn quote_if_needed(s: &str) -> String {
    if s.contains(['\'', '"', ' ', '\t', '\n', '\r', '<', '>']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// A --debug without a numeric value after it made --debug=1; the first
/// --debug with one ends the search (ensureDebugFlagHasVal).
pub fn ensure_debug_flag_has_value(args: &[String]) -> Vec<String> {
    match args.iter().position(|a| a == "--debug") {
        None => args.to_vec(),
        Some(i) => match args.get(i + 1) {
            Some(c) if !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()) => args.to_vec(),
            _ => {
                let mut out = args[..i].to_vec();
                out.push("--debug=1".to_string());
                out.extend(ensure_debug_flag_has_value(&args[i + 1..]));
                out
            }
        },
    }
}

/// Check that each of the given required-value flags, written in full, is
/// followed by a value rather than by another known flag, up to a `--`
/// (checkReqValFlagArgsHaveValues). --debug and the flags that take a value
/// in some commands and none in others are not checked.
pub fn check_req_val_flag_args_have_values(
    req_val_flag_args: &[String],
    args: &[String],
) -> Result<(), String> {
    let fa = flag_args();
    let checkable = |a: &str| {
        a != "--debug"
            && req_val_flag_args.iter().any(|x| x == a)
            && !fa.ambiguous.iter().any(|x| x == a)
    };
    let mut long_prefixes: Vec<String> = (fa.req_val.iter().chain(&fa.opt_val))
        .filter(|x| x.starts_with("--"))
        .map(|x| format!("{x}="))
        .collect();
    long_prefixes.push("--debug=".to_string());
    let known = |b: &str| {
        (fa.no_val.iter().chain(&fa.req_val).chain(&fa.opt_val)).any(|x| x == b)
            || long_prefixes.iter().any(|p| b.starts_with(p.as_str()))
    };
    let usage = crate::reports::opts::usage_error;
    for (i, a) in args.iter().enumerate() {
        if a == "--" {
            break;
        }
        if !checkable(a) {
            continue;
        }
        match args.get(i + 1) {
            None => return Err(usage(&format!("{a} needs a value, none provided"))),
            Some(b) if known(b) => {
                return Err(usage(&format!(
                    "{a} needs a value, but the next argument is another flag: {b}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The flag arguments hledger knows, written in full (`--NAME`, `-C`), by
/// whether they take a value: the general flags' and the builtin commands'
/// (other than names of general flags); those taking a value in some
/// commands and none in others; and the general required-value ones.
pub struct FlagArgs {
    pub no_val: Vec<String>,
    pub req_val: Vec<String>,
    pub opt_val: Vec<String>,
    pub ambiguous: Vec<String>,
    pub general_req_val: Vec<String>,
}

/// The flag arguments (noValFlagArgs, reqValFlagArgs, optValFlagArgs,
/// ambiguousFlagArgs, generalReqValFlagArgs).
pub fn flag_args() -> &'static FlagArgs {
    static FA: std::sync::OnceLock<FlagArgs> = std::sync::OnceLock::new();
    FA.get_or_init(|| {
        let names = |fs: &mut dyn Iterator<Item = &'static Flag>, arity: Arity| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for f in fs.filter(|f| f.arity == arity) {
                for n in flag_names(f) {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
            out
        };
        let general = || general_flags();
        let command = || {
            super::commands::BUILTIN_COMMANDS
                .iter()
                .flat_map(|m| m.flags.iter().filter(|f| !f.hidden))
        };
        let general_names: Vec<String> = general().flat_map(flag_names).collect();
        let merged = |arity: Arity| {
            let mut out = names(&mut general(), arity);
            for n in names(&mut command(), arity) {
                if !general_names.contains(&n) && !out.contains(&n) {
                    out.push(n);
                }
            }
            out
        };
        let union = |a: Vec<String>, b: Vec<String>| {
            let mut out = a;
            out.extend(b);
            out
        };
        let req_any = union(
            names(&mut general(), Arity::Req),
            names(&mut command(), Arity::Req),
        );
        let no_any = union(
            names(&mut general(), Arity::None),
            names(&mut command(), Arity::None),
        );
        FlagArgs {
            no_val: merged(Arity::None),
            req_val: merged(Arity::Req),
            opt_val: merged(Arity::Opt),
            ambiguous: req_any
                .iter()
                .filter(|n| no_any.contains(n))
                .cloned()
                .collect(),
            general_req_val: names(&mut general(), Arity::Req),
        }
    })
}

/// A flag's names written in full (toFlagArg).
fn flag_names(f: &Flag) -> impl Iterator<Item = String> + '_ {
    (f.longs.iter().map(|l| format!("--{l}"))).chain(f.shorts.iter().map(|c| format!("-{c}")))
}

/// The required-value flag arguments a mode accepts, itself or through its
/// commands (modeReqValFlagArgs): for the main mode, every command's.
pub fn mode_req_val_flag_args(mode: &'static Mode) -> Vec<String> {
    let main = std::ptr::eq(mode, &super::commands::MAIN_MODE);
    let mode_flags: Vec<String> = if main {
        mode.all_flags()
            .chain(
                super::commands::BUILTIN_COMMANDS
                    .iter()
                    .flat_map(|m| m.all_flags()),
            )
            .flat_map(flag_names)
            .collect()
    } else {
        mode.all_flags().flat_map(flag_names).collect()
    };
    (flag_args().req_val.iter())
        .filter(|a| mode_flags.contains(a))
        .cloned()
        .collect()
}

/// Whether an argument looks like a flag: `-C...` or `--NAME...`
/// (isFlagArg).
pub fn is_flag_arg(a: &str) -> bool {
    is_short_flag_arg(a) || is_long_flag_arg(a)
}

fn is_short_flag_arg(a: &str) -> bool {
    a.strip_prefix('-')
        .and_then(|r| r.chars().next())
        .is_some_and(|c| c != '-')
}

fn is_long_flag_arg(a: &str) -> bool {
    a.len() > 2 && a.starts_with("--")
}

/// The command argument, and the arguments with the flags (and their
/// values) written before the command moved after it, before any `--`
/// (moveFlagsAfterCommand): the command or "", the arguments without it,
/// and the arguments with it first.
pub fn move_flags_after_command(args: &[String]) -> (String, Vec<String>, Vec<String>) {
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (as1, as2) = (&args[..split], &args[split..]);
    let mut moved: Vec<String> = Vec::new();
    let mut i = 0;
    while i < as1.len() {
        match is_movable_flag_arg(&as1[i], as1.get(i + 1).map(String::as_str)) {
            2 if i + 1 < as1.len() => {
                moved.extend(as1[i..i + 2].iter().cloned());
                i += 2;
            }
            1 => {
                moved.push(as1[i].clone());
                i += 1;
            }
            _ => break,
        }
    }
    let unmoved = &as1[i..];
    let join = |parts: &[&[String]]| parts.concat();
    match unmoved.first() {
        None => {
            let a = join(&[&moved, as2]);
            (String::new(), a.clone(), a)
        }
        Some(u) if u.starts_with('-') => {
            let a = join(&[&moved, unmoved, as2]);
            (String::new(), a.clone(), a)
        }
        Some(cmd) => {
            let a = join(&[&moved, &unmoved[1..], as2]);
            let mut with_cmd = vec![cmd.clone()];
            with_cmd.extend(a.iter().cloned());
            (cmd.clone(), a, with_cmd)
        }
    }
}

/// Whether to move an argument (1), it and the next (2), or neither (0)
/// (isMovableFlagArg): a required-value flag written in full takes the
/// next argument, unless it is a --debug not followed by a --debug value;
/// any other flag-like argument is moved alone.
fn is_movable_flag_arg(a: &str, next: Option<&str>) -> u8 {
    let fa = flag_args();
    let has = |v: &[String]| v.iter().any(|x| x == a);
    let debug_without_value = a == "--debug" && next.is_some_and(|n| !is_debug_value(n));
    if has(&fa.req_val) && !has(&fa.no_val) && !debug_without_value {
        2
    } else if is_flag_arg(a) {
        1
    } else {
        0
    }
}

/// Whether a string is a --debug value: an optionally negative number
/// (isDebugValue).
fn is_debug_value(s: &str) -> bool {
    let ds = s.strip_prefix('-').unwrap_or(s);
    !ds.is_empty() && ds.bytes().all(|b| b.is_ascii_digit())
}

/// The arguments without --conf FILE, --conf=FILE, --no-conf and -n
/// (dropCliSpecificOpts).
pub fn drop_cli_specific_opts(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--conf" {
            i += 2;
            continue;
        }
        if !(a.starts_with("--conf=") || a == "--no-conf" || a == "-n") {
            out.push(args[i].clone());
        }
        i += 1;
    }
    out
}

/// The arguments without --conf FILE, --conf=FILE, --no-conf and -n, for
/// a config file's general arguments (dropConfFlags).
pub fn drop_conf_flags(args: &[String]) -> Vec<String> {
    drop_cli_specific_opts(args)
}

/// The arguments without the flags (and their values) that a mode does not
/// accept, written in full (dropUnsupportedOpts).
pub fn drop_unsupported_opts(mode: &'static Mode, args: &[String]) -> Vec<String> {
    let fa = flag_args();
    let supports = |f: &str| mode.all_flags().flat_map(flag_names).any(|n| n == f);
    let req = |f: &str| fa.req_val.iter().any(|x| x == f);
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // upstream asks whether the mode supports "debug", not "--debug",
        // so a --debug followed by another argument is always dropped
        if a == "--debug" && i + 1 < args.len() {
            i += if is_debug_value(&args[i + 1]) { 2 } else { 1 };
            continue;
        }
        let f = if is_long_flag_arg(a) {
            Some((a.split('=').next().unwrap_or(a), !a.contains('=')))
        } else if is_short_flag_arg(a) {
            let two: String = a.chars().take(2).collect();
            Some((&a[..two.len()], a.chars().count() == 2))
        } else {
            None
        };
        match f {
            Some((f, separate)) if !supports(f) => {
                i += if req(f) && separate { 2 } else { 1 };
            }
            _ => {
                out.push(a.clone());
                i += 1;
            }
        }
    }
    out
}

/// The arguments split at the first `--`, which is dropped
/// (breakAtFirstSeparator).
pub fn break_at_first_separator(args: &[String]) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    }
}

/// `@FILE` arguments before any `--` replaced by the lines of the file,
/// recursively, `@` lines in a file read relative to that file's directory
/// (cmdargs' expandArgsAt).
pub fn expand_args_at(args: &[String]) -> Result<Vec<String>, String> {
    fn expand(
        seen: &mut Vec<String>,
        dir: &str,
        a: &str,
        out: &mut Vec<String>,
    ) -> Result<(), String> {
        let Some(x) = a.strip_prefix('@') else {
            out.push(a.to_string());
            return Ok(());
        };
        let chain_error = |what: &str, chain: Vec<&str>| {
            let lines: String = chain.iter().map(|s| format!("  {s}\n")).collect();
            Err(format!(
                "System.Console.CmdArgs.Explicit.expandArgsAt, {what}:\n{lines}"
            ))
        };
        if seen.iter().any(|s| s == x) {
            let mut chain: Vec<&str> = seen.iter().map(String::as_str).collect();
            chain.push(x);
            return chain_error("recursion in @ directives", chain);
        }
        if seen.len() > 15 {
            return chain_error(
                "over 15 @ directives deep",
                seen.iter().map(String::as_str).collect(),
            );
        }
        let path = if x.starts_with('/') {
            x.to_string()
        } else {
            format!("{dir}/{x}")
        };
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("{path}: openFile: {}", crate::read::io_error_text(&e)))?;
        let sub_dir = match x.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => x[..i].to_string(),
            None => ".".to_string(),
        };
        seen.push(x.to_string());
        for l in hs_lines(&src) {
            expand(seen, &sub_dir, l, out)?;
        }
        seen.pop();
        Ok(())
    }
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut out = Vec::new();
    for a in &args[..split] {
        expand(&mut Vec::new(), ".", a, &mut out)?;
    }
    out.extend(args[split..].iter().cloned());
    Ok(out)
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

pub(crate) mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_before_the_command_move_after_it() {
        let moved = |args: &[&str]| move_flags_after_command(&strings(args));
        assert_eq!(
            moved(&["-f", "x", "bal", "a"]),
            (
                "bal".to_string(),
                strings(&["-f", "x", "a"]),
                strings(&["bal", "-f", "x", "a"])
            )
        );
        assert_eq!(moved(&["-fx", "-BI", "bal"]).0, "bal");
        assert_eq!(moved(&["--file", "x", "--version"]).0, "");
        assert_eq!(moved(&["--debug", "bal"]).0, "bal");
        assert_eq!(moved(&["--debug", "2", "bal"]).0, "bal");
        assert_eq!(
            moved(&["-f"]),
            (String::new(), strings(&["-f"]), strings(&["-f"]))
        );
        assert_eq!(
            moved(&["-f", "x", "--", "bal"]),
            (
                String::new(),
                strings(&["-f", "x", "--", "bal"]),
                strings(&["-f", "x", "--", "bal"])
            )
        );
    }

    #[cfg_attr(test, test)]
    pub(crate) fn unsupported_flags_and_cli_specific_flags_are_dropped() {
        // upstream's doctests
        for args in [
            &["--debug", "1", "-f", "file"][..],
            &["--debug", "-f", "file"][..],
        ] {
            assert!(drop_unsupported_opts(&CONF_FLAGS_MODE, &strings(args)).is_empty());
        }
        let args = strings(&["-f", "x", "--conf", "c", "--debug", "2", "-n", "--pretty"]);
        assert_eq!(
            drop_unsupported_opts(&CONF_FLAGS_MODE, &args),
            strings(&["--conf", "c", "-n"])
        );
        assert_eq!(
            drop_cli_specific_opts(&args),
            strings(&["-f", "x", "--debug", "2", "--pretty"])
        );
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
        assert_eq!(err(&["-f"]), "Flag requires argument: -f");
        assert_eq!(err(&["--fil"]), "Flag requires argument: --fil");
        assert_eq!(
            err(&["-E=x"]),
            "Unhandled argument to flag, none expected: -E"
        );
        assert_eq!(
            process(mode, &strings(&["-f=x"])).unwrap().string("file"),
            Some("x")
        );
        let check = |args: &[&str]| {
            check_req_val_flag_args_have_values(&flag_args().req_val, &strings(args)).unwrap_err()
        };
        assert_eq!(
            check(&["-f"]),
            "-f needs a value, none provided (use -h to see usage)"
        );
        assert_eq!(
            check(&["-f", "--pretty"]),
            "-f needs a value, but the next argument is another flag: --pretty (use -h to see usage)"
        );
        assert_eq!(
            check(&["--file", "--begin=2020"]),
            "--file needs a value, but the next argument is another flag: --begin=2020 (use -h to see usage)"
        );
        let ok = |args: &[&str]| {
            check_req_val_flag_args_have_values(&flag_args().req_val, &strings(args))
        };
        assert!(ok(&["-f", "-"]).is_ok());
        assert!(ok(&["--sort", "-date"]).is_ok());
        assert!(ok(&["-m"]).is_ok()); // a value in some commands, none in others
        assert!(ok(&["--", "-f"]).is_ok());
    }

    #[test]
    fn commands_are_found_by_name_alias_or_unique_prefix() {
        let name = |a: &str| lookup_mode(&[], a).map(|m| m.map(|m| m.name));
        assert_eq!(name("reg"), Ok(Some("register")));
        assert_eq!(name("balancesheete"), Ok(Some("balancesheetequity")));
        assert_eq!(name("nosuch"), Ok(None));
        assert_eq!(
            name("balan"),
            Err(
                "Ambiguous mode 'balan', could be any of: balance balancesheetequity balancesheet"
                    .to_string()
            )
        );
        let addons = strings(&["addon", "addon.sh"]);
        assert_eq!(
            lookup_mode(&addons, "addon").unwrap().map(|m| m.name),
            Some("addon")
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
