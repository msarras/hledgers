//! Config files: extra command-line arguments, general and per command, and
//! command aliases, read from a hledger.conf file (Hledger.Cli.Conf).

use crate::parse::{Item, ParseError};
use crate::platform::{config_dir, home_dir};

/// Config file sections: each name with its argument lines and their line
/// numbers.
type Sections = Vec<(String, Vec<(usize, String)>)>;

/// A config file: its path and text, and its sections in order, the first
/// being the general one; each holds its argument lines with their line
/// numbers (Conf, ConfSection).
#[derive(Clone, Debug, Default)]
pub struct Conf {
    pub file: String,
    text: String,
    sections: Sections,
}

impl Conf {
    /// The arguments of the last section with this name, split into words
    /// (confLookup).
    pub fn lookup(&self, name: &str) -> Vec<String> {
        let section = self.sections.iter().rev().find(|(n, _)| n == name);
        section.map_or_else(Vec::new, |(_, args)| {
            (args.iter())
                .flat_map(|(_, a)| crate::text::words_quoted(a).unwrap_or_default())
                .collect()
        })
    }

    /// The command aliases of the [alias] sections, in order of definition
    /// (confAliases); `validate` has checked their form.
    pub fn aliases(&self) -> Vec<(String, String)> {
        self.aliases_checked().unwrap_or_default()
    }

    /// The command aliases, or an error about a line without `=`
    /// (confAliasesE).
    fn aliases_checked(&self) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for (_, args) in self.sections.iter().filter(|(n, _)| n == "alias") {
            for (lnum, l) in args {
                match l.split_once('=') {
                    Some((name, cmdline)) if !name.trim().is_empty() => {
                        out.push((name.trim().to_string(), cmdline.trim().to_string()));
                    }
                    _ => {
                        return Err(self.error_at(
                            *lnum,
                            l,
                            "an [alias] section line should look like: NAME = COMMAND [ARGS..]",
                        ));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Check that every argument line splits into words (quotes are
    /// balanced) and that the aliases are well formed (confArgsE,
    /// confAliasesE).
    fn validate(&self) -> Result<(), String> {
        for (lnum, l) in self.sections.iter().flat_map(|(_, args)| args) {
            if crate::text::words_quoted(l).is_none() {
                return Err(self.error_at(
                    *lnum,
                    l,
                    "this config file line has an unmatched quote (' or \").",
                ));
            }
        }
        self.aliases_checked().map(|_| ())
    }

    /// An error about a line of this file: its path and line number, the
    /// line as written, and the explanation (confErrorAt).
    fn error_at(&self, lnum: usize, fallback: &str, explanation: &str) -> String {
        let line = self.text.lines().nth(lnum - 1).unwrap_or(fallback);
        let excerpt = crate::read::errors::decorate_excerpt(lnum, None, &format!("{line}\n"));
        format!("{}:{lnum}:\n{excerpt}{explanation}", self.file)
    }
}

/// What a command name resolves to (ResolvedCommand): a hledger command and
/// the arguments to put before the command line's, or a shell command line.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolvedCommand {
    Hledger(String, Vec<String>),
    Shell(String),
}

/// Resolve a command name that may be a command alias, the first definition
/// of each name in `aliases` counting. An alias may name another; an exact
/// builtin command name, or a name already seen, ends the expansion
/// (expandCommandAlias).
pub fn expand_command_alias(
    is_builtin: impl Fn(&str) -> bool,
    aliases: &[(String, String)],
    name: &str,
) -> ResolvedCommand {
    fn go(
        is_builtin: &dyn Fn(&str) -> bool,
        aliases: &[(String, String)],
        seen: &mut Vec<String>,
        name: &str,
    ) -> ResolvedCommand {
        let def = aliases.iter().find(|(a, _)| a == name).map(|(_, d)| d);
        let Some(cmdline) = def.filter(|_| !seen.iter().any(|s| s == name) && !is_builtin(name))
        else {
            return ResolvedCommand::Hledger(name.to_string(), Vec::new());
        };
        let cmdline = cmdline.trim();
        if let Some(shell) = cmdline.strip_prefix('!') {
            return ResolvedCommand::Shell(shell.trim().to_string());
        }
        let words = crate::text::words_quoted(cmdline).unwrap_or_default();
        let Some((real, defargs)) = words.split_first() else {
            return ResolvedCommand::Hledger(name.to_string(), Vec::new());
        };
        seen.push(name.to_string());
        match go(is_builtin, aliases, seen, real) {
            ResolvedCommand::Hledger(c, mut args) => {
                args.extend(defargs.iter().cloned());
                ResolvedCommand::Hledger(c, args)
            }
            ResolvedCommand::Shell(s) => {
                let mut parts = vec![s];
                parts.extend(defargs.iter().cloned());
                ResolvedCommand::Shell(parts.join(" "))
            }
        }
    }
    go(&is_builtin, aliases, &mut Vec::new(), name)
}

/// Which config file the options select, the rightmost --conf or --no-conf
/// counting: Some(Some(file)), Some(None) for none, or None to search
/// (confFileSpecFromRawOpts).
fn conf_file_spec(raw: &crate::reports::opts::RawOpts) -> Option<Option<&str>> {
    raw.0.iter().rev().find_map(|(n, v)| match n.as_str() {
        "conf" => Some(Some(v.as_str())),
        "no-conf" => Some(None),
        _ => None,
    })
}

/// The config file the options select, read and checked, and its path; an
/// empty one with --no-conf or if none is found; or an error if it can not
/// be read or parsed (getConf).
pub fn get_conf(
    raw: &crate::reports::opts::RawOpts,
    debug: bool,
) -> Result<(Conf, Option<String>), String> {
    let dbg = |msg: &str| {
        if debug {
            eprintln!("{msg}");
        }
    };
    match conf_file_spec(raw) {
        Some(None) => {
            dbg("ignoring config files");
            Ok((Conf::default(), None))
        }
        Some(Some(f)) => {
            let f = expand_path(f)?;
            dbg(&format!("using specified config file: {f:?}"));
            read_conf_file(&f)
        }
        None => match conf_files().into_iter().next() {
            Some(f) => {
                dbg(&format!("using nearest config file: {f:?}"));
                read_conf_file(&f)
            }
            None => {
                dbg("no config file found");
                Ok((Conf::default(), None))
            }
        },
    }
}

/// A path relative to the current directory made absolute, after expanding
/// `~` (expandPath).
fn expand_path(p: &str) -> Result<String, String> {
    if p == "-" {
        return Ok(p.to_string());
    }
    let p = crate::read::journal::expand_home_path(p)?;
    if p.starts_with('/') {
        Ok(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        Ok(format!("{}/{p}", cwd.display()))
    }
}

/// A config file read, parsed and checked (readConfFile).
fn read_conf_file(f: &str) -> Result<(Conf, Option<String>), String> {
    if !file_exists(f) {
        return Err(format!("{f} does not exist"));
    }
    let bytes = std::fs::read(f)
        .map_err(|e| format!("{f}: openFile: {}", crate::read::io_error_text(&e)))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let sections = parse_conf(&text).map_err(|e| e.pretty(f, &text))?;
    let conf = Conf {
        file: f.to_string(),
        text,
        sections,
    };
    conf.validate()?;
    Ok((conf, Some(f.to_string())))
}

/// Whether the config file may run shell command aliases: one given with
/// --conf, or a user's own (confFileIsTrusted).
pub fn conf_file_is_trusted(raw: &crate::reports::opts::RawOpts, file: Option<&str>) -> bool {
    match (file, conf_file_spec(raw)) {
        (None, _) => false,
        (Some(_), Some(Some(_))) => true,
        (Some(f), _) => user_conf_files().iter().any(|u| u == f),
    }
}

/// Whether a path exists and is not a directory (doesFileExist).
fn file_exists(f: &str) -> bool {
    let p = std::path::Path::new(f);
    p.exists() && !p.is_dir()
}

/// The config file in a directory: .hledger.conf in the home directory,
/// hledger.conf elsewhere.
fn conf_file_in(dir: &str, home: &str) -> String {
    let name = if dir == home {
        ".hledger.conf"
    } else {
        "hledger.conf"
    };
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The existing config files, nearest first: in the current directory and
/// each parent, the home directory, and the XDG config directory
/// (confFiles, possibleConfFiles, possibleConfDirs).
pub fn conf_files() -> Vec<String> {
    let home = home_dir();
    let xdg = config_dir();
    let mut dirs = Vec::new();
    let mut d = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());
    while dirs.len() < 100 && !dirs.contains(&d) {
        dirs.push(d.clone());
        if d == "/" {
            break;
        }
        d = match d.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(i) => d[..i].to_string(),
        };
    }
    for extra in [home.clone(), xdg] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    (dirs.iter())
        .map(|d| conf_file_in(d, &home))
        .filter(|f| file_exists(f))
        .collect()
}

/// The user's existing config files: ~/.hledger.conf and the XDG one
/// (userConfFiles).
pub fn user_conf_files() -> Vec<String> {
    let home = home_dir();
    [home.clone(), config_dir()]
        .iter()
        .map(|d| conf_file_in(d, &home))
        .filter(|f| file_exists(f))
        .collect()
}

/// A line of the file: its number, its start offset and its text without
/// the newline, and whether a newline ends it.
struct Line<'a> {
    num: usize,
    start: usize,
    text: &'a str,
    newline: bool,
}

/// Parse a config file (confp): blank and `#` comment lines; argument
/// lines of the general section; then sections, each a `[NAME]` line with
/// perhaps an argument after it, and argument lines, or in an [alias]
/// section definitions that may continue on more-indented lines. Leading
/// white space is ignored; `#` starts a comment, except inside quotes.
fn parse_conf(text: &str) -> Result<Sections, ParseError> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        let newline = l.ends_with('\n');
        lines.push(Line {
            num: i + 1,
            start,
            text: l.strip_suffix('\n').unwrap_or(l),
            newline,
        });
        start += l.len();
    }
    let mut sections = vec![("general".to_string(), Vec::new())];
    let mut i = 0;
    let mut name = "general".to_string();
    while i < lines.len() {
        let l = &lines[i];
        let rest = l.text.trim_start_matches(is_blank);
        let indent = l.text.len() - rest.len();
        if rest.is_empty() || rest.starts_with('#') {
            i += 1;
            continue;
        }
        if rest.starts_with('[') {
            let (n, arg) = section_start(l, indent, text)?;
            name = n.clone();
            sections.push((n, arg.into_iter().collect()));
            i += 1;
            continue;
        }
        let arg = arg_text(rest).trim().to_string();
        if name != "alias" {
            sections.last_mut().unwrap().1.push((l.num, arg));
            i += 1;
            continue;
        }
        // an alias definition, continued by following lines indented more
        // than its name, with blank and comment lines among them skipped
        let col = column(&l.text[..indent]);
        let mut parts = vec![arg];
        let mut j = i + 1;
        let mut end = i + 1;
        while j < lines.len() {
            let r = lines[j].text.trim_start_matches(is_blank);
            if r.is_empty() || r.starts_with('#') {
                j += 1;
                continue;
            }
            let ind = lines[j].text.len() - r.len();
            if column(&lines[j].text[..ind]) <= col || r.starts_with('[') {
                break;
            }
            parts.push(arg_text(r).trim().to_string());
            j += 1;
            end = j;
        }
        sections
            .last_mut()
            .unwrap()
            .1
            .push((l.num, parts.join(" ").trim().to_string()));
        i = end;
    }
    Ok(sections)
}

/// A section's start line (sectionstartp): its name, and an argument
/// written after it, if any.
fn section_start(
    l: &Line,
    indent: usize,
    text: &str,
) -> Result<(String, Option<(usize, String)>), ParseError> {
    let after = &l.text[indent + 1..];
    let name_len = after.find([']', '#']).unwrap_or(after.len());
    let at = l.start + indent + 1 + name_len;
    let unexpected = || match text[at..].chars().next() {
        Some(c) => Item::tokens(&c.to_string()),
        None => Item::EndOfInput,
    };
    if name_len == 0 {
        return Err(ParseError::trivial(at, Some(unexpected()), &[]));
    }
    if !after[name_len..].starts_with(']') {
        return Err(ParseError::trivial(
            at,
            Some(unexpected()),
            &[Item::tokens("]")],
        ));
    }
    let name = after[..name_len].trim().to_string();
    let rest = after[name_len + 1..].trim_start_matches(is_blank);
    let arg = arg_text(rest);
    let after_arg = &rest[arg.len()..];
    if !l.newline && !after_arg.starts_with('#') {
        let end = l.start + l.text.len();
        return Err(ParseError::trivial(
            end,
            Some(Item::EndOfInput),
            &[
                Item::tokens("\""),
                Item::tokens("'"),
                Item::label("empty line or comment line beginning with #"),
            ],
        ));
    }
    let arg = arg.trim();
    Ok((name, (!arg.is_empty()).then(|| (l.num, arg.to_string()))))
}

/// The argument text at the start of a line: up to an unquoted `#` or the
/// end of the line; a quote left open runs to the end (argtextp).
fn arg_text(s: &str) -> &str {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') => return &s[..i],
            _ => {}
        }
    }
    s
}

/// A non-newline space character (spacenonewline).
fn is_blank(c: char) -> bool {
    c != '\n' && crate::text::is_space(c)
}

/// The column, from 1, after this leading white space, with tab stops
/// every 8 columns (megaparsec's sourceColumn).
fn column(indent: &str) -> usize {
    indent.chars().fold(1, |col, c| {
        if c == '\t' {
            col + 8 - (col - 1) % 8
        } else {
            col + 1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(text: &str) -> Sections {
        parse_conf(text).unwrap()
    }

    #[test]
    fn general_arguments_sections_and_comments() {
        let s =
            sections("# c\n  --pretty  # same-line\n\n[files]\n\t--pretty\n[bal] -N\n  '#x' # y\n");
        assert_eq!(
            s[0],
            ("general".to_string(), vec![(2, "--pretty".to_string())])
        );
        assert_eq!(
            s[1],
            ("files".to_string(), vec![(5, "--pretty".to_string())])
        );
        assert_eq!(
            s[2],
            (
                "bal".to_string(),
                vec![(6, "-N".to_string()), (7, "'#x'".to_string())]
            )
        );
        assert_eq!(
            sections("  --pretty\n  ")[0].1,
            [(1, "--pretty".to_string())]
        );
    }

    #[test]
    fn aliases_continue_on_more_indented_lines() {
        let s = sections("[alias]\nmybal = balance --tree\n  # --flat\n  -N\nn = accounts\n");
        assert_eq!(
            s[1].1,
            [
                (2, "mybal = balance --tree -N".to_string()),
                (5, "n = accounts".to_string())
            ]
        );
    }

    #[test]
    fn section_header_errors() {
        let e = parse_conf("[cmd\n").unwrap_err();
        assert_eq!(
            e.pretty("f.conf", "[cmd\n"),
            "f.conf:1:5:\n  |\n1 | [cmd\n  |     ^\nunexpected newline\nexpecting ']'\n"
        );
    }

    #[test]
    fn alias_expansion() {
        let aliases = vec![
            ("list2".to_string(), "list1 -N".to_string()),
            ("list1".to_string(), "files --x".to_string()),
            ("hi".to_string(), "!echo hello".to_string()),
            ("files".to_string(), "stats".to_string()),
            ("me".to_string(), "me --extra".to_string()),
        ];
        let builtin = |n: &str| n == "files";
        let x = |n| expand_command_alias(builtin, &aliases, n);
        assert_eq!(
            x("list2"),
            ResolvedCommand::Hledger("files".into(), vec!["--x".into(), "-N".into()])
        );
        assert_eq!(x("hi"), ResolvedCommand::Shell("echo hello".into()));
        assert_eq!(x("files"), ResolvedCommand::Hledger("files".into(), vec![]));
        assert_eq!(
            x("me"),
            ResolvedCommand::Hledger("me".into(), vec!["--extra".into()])
        );
    }
}
