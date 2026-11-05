//! The built-in commands that only read and write data: the text tools and file
//! operations the corpus pipes `hledger` through. Each has a parser for exactly
//! the options the corpus uses; any other option makes the case unsupported.

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::PermissionsExt;
use std::time::SystemTime;

use regex::bytes::Regex;

use super::exec::{Flow, Io, R, Shell};
use super::posix_re::{Flavor, translate};

/// Check that a built-in exists and accepts these arguments. The built-ins that
/// need the shell itself (cd, exit, export, sh, ./csvtest.sh) are in exec.rs.
pub fn validate(name: &str, args: &[String]) -> Result<(), String> {
    match name {
        "printf" => printf(args, &mut Vec::new()).map(drop),
        "echo" | "true" | "false" => Ok(()),
        "pwd" | "uniq" if args.is_empty() => Ok(()),
        "pwd" | "uniq" => Err(format!("{name} with arguments")),
        "touch" => operands(name, args).map(drop),
        "cat" => cat_files(args).map(drop),
        "head" => head_opts(args).map(drop),
        "tail" => tail_opts(args).map(drop),
        "grep" => Grep::parse(args).map(drop),
        "sed" => Sed::parse(args).map(drop),
        "tr" => Tr::parse(args).map(drop),
        "sort" => sort_opts(args).map(drop),
        "diff" => diff_file(args).map(drop),
        "cmp" | "cp" => two_operands(name, args).map(drop),
        "rm" => rm_opts(args).map(drop),
        "mkdir" => mkdir_opts(args).map(drop),
        "chmod" => chmod_opts(args).map(drop),
        "test" => test_path(args).map(drop),
        "mktemp" => mktemp_opts(args).map(drop),
        "ls" => ls_operand(args).map(drop),
        _ => Err(format!("command `{name}`")),
    }
}

pub fn run(sh: &mut Shell, name: &str, args: &[String], io: &Io) -> R<i32> {
    validate(name, args).map_err(Flow::Unsupported)?;
    let status = match name {
        "printf" => {
            let mut out = Vec::new();
            printf(args, &mut out).map_err(Flow::Unsupported)?;
            io.out.write(&out);
            0
        }
        "echo" => {
            io.out.write(format!("{}\n", args.join(" ")).as_bytes());
            0
        }
        "true" => 0,
        "false" => 1,
        "pwd" => {
            io.out
                .write(format!("{}\n", sh.cwd.to_string_lossy()).as_bytes());
            0
        }
        "cat" => {
            let mut status = 0;
            for f in cat_files(args).unwrap() {
                match read_input(sh, io, name, Some(f)) {
                    Ok(data) => io.out.write(&data),
                    Err(()) => status = 1,
                }
            }
            status
        }
        "head" => {
            let (n, file) = head_opts(args).unwrap();
            with_input(sh, io, name, file, |data| {
                lines(data).take(n).flatten().copied().collect()
            })
        }
        "tail" => {
            let (from, file) = tail_opts(args).unwrap();
            with_input(sh, io, name, file, |data| {
                lines(data)
                    .skip(from.saturating_sub(1))
                    .flatten()
                    .copied()
                    .collect()
            })
        }
        "grep" => Grep::parse(args).unwrap().run(sh, io),
        "sed" => {
            let sed = Sed::parse(args).unwrap();
            with_input(sh, io, name, sed.file.as_deref(), |data| sed.apply(data))
        }
        "tr" => {
            let tr = Tr::parse(args).unwrap();
            let data = io.stdin.take();
            io.out.write(&tr.apply(&data));
            0
        }
        "uniq" => {
            let data = io.stdin.take();
            let mut ls: Vec<&[u8]> = lines(&data).map(chomp).collect();
            ls.dedup();
            io.out.write(&unlines(&ls));
            0
        }
        "sort" => {
            let (unique, file) = sort_opts(args).unwrap();
            with_input(sh, io, name, file, |data| {
                let mut ls: Vec<&[u8]> = lines(data).map(chomp).collect();
                ls.sort();
                if unique {
                    ls.dedup();
                }
                unlines(&ls)
            })
        }
        "diff" => {
            let file = diff_file(args).unwrap();
            let left = io.stdin.take();
            match read_input(sh, io, name, Some(file)) {
                Err(()) => 2,
                Ok(right) if left == right => 0,
                Ok(right) => {
                    let (l, r) = (
                        String::from_utf8_lossy(&left),
                        String::from_utf8_lossy(&right),
                    );
                    let diff = similar::TextDiff::from_lines(&l, &r);
                    io.out
                        .write(diff.unified_diff().header("-", file).to_string().as_bytes());
                    1
                }
            }
        }
        "cmp" => {
            let (a, b) = two_operands(name, args).unwrap();
            match (
                read_input(sh, io, name, Some(a)),
                read_input(sh, io, name, Some(b)),
            ) {
                (Ok(x), Ok(y)) if x == y => 0,
                (Ok(x), Ok(y)) => {
                    let byte = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
                    let line = 1 + x[..byte].iter().filter(|&&c| c == b'\n').count();
                    io.out.write(
                        format!("{a} {b} differ: byte {}, line {line}\n", byte + 1).as_bytes(),
                    );
                    1
                }
                _ => 2,
            }
        }
        "rm" => {
            let (force, recursive, paths) = rm_opts(args).unwrap();
            let mut status = 0;
            for p in paths {
                let path = sh.path(p);
                let result = match fs::symlink_metadata(&path) {
                    Err(_) if force => Ok(()),
                    Ok(m) if m.is_dir() && recursive => fs::remove_dir_all(&path),
                    Ok(m) if m.is_dir() => Err(std::io::Error::other("Is a directory")),
                    _ => fs::remove_file(&path),
                };
                status |= report(io, "rm", &format!("cannot remove '{p}'"), result);
            }
            status
        }
        "mkdir" => {
            let (parents, dirs) = mkdir_opts(args).unwrap();
            let mut status = 0;
            for d in dirs {
                let result = if parents {
                    fs::create_dir_all(sh.path(d))
                } else {
                    fs::create_dir(sh.path(d))
                };
                status |= report(
                    io,
                    "mkdir",
                    &format!("cannot create directory '{d}'"),
                    result,
                );
            }
            status
        }
        "touch" => {
            let mut status = 0;
            for f in args {
                let result = fs::File::options()
                    .create(true)
                    .append(true)
                    .open(sh.path(f))
                    .and_then(|file| file.set_modified(SystemTime::now()));
                status |= report(io, "touch", &format!("cannot touch '{f}'"), result);
            }
            status
        }
        "cp" => {
            let (src, dst) = two_operands(name, args).unwrap();
            let mut to = sh.path(dst);
            if to.is_dir() {
                to.push(std::path::Path::new(src).file_name().unwrap_or_default());
            }
            report(
                io,
                "cp",
                &format!("cannot copy '{src}'"),
                fs::copy(sh.path(src), to).map(drop),
            )
        }
        "chmod" => {
            let (mode, files) = chmod_opts(args).unwrap();
            let mut status = 0;
            for f in files {
                let path = sh.path(f);
                let result = fs::metadata(&path).and_then(|m| {
                    let bits = mode.apply(m.permissions().mode() & 0o7777);
                    fs::set_permissions(&path, fs::Permissions::from_mode(bits))
                });
                status |= report(io, "chmod", &format!("cannot access '{f}'"), result);
            }
            status
        }
        "test" => i32::from(!sh.path(test_path(args).unwrap()).exists()),
        "mktemp" => {
            let (dir, template) = mktemp_opts(args).unwrap();
            let template = match template {
                Some(t) => t.to_string(),
                None => format!(
                    "{}/tmp.XXXXXXXXXX",
                    sh.env
                        .get("TMPDIR")
                        .filter(|t| !t.is_empty())
                        .map_or("/tmp", |t| t.as_str())
                ),
            };
            let stem = template.trim_end_matches('X');
            let xs = template.len() - stem.len();
            let created = (0..100).find_map(|_| {
                let mut seed = RandomState::new().hash_one(SystemTime::now());
                let suffix: String = (0..xs)
                    .map(|_| {
                        let c = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                            [(seed % 62) as usize];
                        seed /= 62;
                        c as char
                    })
                    .collect();
                let name = format!("{stem}{suffix}");
                let path = sh.path(&name);
                let ok = if dir {
                    fs::create_dir(&path).is_ok()
                } else {
                    fs::File::create_new(&path).is_ok()
                };
                ok.then_some(name)
            });
            match created {
                Some(name) => {
                    io.out.write(format!("{name}\n").as_bytes());
                    0
                }
                None => {
                    io.err.write(
                        format!("mktemp: failed to create from template '{template}'\n").as_bytes(),
                    );
                    1
                }
            }
        }
        "ls" => {
            let operand = ls_operand(args).unwrap();
            let path = sh.path(operand.unwrap_or("."));
            match fs::metadata(&path) {
                Err(e) => {
                    let name = operand.unwrap_or(".");
                    io.err.write(
                        format!("ls: cannot access '{name}': {}\n", os_error(&e)).as_bytes(),
                    );
                    2
                }
                Ok(m) if !m.is_dir() => {
                    io.out
                        .write(format!("{}\n", operand.unwrap_or(".")).as_bytes());
                    0
                }
                Ok(_) => match fs::read_dir(&path) {
                    Err(e) => {
                        let name = operand.unwrap_or(".");
                        io.err.write(
                            format!("ls: cannot open directory '{name}': {}\n", os_error(&e))
                                .as_bytes(),
                        );
                        2
                    }
                    Ok(entries) => {
                        let mut names: Vec<Vec<u8>> = entries
                            .flatten()
                            .map(|e| e.file_name().into_encoded_bytes())
                            .filter(|n| !n.starts_with(b"."))
                            .collect();
                        names.sort();
                        for n in names {
                            io.out.write(&n);
                            io.out.write(b"\n");
                        }
                        0
                    }
                },
            }
        }
        _ => unreachable!("validated above"),
    };
    Ok(status)
}

/// `ls [DIR]`: the names in a directory (the current one by default), except
/// hidden ones, one per line in byte order, as GNU ls writes them to a pipe
/// under LC_COLLATE=C; a file operand is listed as itself.
fn ls_operand(args: &[String]) -> Result<Option<&str>, String> {
    match operands("ls", args)?[..] {
        [] => Ok(None),
        [d] => Ok(Some(d)),
        _ => Err("ls with several operands".to_string()),
    }
}

/// Print a tool's error message for a failed file operation; returns the status.
fn report(io: &Io, tool: &str, what: &str, result: std::io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            io.err
                .write(format!("{tool}: {what}: {}\n", os_error(&e)).as_bytes());
            1
        }
    }
}

/// An I/O error as the C library describes it, without Rust's ` (os error N)`.
fn os_error(e: &std::io::Error) -> String {
    let s = e.to_string();
    s.split(" (os error ").next().unwrap_or(&s).to_string()
}

/// Read a file operand, or stdin for `-` or none.
fn read_input(sh: &Shell, io: &Io, tool: &str, file: Option<&str>) -> Result<Vec<u8>, ()> {
    match file {
        None | Some("-") => Ok(io.stdin.take()),
        Some(f) => fs::read(sh.path(f)).map_err(|e| {
            io.err
                .write(format!("{tool}: {f}: {}\n", os_error(&e)).as_bytes());
        }),
    }
}

/// Filter one input (file or stdin) to stdout.
fn with_input(
    sh: &Shell,
    io: &Io,
    tool: &str,
    file: Option<&str>,
    f: impl FnOnce(&[u8]) -> Vec<u8>,
) -> i32 {
    match read_input(sh, io, tool, file) {
        Ok(data) => {
            io.out.write(&f(&data));
            0
        }
        Err(()) => 2,
    }
}

/// Lines with their newlines; a last line without one is still a line.
fn lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split_inclusive(|&b| b == b'\n')
}

fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn unlines(lines: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in lines {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out
}

fn operands<'a>(name: &str, args: &'a [String]) -> Result<Vec<&'a str>, String> {
    args.iter()
        .map(|a| match a.as_str() {
            "-" => Ok("-"),
            a if a.starts_with('-') => Err(format!("{name} {a}")),
            a => Ok(a),
        })
        .collect()
}

fn two_operands<'a>(name: &str, args: &'a [String]) -> Result<(&'a str, &'a str), String> {
    match operands(name, args)?[..] {
        [a, b] => Ok((a, b)),
        _ => Err(format!("{name} without exactly two operands")),
    }
}

fn at_most_one_file<'a>(name: &str, files: &[&'a str]) -> Result<Option<&'a str>, String> {
    match files {
        [] => Ok(None),
        [f] => Ok(Some(f)),
        _ => Err(format!("{name} with several files")),
    }
}

/// `printf FORMAT [ARG...]`, supporting `\n \t \r \\ %% %s` and `--`.
/// The format is reused while arguments remain, as POSIX printf does.
fn printf(args: &[String], out: &mut Vec<u8>) -> Result<(), String> {
    let args = match args.first().map(String::as_str) {
        Some("--") => &args[1..],
        _ => args,
    };
    let Some((format, args)) = args.split_first() else {
        return Err("printf without a format".into());
    };
    let mut next = 0;
    loop {
        let before = next;
        let mut cs = format.chars();
        while let Some(c) = cs.next() {
            match c {
                '\\' => match cs.next() {
                    Some('n') => out.push(b'\n'),
                    Some('t') => out.push(b'\t'),
                    Some('r') => out.push(b'\r'),
                    Some('\\') => out.push(b'\\'),
                    Some(c) => out.extend(format!("\\{c}").bytes()),
                    None => out.push(b'\\'),
                },
                '%' => match cs.next() {
                    Some('%') => out.push(b'%'),
                    Some('s') => {
                        out.extend(args.get(next).map_or("", |a| a.as_str()).bytes());
                        next += 1;
                    }
                    c => return Err(format!("printf %{}", c.unwrap_or(' '))),
                },
                c => out.extend(c.to_string().bytes()),
            }
        }
        if next == before || next >= args.len() {
            return Ok(());
        }
    }
}

fn cat_files(args: &[String]) -> Result<Vec<&str>, String> {
    let files = operands("cat", args)?;
    Ok(if files.is_empty() { vec!["-"] } else { files })
}

/// `head [-N | -n N] [FILE]`
fn head_opts(args: &[String]) -> Result<(usize, Option<&str>), String> {
    let (n, rest) = match args {
        [n, count, rest @ ..] if n == "-n" => (count.as_str(), rest),
        [a, rest @ ..] if a.starts_with("-n") => (&a[2..], rest),
        [a, rest @ ..] if a.starts_with('-') && a.len() > 1 => (&a[1..], rest),
        _ => ("10", args),
    };
    let n = n.parse().map_err(|_| format!("head -n {n}"))?;
    Ok((n, at_most_one_file("head", &operands("head", rest)?)?))
}

/// `tail -n +N [FILE]`
fn tail_opts(args: &[String]) -> Result<(usize, Option<&str>), String> {
    let (n, rest) = match args {
        [n, count, rest @ ..] if n == "-n" => (count.as_str(), rest),
        [a, rest @ ..] if a.starts_with("-n") => (&a[2..], rest),
        _ => return Err("tail without -n +N".into()),
    };
    let from = n
        .strip_prefix('+')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("tail -n {n}"))?;
    Ok((from, at_most_one_file("tail", &operands("tail", rest)?)?))
}

fn sort_opts(args: &[String]) -> Result<(bool, Option<&str>), String> {
    let unique = args.first().is_some_and(|a| a == "-u");
    let files = operands("sort", &args[usize::from(unique)..])?;
    Ok((unique, at_most_one_file("sort", &files)?))
}

/// `diff - FILE`
fn diff_file(args: &[String]) -> Result<&str, String> {
    match args {
        [dash, file] if dash == "-" && !file.starts_with('-') => Ok(file),
        _ => Err("diff other than `diff - FILE`".into()),
    }
}

/// `rm [-f] [-r] PATH...`
fn rm_opts(args: &[String]) -> Result<(bool, bool, Vec<&str>), String> {
    let (mut force, mut recursive) = (false, false);
    let mut paths = Vec::new();
    for a in args {
        match a.strip_prefix('-') {
            Some(flags) if !flags.is_empty() => {
                for c in flags.chars() {
                    match c {
                        'f' => force = true,
                        'r' | 'R' => recursive = true,
                        _ => return Err(format!("rm -{c}")),
                    }
                }
            }
            _ => paths.push(a.as_str()),
        }
    }
    Ok((force, recursive, paths))
}

fn mkdir_opts(args: &[String]) -> Result<(bool, Vec<&str>), String> {
    let parents = args.first().is_some_and(|a| a == "-p");
    Ok((parents, operands("mkdir", &args[usize::from(parents)..])?))
}

/// `test -e PATH`
fn test_path(args: &[String]) -> Result<&str, String> {
    match args {
        [e, path] if e == "-e" => Ok(path),
        _ => Err("test other than `test -e PATH`".into()),
    }
}

fn mktemp_opts(args: &[String]) -> Result<(bool, Option<&str>), String> {
    let dir = args.first().is_some_and(|a| a == "-d");
    let template = at_most_one_file("mktemp", &operands("mktemp", &args[usize::from(dir)..])?)?;
    if template.is_some_and(|t| !t.ends_with("XXX")) {
        return Err("mktemp template without XXX".into());
    }
    Ok((dir, template))
}

/// A chmod mode: octal, or symbolic clauses like `+x`, `u-w`, `go=r`.
enum Mode {
    Octal(u32),
    Symbolic(Vec<(u32, char, u32)>),
}

impl Mode {
    fn apply(&self, current: u32) -> u32 {
        match self {
            Mode::Octal(m) => *m,
            Mode::Symbolic(clauses) => clauses.iter().fold(current, |m, &(who, op, perms)| {
                let bits = who & perms;
                match op {
                    '+' => m | bits,
                    '-' => m & !bits,
                    _ => (m & !who) | bits,
                }
            }),
        }
    }
}

/// `chmod MODE FILE...`. Symbolic modes with no who-letters apply to all
/// classes, ignoring the umask (the same result under the usual umask 022).
fn chmod_opts(args: &[String]) -> Result<(Mode, Vec<&str>), String> {
    let Some((mode, files)) = args.split_first() else {
        return Err("chmod without a mode".into());
    };
    let bad = || format!("chmod mode {mode}");
    let mode = if mode.chars().all(|c| c.is_digit(8)) {
        Mode::Octal(u32::from_str_radix(mode, 8).map_err(|_| bad())?)
    } else {
        let mut clauses = Vec::new();
        for clause in mode.split(',') {
            let op_at = clause.find(['+', '-', '=']).ok_or_else(bad)?;
            let who = clause[..op_at].chars().try_fold(0, |w, c| match c {
                'u' => Ok(w | 0o4700),
                'g' => Ok(w | 0o2070),
                'o' => Ok(w | 0o1007),
                'a' => Ok(w | 0o7777),
                _ => Err(bad()),
            })?;
            let who = if who == 0 { 0o7777 } else { who };
            let op = clause[op_at..].chars().next().unwrap();
            let perms = clause[op_at + 1..].chars().try_fold(0, |p, c| match c {
                'r' => Ok(p | 0o444),
                'w' => Ok(p | 0o222),
                'x' => Ok(p | 0o111),
                _ => Err(bad()),
            })?;
            clauses.push((who, op, perms));
        }
        Mode::Symbolic(clauses)
    };
    Ok((mode, operands("chmod", files)?))
}

/// `grep [-c] [-o] [-v] [-E|-F] [-A N] [-e PATTERN]... [PATTERN] [FILE]`
struct Grep {
    count: bool,
    only: bool,
    invert: bool,
    after: usize,
    re: Regex,
    file: Option<String>,
}

impl Grep {
    fn parse(args: &[String]) -> Result<Grep, String> {
        let (mut count, mut only, mut invert, mut after) = (false, false, false, 0);
        let mut flavor = Flavor::Basic;
        let mut fixed = false;
        let mut patterns = Vec::new();
        let mut operands = Vec::new();
        let mut args = args.iter();
        while let Some(a) = args.next() {
            let Some(flags) = a.strip_prefix('-').filter(|f| !f.is_empty()) else {
                operands.push(a.clone());
                continue;
            };
            for (i, c) in flags.char_indices() {
                let rest = &flags[i + 1..];
                let mut value = || match rest {
                    "" => args
                        .next()
                        .cloned()
                        .ok_or(format!("grep -{c} without a value")),
                    r => Ok(r.to_string()),
                };
                match c {
                    'c' => count = true,
                    'o' => only = true,
                    'v' => invert = true,
                    'E' => flavor = Flavor::Extended,
                    'F' => fixed = true,
                    'A' => {
                        let v = value()?;
                        after = v.parse().map_err(|_| format!("grep -A {v}"))?;
                        break;
                    }
                    'e' => {
                        patterns.push(value()?);
                        break;
                    }
                    _ => return Err(format!("grep -{c}")),
                }
            }
        }
        if patterns.is_empty() {
            if operands.is_empty() {
                return Err("grep without a pattern".into());
            }
            patterns.push(operands.remove(0));
        }
        let file = at_most_one_file(
            "grep",
            &operands.iter().map(String::as_str).collect::<Vec<_>>(),
        )?
        .map(str::to_string);
        let alternatives: Vec<String> = patterns
            .iter()
            .map(|p| {
                let re = if fixed {
                    regex::escape(p)
                } else {
                    translate(p, flavor)
                };
                format!("(?:{re})")
            })
            .collect();
        let re = Regex::new(&alternatives.join("|")).map_err(|e| format!("grep pattern: {e}"))?;
        Ok(Grep {
            count,
            only,
            invert,
            after,
            re,
            file,
        })
    }

    fn run(&self, sh: &Shell, io: &Io) -> i32 {
        let Ok(data) = read_input(sh, io, "grep", self.file.as_deref()) else {
            return 2;
        };
        let ls: Vec<&[u8]> = lines(&data).map(chomp).collect();
        let selected: Vec<bool> = ls
            .iter()
            .map(|l| self.re.is_match(l) != self.invert)
            .collect();
        let n = selected.iter().filter(|&&s| s).count();
        let mut out = Vec::new();
        if self.count {
            out.extend(format!("{n}\n").bytes());
        } else if self.only {
            for (l, _) in ls
                .iter()
                .zip(&selected)
                .filter(|(_, s)| **s && !self.invert)
            {
                for m in self.re.find_iter(l).filter(|m| !m.is_empty()) {
                    out.extend_from_slice(m.as_bytes());
                    out.push(b'\n');
                }
            }
        } else {
            // Selected lines, each followed by up to `after` lines of context,
            // with `--` between groups that are not adjacent.
            let mut last_printed: Option<usize> = None;
            let mut context_left = 0;
            for (i, l) in ls.iter().enumerate() {
                if selected[i] {
                    context_left = self.after;
                } else if context_left > 0 {
                    context_left -= 1;
                } else {
                    continue;
                }
                if self.after > 0 && last_printed.is_some_and(|p| i > p + 1) {
                    out.extend_from_slice(b"--\n");
                }
                out.extend_from_slice(l);
                out.push(b'\n');
                last_printed = Some(i);
            }
        }
        io.out.write(&out);
        i32::from(n == 0)
    }
}

/// `sed [-n] [-E] [-e] SCRIPT [FILE]`, where SCRIPT is `s` commands only.
struct Sed {
    quiet: bool,
    commands: Vec<Subst>,
    file: Option<String>,
}

struct Subst {
    re: Regex,
    replacement: Vec<Piece>,
    global: bool,
    print: bool,
}

enum Piece {
    Text(Vec<u8>),
    Group(usize),
}

impl Sed {
    fn parse(args: &[String]) -> Result<Sed, String> {
        let mut quiet = false;
        let mut flavor = Flavor::Basic;
        let mut scripts = Vec::new();
        let mut operands = Vec::new();
        let mut args = args.iter();
        while let Some(a) = args.next() {
            match a.as_str() {
                "-n" => quiet = true,
                "-E" | "-r" => flavor = Flavor::Extended,
                "-e" => scripts.push(args.next().ok_or("sed -e without a script")?.clone()),
                "-nE" | "-En" => (quiet, flavor) = (true, Flavor::Extended),
                o if o.starts_with('-') && o.len() > 1 => return Err(format!("sed {o}")),
                _ => operands.push(a.clone()),
            }
        }
        if scripts.is_empty() {
            if operands.is_empty() {
                return Err("sed without a script".into());
            }
            scripts.push(operands.remove(0));
        }
        let file = at_most_one_file(
            "sed",
            &operands.iter().map(String::as_str).collect::<Vec<_>>(),
        )?
        .map(str::to_string);
        let mut commands = Vec::new();
        let script: Vec<char> = scripts.join("\n").chars().collect();
        let mut i = 0;
        loop {
            while i < script.len() && (script[i].is_whitespace() || script[i] == ';') {
                i += 1;
            }
            if i == script.len() {
                break;
            }
            if script[i] != 's' || i + 1 == script.len() {
                return Err("sed command other than s".into());
            }
            let delim = script[i + 1];
            i += 2;
            let mut field = || -> Result<String, String> {
                let mut s = String::new();
                loop {
                    match script.get(i) {
                        None => return Err("unterminated sed s command".into()),
                        Some(&c) if c == delim => {
                            i += 1;
                            return Ok(s);
                        }
                        Some('\\') if script.get(i + 1) == Some(&delim) => {
                            s.push(delim);
                            i += 2;
                        }
                        Some('\\') if i + 1 < script.len() => {
                            s.push('\\');
                            s.push(script[i + 1]);
                            i += 2;
                        }
                        Some(&c) => {
                            s.push(c);
                            i += 1;
                        }
                    }
                }
            };
            let re = field()?;
            let replacement = field()?;
            if re.is_empty() {
                return Err("sed s with an empty regex".into());
            }
            let (mut global, mut print) = (false, false);
            while let Some(&c) = script.get(i) {
                match c {
                    'g' => global = true,
                    'p' => print = true,
                    ';' | '\n' | ' ' | '\t' => break,
                    c => return Err(format!("sed s flag {c}")),
                }
                i += 1;
            }
            let re = Regex::new(&translate(&re, flavor)).map_err(|e| format!("sed regex: {e}"))?;
            commands.push(Subst {
                re,
                replacement: parse_replacement(&replacement),
                global,
                print,
            });
        }
        Ok(Sed {
            quiet,
            commands,
            file,
        })
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines(data) {
            let newline = line.ends_with(b"\n");
            let mut space = chomp(line).to_vec();
            for cmd in &self.commands {
                let mut result = Vec::new();
                let mut last = 0;
                let mut replaced = false;
                for caps in cmd.re.captures_iter(&space) {
                    let m = caps.get(0).unwrap();
                    result.extend_from_slice(&space[last..m.start()]);
                    for piece in &cmd.replacement {
                        match piece {
                            Piece::Text(t) => result.extend_from_slice(t),
                            Piece::Group(g) => result
                                .extend_from_slice(caps.get(*g).map_or(&b""[..], |m| m.as_bytes())),
                        }
                    }
                    last = m.end();
                    replaced = true;
                    if !cmd.global {
                        break;
                    }
                }
                if replaced {
                    result.extend_from_slice(&space[last..]);
                    space = result;
                    if cmd.print {
                        out.extend_from_slice(&space);
                        out.push(b'\n');
                    }
                }
            }
            if !self.quiet {
                out.extend_from_slice(&space);
                if newline {
                    out.push(b'\n');
                }
            }
        }
        out
    }
}

/// A sed replacement: `\N` and `&` refer to groups, `\n` is a newline.
fn parse_replacement(s: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = Vec::new();
    let mut cs = s.chars();
    let flush = |text: &mut Vec<u8>, pieces: &mut Vec<Piece>| {
        if !text.is_empty() {
            pieces.push(Piece::Text(std::mem::take(text)));
        }
    };
    while let Some(c) = cs.next() {
        match c {
            '&' => {
                flush(&mut text, &mut pieces);
                pieces.push(Piece::Group(0));
            }
            '\\' => match cs.next() {
                Some(d @ '0'..='9') => {
                    flush(&mut text, &mut pieces);
                    pieces.push(Piece::Group(d as usize - '0' as usize));
                }
                Some('n') => text.push(b'\n'),
                Some(d) => text.extend(d.to_string().bytes()),
                None => text.push(b'\\'),
            },
            c => text.extend(c.to_string().bytes()),
        }
    }
    flush(&mut text, &mut pieces);
    pieces
}

/// `tr SET1 SET2` and `tr -d SET1`, on ASCII sets with escapes and ranges.
struct Tr {
    delete: bool,
    from: Vec<u8>,
    to: Vec<u8>,
}

impl Tr {
    fn parse(args: &[String]) -> Result<Tr, String> {
        let (delete, sets) = match args {
            [d, set] if d == "-d" => (true, vec![set]),
            [a, b] if !a.starts_with('-') => (false, vec![a, b]),
            _ => return Err("tr arguments".into()),
        };
        let mut parsed = Vec::new();
        for set in sets {
            let mut chars = Vec::new();
            let mut cs = set.chars();
            while let Some(c) = cs.next() {
                chars.push(match (c, c == '\\') {
                    (_, true) => match cs.next() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some(d) => d,
                        None => '\\',
                    },
                    (c, false) => c,
                });
            }
            if !chars.iter().all(char::is_ascii) || set.contains("[:") {
                return Err(format!("tr set {set}"));
            }
            let mut bytes = Vec::new();
            let mut i = 0;
            while i < chars.len() {
                if i + 2 < chars.len() && chars[i + 1] == '-' {
                    bytes.extend(chars[i] as u8..=chars[i + 2] as u8);
                    i += 3;
                } else {
                    bytes.push(chars[i] as u8);
                    i += 1;
                }
            }
            parsed.push(bytes);
        }
        let to = parsed.get(1).cloned().unwrap_or_default();
        if !delete && to.is_empty() {
            return Err("tr with an empty SET2".into());
        }
        Ok(Tr {
            delete,
            from: parsed.swap_remove(0),
            to,
        })
    }

    fn apply(&self, data: &[u8]) -> Vec<u8> {
        data.iter()
            .filter_map(|b| match self.from.iter().rposition(|f| f == b) {
                Some(_) if self.delete => None,
                Some(i) => Some(self.to[i.min(self.to.len() - 1)]),
                None => Some(*b),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn printf_out(args: &[&str]) -> String {
        let mut out = Vec::new();
        printf(&strings(args), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn printf_formats() {
        assert_eq!(printf_out(&[r"a\tb\n%%\\"]), "a\tb\n%\\");
        assert_eq!(printf_out(&["--", r"--x\n"]), "--x\n");
        assert_eq!(printf_out(&["[%s]", "a", "b"]), "[a][b]");
        assert_eq!(printf_out(&["%s-%s;", "a", "b", "c"]), "a-b;c-;");
        assert_eq!(printf_out(&["%s", "it's"]), "it's");
        assert!(printf(&strings(&["%d", "1"]), &mut Vec::new()).is_err());
    }

    #[test]
    fn grep_options() {
        let g = Grep::parse(&strings(&["-cE", "^a|b$"])).unwrap();
        assert!(g.count && g.re.is_match(b"xb"));
        let g = Grep::parse(&strings(&["-o", r"x\(y\)\|z"])).unwrap();
        assert!(g.only && g.re.is_match(b"z") && g.re.is_match(b"xy"));
        let g = Grep::parse(&strings(&["-A1", "^ALIASES", "f"])).unwrap();
        assert_eq!((g.after, g.file.as_deref()), (1, Some("f")));
        let g = Grep::parse(&strings(&["-o", "-e", "<svg", "-e", "<table"])).unwrap();
        assert!(g.re.is_match(b"<table"));
        assert!(
            Grep::parse(&strings(&["-F", "a.b"]))
                .unwrap()
                .re
                .find(b"axb")
                .is_none()
        );
        assert!(Grep::parse(&strings(&["-i", "x"])).is_err());
    }

    #[test]
    fn sed_substitutions() {
        let run = |args: &[&str], input: &str| {
            String::from_utf8(Sed::parse(&strings(args)).unwrap().apply(input.as_bytes())).unwrap()
        };
        assert_eq!(run(&[r"s|.*/||"], "/a/b/c\nd"), "c\nd");
        assert_eq!(run(&["-n", r"s/x\(.\)/[\1]/p"], "ax1\nb\n"), "a[1]\n");
        assert_eq!(run(&["-E", r"s/([0-9]+)/<&>/g"], "1 22\n"), "<1> <22>\n");
        assert_eq!(run(&["-n", "s/a/A/p; s/b/B/p"], "ab\n"), "Ab\nAB\n");
        assert_eq!(run(&["-e", "s#/x/##"], "a/x/b\n"), "ab\n");
        assert_eq!(run(&["s,/.*/,DIR/,"], "w /a/b/c\n"), "w DIR/c\n");
        assert!(Sed::parse(&strings(&["1d"])).is_err());
    }

    #[test]
    fn tr_sets() {
        let tr = |args: &[&str], s: &str| {
            String::from_utf8(Tr::parse(&strings(args)).unwrap().apply(s.as_bytes())).unwrap()
        };
        assert_eq!(tr(&["<", r"\n"], "a<b"), "a\nb");
        assert_eq!(tr(&["-d", "<"], "<a<"), "a");
        assert_eq!(tr(&[r"\t\r", "TR"], "a\tb\rc"), "aTbRc");
        assert_eq!(tr(&["a-c", "x"], "abcd"), "xxxd");
    }

    #[test]
    fn chmod_modes() {
        let (m, _) = chmod_opts(&strings(&["+x", "f"])).unwrap();
        assert_eq!(m.apply(0o644), 0o755);
        let (m, _) = chmod_opts(&strings(&["-r", "f"])).unwrap();
        assert_eq!(m.apply(0o644), 0o200);
        let (m, _) = chmod_opts(&strings(&["u=rw,go=r", "f"])).unwrap();
        assert_eq!(m.apply(0o777), 0o644);
        let (m, _) = chmod_opts(&strings(&["600", "f"])).unwrap();
        assert_eq!(m.apply(0o777), 0o600);
    }

    #[test]
    fn head_and_tail_options() {
        assert_eq!(head_opts(&strings(&["-1"])).unwrap(), (1, None));
        assert_eq!(
            head_opts(&strings(&["-n", "3", "f"])).unwrap(),
            (3, Some("f"))
        );
        assert_eq!(head_opts(&strings(&[])).unwrap(), (10, None));
        assert_eq!(tail_opts(&strings(&["-n", "+2"])).unwrap(), (2, None));
        assert!(tail_opts(&strings(&["-n", "2"])).is_err());
    }
}
