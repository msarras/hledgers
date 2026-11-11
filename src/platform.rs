//! What differs between unix and Windows: the home and config directories,
//! which files are programs and which are in PATH, how a shell command line
//! is run, and the signal that ended a child. Everything else in hledgers
//! is plain std and builds anywhere. Also file paths: `~` expanded, made
//! absolute or canonical, and glob patterns matched.

use std::path::{Path, PathBuf};

/// The user's home directory (getHomeDirectory): HOME, or USERPROFILE on
/// Windows when HOME is unset.
pub fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|e| {
            if cfg!(windows) {
                std::env::var("USERPROFILE")
            } else {
                Err(e)
            }
        })
        .unwrap_or_default()
}

/// hledger's directory in the user's XDG config directory:
/// $XDG_CONFIG_HOME/hledger, or ~/.config/hledger (getXdgDirectory
/// XdgConfig "hledger").
pub fn config_dir() -> String {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|d| d.starts_with('/'))
        .unwrap_or_else(|| format!("{}/.config", home_dir()));
    format!("{base}/hledger")
}

/// The executables of this name in PATH, in order (findExecutables).
pub fn find_executables(name: &str) -> Vec<String> {
    if name.contains(std::path::is_separator) {
        return if is_executable(std::path::Path::new(name)) {
            vec![name.to_string()]
        } else {
            Vec::new()
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .filter(|f| is_executable(f))
        .map(|f| f.display().to_string())
        .collect()
}

/// Whether a path is a program that can be run (executable): on unix a
/// regular file with an execute bit, on Windows a regular file with a
/// program extension, or the path with `.exe` added.
#[cfg(unix)]
pub fn is_executable(f: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    f.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(f: &std::path::Path) -> bool {
    match f.extension().and_then(|e| e.to_str()) {
        Some(e) => {
            ["exe", "bat", "cmd", "com"].contains(&e.to_ascii_lowercase().as_str()) && f.is_file()
        }
        None => f.with_extension("exe").is_file(),
    }
}

/// A command that runs a shell command line (shell): `/bin/sh -c` on unix,
/// `cmd /C` on Windows.
pub fn shell_command(cmd: &str) -> std::process::Command {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    let mut command = std::process::Command::new(shell);
    command.arg(flag).arg(cmd);
    command
}

/// The signal that ended a child process, if one did; never on Windows.
#[cfg(unix)]
pub fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
pub fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// Options that create or empty a file only its owner may read (on unix;
/// Windows files inherit their directory's permissions).
pub fn private_file_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// A path with a leading `~` replaced by the home directory
/// (expandHomePath).
pub fn expand_home_path(p: &str) -> Result<String, String> {
    let home = crate::platform::home_dir;
    if p == "~" {
        Ok(home())
    } else if let Some(rest) = p.strip_prefix("~/") {
        Ok(format!("{}/{rest}", home()))
    } else if p.starts_with('~') {
        Err("~USERNAME in paths is not supported".to_string())
    } else {
        Ok(p.to_string())
    }
}

/// The path made absolute by prepending the current directory, if it is
/// relative (makeAbsolute).
pub fn absolute_path(p: &str) -> String {
    if p.starts_with('/') {
        normalise_path(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        normalise_path(&format!("{}/{p}", cwd.display()))
    }
}

/// The path without `.` components or repeated slashes (normalise).
pub fn normalise_path(p: &str) -> String {
    let absolute = p.starts_with('/');
    let parts: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The path with symbolic links and `..` resolved, as far as it exists
/// (canonicalizePath).
pub fn canonical_path(p: &str) -> PathBuf {
    let path = Path::new(p);
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => {
            canonical_path(&dir.to_string_lossy()).join(name)
        }
        _ => path.to_path_buf(),
    }
}

/// The files and directories a glob pattern matches, relative to a
/// directory, sorted (for -f's patterns).
pub fn glob_matches(dir: &str, pattern: &str) -> Vec<String> {
    match Glob::compile(pattern) {
        Ok(g) => {
            let mut found: Vec<String> = g
                .matches(Path::new(dir))
                .into_iter()
                .filter(|f| Path::new(f).exists())
                .collect();
            found.sort();
            found
        }
        Err(_) => Vec::new(),
    }
}

/// `**` not followed by a slash made `**/*`, so that it also matches file
/// name parts.
pub fn expand_double_stars(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 4);
    let b = p.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if p[i..].starts_with("**") && b.get(i + 2).is_some_and(|&c| c != b'/' && c != b'\\') {
            out.push_str("**/*");
            i += 2;
        } else {
            let ch = p[i..].chars().next().unwrap_or_default();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// A compiled glob pattern (the parts of System.FilePath.Glob that include
/// uses): path components of literal text, `*`, `?` and `[...]` classes,
/// and `**/` matching any number of directories.
pub struct Glob {
    absolute: bool,
    components: Vec<GlobComponent>,
}

enum GlobComponent {
    /// `**`: any number of directories.
    AnyDirectories,
    Pattern(Vec<GlobToken>),
}

#[derive(Clone)]
enum GlobToken {
    Char(char),
    Any,
    One,
    Class(bool, Vec<(char, char)>),
}

impl Glob {
    pub fn compile(pattern: &str) -> Result<Glob, String> {
        let absolute = pattern.starts_with('/');
        let mut components = Vec::new();
        for comp in pattern.split('/').filter(|s| !s.is_empty()) {
            if comp == "**" {
                components.push(GlobComponent::AnyDirectories);
                continue;
            }
            let mut tokens = Vec::new();
            let mut chars = comp.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    '*' => {
                        while chars.peek() == Some(&'*') {
                            chars.next();
                        }
                        tokens.push(GlobToken::Any);
                    }
                    '?' => tokens.push(GlobToken::One),
                    '[' => {
                        let negated = matches!(chars.peek(), Some('!' | '^'));
                        if negated {
                            chars.next();
                        }
                        let mut ranges = Vec::new();
                        let mut first = true;
                        loop {
                            match chars.next() {
                                None => return Err("compile :: unclosed [] in pattern".to_string()),
                                Some(']') if !first => break,
                                Some(a) => {
                                    if chars.peek() == Some(&'-') {
                                        let mut look = chars.clone();
                                        look.next();
                                        match look.peek() {
                                            Some(&b) if b != ']' => {
                                                chars.next();
                                                chars.next();
                                                ranges.push((a, b));
                                            }
                                            _ => ranges.push((a, a)),
                                        }
                                    } else {
                                        ranges.push((a, a));
                                    }
                                }
                            }
                            first = false;
                        }
                        tokens.push(GlobToken::Class(negated, ranges));
                    }
                    c => tokens.push(GlobToken::Char(c)),
                }
            }
            components.push(GlobComponent::Pattern(tokens));
        }
        Ok(Glob {
            absolute,
            components,
        })
    }

    pub fn is_literal(&self) -> bool {
        self.components.iter().all(|c| match c {
            GlobComponent::AnyDirectories => false,
            GlobComponent::Pattern(ts) => ts.iter().all(|t| matches!(t, GlobToken::Char(_))),
        })
    }

    /// The paths matching this pattern, relative to `dir` if it is relative
    /// (globDir1). Wildcards do not match a name's leading dot, and `**`
    /// does not enter the dot directories directly below where it starts,
    /// as with upstream's Glob library.
    pub fn matches(&self, dir: &Path) -> Vec<String> {
        let base = if self.absolute {
            String::new()
        } else {
            dir.display().to_string()
        };
        let mut out = Vec::new();
        self.walk(&base, 0, &mut out);
        out
    }

    fn walk(&self, path: &str, i: usize, out: &mut Vec<String>) {
        let Some(comp) = self.components.get(i) else {
            out.push(if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            });
            return;
        };
        match comp {
            GlobComponent::AnyDirectories => {
                self.walk(path, i + 1, out);
                self.walk_dirs(path, i + 1, true, out);
            }
            GlobComponent::Pattern(tokens) => {
                if tokens.iter().all(|t| matches!(t, GlobToken::Char(_))) {
                    let name: String = tokens
                        .iter()
                        .map(|t| match t {
                            GlobToken::Char(c) => *c,
                            _ => unreachable!(),
                        })
                        .collect();
                    self.walk(&format!("{path}/{name}"), i + 1, out);
                    return;
                }
                for name in dir_entries(path) {
                    if glob_tokens_match(tokens, &name) {
                        self.walk(&format!("{path}/{name}"), i + 1, out);
                    }
                }
            }
        }
    }

    /// The directories below `path`, for `**`, at any depth, each followed
    /// by the rest of the pattern.
    fn walk_dirs(&self, path: &str, rest: usize, first_level: bool, out: &mut Vec<String>) {
        for name in dir_entries(path) {
            if first_level && name.starts_with('.') {
                continue;
            }
            let sub = format!("{path}/{name}");
            if Path::new(&sub).is_dir() {
                self.walk(&sub, rest, out);
                self.walk_dirs(&sub, rest, false, out);
            }
        }
    }
}

/// The names in a directory, sorted (none if it can't be read).
fn dir_entries(path: &str) -> Vec<String> {
    let dir = if path.is_empty() { "/" } else { path };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether a file name matches a glob component; a leading dot must be
/// matched literally.
fn glob_tokens_match(tokens: &[GlobToken], name: &str) -> bool {
    if name.starts_with('.') && !matches!(tokens.first(), Some(GlobToken::Char('.'))) {
        return false;
    }
    fn go(tokens: &[GlobToken], s: &[char]) -> bool {
        match tokens.split_first() {
            None => s.is_empty(),
            Some((GlobToken::Any, rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some((t, rest)) => match s.split_first() {
                None => false,
                Some((c, srest)) => {
                    let ok = match t {
                        GlobToken::Char(x) => x == c,
                        GlobToken::One => true,
                        GlobToken::Class(neg, ranges) => {
                            ranges.iter().any(|(a, b)| a <= c && c <= b) != *neg
                        }
                        GlobToken::Any => unreachable!(),
                    };
                    ok && go(rest, srest)
                }
            },
        }
    }
    let chars: Vec<char> = name.chars().collect();
    go(tokens, &chars)
}
