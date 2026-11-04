//! Evaluation of parsed command lines: expansion, redirection, pipelines, the
//! built-ins that change shell state, and running `hledger` as a child process.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;

use super::builtins;
use super::syntax::{self, Command, Connector, List, Part, RedirOp, Simple, Word};

/// What a command line printed, and its exit status.
#[derive(Debug, Default)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

/// Run a command line in `cwd` with these (exported) environment variables,
/// feeding it `stdin`. `pid` is what `$$` expands to. Err means the line turned
/// out to use something this interpreter does not support.
pub fn run(
    cmdline: &str,
    stdin: &[u8],
    cwd: &Path,
    env: BTreeMap<String, String>,
    pid: u64,
) -> Result<Output, String> {
    let list = syntax::parse(cmdline)?;
    let mut sh = Shell {
        cwd: cwd.to_path_buf(),
        env,
        vars: BTreeMap::new(),
        status: 0,
        pid,
    };
    let (out, err) = (Out::buffer(), Out::buffer());
    let io = Io {
        stdin: Input::new(stdin.to_vec()),
        out: out.clone(),
        err: err.clone(),
    };
    let status = match sh.run_list(&list, &io) {
        Ok(s) | Err(Flow::Exit(s)) => s,
        Err(Flow::Unsupported(why)) => return Err(why),
    };
    Ok(Output {
        stdout: out.take(),
        stderr: err.take(),
        status,
    })
}

/// Non-local exits: `exit N`, or hitting an unsupported construct at run time.
pub enum Flow {
    Exit(i32),
    Unsupported(String),
}

pub type R<T> = Result<T, Flow>;

/// A command's standard input. Built-ins that read it consume it; `hledger` is
/// given a copy, since whether it reads stdin is not known from outside.
#[derive(Clone)]
pub struct Input(Rc<RefCell<Vec<u8>>>);

impl Input {
    pub fn new(data: Vec<u8>) -> Input {
        Input(Rc::new(RefCell::new(data)))
    }

    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.0.borrow_mut())
    }

    fn copy(&self) -> Vec<u8> {
        self.0.borrow().clone()
    }
}

/// Where a file descriptor writes. Clones share the destination, so `2>&1`
/// interleaves both streams in order.
#[derive(Clone)]
pub enum Out {
    Buffer(Rc<RefCell<Vec<u8>>>),
    File(Rc<File>),
}

impl Out {
    fn buffer() -> Out {
        Out::Buffer(Rc::default())
    }

    pub fn write(&self, data: &[u8]) {
        match self {
            Out::Buffer(b) => b.borrow_mut().extend_from_slice(data),
            // Like a shell, ignore write errors (eg a full disk) here.
            Out::File(f) => drop((&**f).write_all(data)),
        }
    }

    fn take(&self) -> Vec<u8> {
        match self {
            Out::Buffer(b) => std::mem::take(&mut b.borrow_mut()),
            _ => Vec::new(),
        }
    }

    fn same(&self, other: &Out) -> bool {
        match (self, other) {
            (Out::Buffer(a), Out::Buffer(b)) => Rc::ptr_eq(a, b),
            (Out::File(a), Out::File(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct Io {
    pub stdin: Input,
    pub out: Out,
    pub err: Out,
}

/// Shell state: what a subshell copies and `cd`, assignments and `export` change.
#[derive(Clone)]
pub struct Shell {
    pub cwd: PathBuf,
    /// Exported variables, passed to `hledger`.
    pub env: BTreeMap<String, String>,
    /// Unexported shell variables.
    pub vars: BTreeMap<String, String>,
    /// `$?`
    pub status: i32,
    /// `$$`
    pub pid: u64,
}

impl Shell {
    pub fn path(&self, p: &str) -> PathBuf {
        self.cwd.join(p)
    }

    fn get(&self, name: &str) -> Option<String> {
        match name {
            "$" => Some(self.pid.to_string()),
            "?" => Some(self.status.to_string()),
            _ => self.vars.get(name).or_else(|| self.env.get(name)).cloned(),
        }
    }

    fn set(&mut self, name: &str, value: String) {
        if let Some(v) = self.env.get_mut(name) {
            *v = value;
        } else {
            self.vars.insert(name.to_string(), value);
        }
    }

    /// Run a list in a copy of this shell, as `( ... )` and pipeline stages do.
    fn subshell(&self, list: &List, io: &Io) -> R<i32> {
        match self.clone().run_list(list, io) {
            Ok(s) | Err(Flow::Exit(s)) => Ok(s),
            Err(u) => Err(u),
        }
    }

    pub fn run_list(&mut self, list: &List, io: &Io) -> R<i32> {
        for and_or in &list.0 {
            self.status = self.run_pipeline(&and_or.first.0, io)?;
            for (connector, pipeline) in &and_or.rest {
                if (*connector == Connector::And) == (self.status == 0) {
                    self.status = self.run_pipeline(&pipeline.0, io)?;
                }
            }
        }
        Ok(self.status)
    }

    /// Run a pipeline. Stages run one after another, each in a subshell, with
    /// each stage's whole output becoming the next stage's input.
    fn run_pipeline(&mut self, cmds: &[Command], io: &Io) -> R<i32> {
        if let [cmd] = cmds {
            return self.run_command(cmd, io);
        }
        let mut stdin = io.stdin.clone();
        let mut status = 0;
        for (i, cmd) in cmds.iter().enumerate() {
            let last = i + 1 == cmds.len();
            let out = if last { io.out.clone() } else { Out::buffer() };
            let stage = Io {
                stdin: stdin.clone(),
                out: out.clone(),
                err: io.err.clone(),
            };
            status = match self.clone().run_command(cmd, &stage) {
                Ok(s) | Err(Flow::Exit(s)) => s,
                Err(u) => return Err(u),
            };
            if !last {
                stdin = Input::new(out.take());
            }
        }
        Ok(status)
    }

    fn run_command(&mut self, cmd: &Command, io: &Io) -> R<i32> {
        match cmd {
            Command::Subshell(list, redirs) => match self.redirect(redirs, io)? {
                Ok(io) => self.subshell(list, &io),
                Err(status) => Ok(status),
            },
            Command::Simple(s) => self.run_simple(s, io),
        }
    }

    fn run_simple(&mut self, cmd: &Simple, io: &Io) -> R<i32> {
        let mut argv = Vec::new();
        for w in &cmd.words {
            check_glob(w, &self.cwd).map_err(Flow::Unsupported)?;
            argv.extend(self.expand(w, io)?);
        }
        let mut assigns = Vec::new();
        for (name, w) in &cmd.assigns {
            assigns.push((name.clone(), self.expand_one(w, io)?));
        }
        let io = match self.redirect(&cmd.redirs, io)? {
            Ok(io) => io,
            Err(status) => return Ok(status),
        };
        let Some((name, args)) = argv.split_first() else {
            for (name, value) in assigns {
                self.set(&name, value);
            }
            return Ok(0);
        };
        match name.as_str() {
            "hledger" => self.external(&argv, &assigns, &io),
            "./csvtest.sh" => self.csvtest(args, &io, false),
            "./ssvtest.sh" => self.csvtest(args, &io, true),
            "sh" => self.sh(args, &assigns, &io),
            "cd" => Ok(self.cd(args, &io)),
            "exit" => {
                let status = match args.first() {
                    Some(n) => n.parse().map_err(|_| unsupported("exit status"))?,
                    None => self.status,
                };
                Err(Flow::Exit(status))
            }
            "export" => {
                for a in args {
                    match a.split_once('=') {
                        Some((n, v)) => {
                            self.vars.remove(n);
                            self.env.insert(n.to_string(), v.to_string());
                        }
                        None => {
                            let v = self.vars.remove(a).unwrap_or_default();
                            self.env.insert(a.clone(), v);
                        }
                    }
                }
                Ok(0)
            }
            _ => builtins::run(self, name, args, &io),
        }
    }

    /// Apply redirections in order to a copy of `io`. Ok(Err(status)) means a
    /// file could not be opened, so the command does not run.
    fn redirect(&mut self, redirs: &[syntax::Redir], io: &Io) -> R<Result<Io, i32>> {
        let mut io = io.clone();
        for r in redirs {
            let target = match r.op {
                RedirOp::Dup(_) => String::new(),
                _ => self.expand_one(&r.target, &io)?,
            };
            let path = self.path(&target);
            let opened = match r.op {
                RedirOp::Dup(n) => {
                    let src = if n == 1 {
                        io.out.clone()
                    } else {
                        io.err.clone()
                    };
                    Ok(Some(src))
                }
                RedirOp::In => std::fs::read(&path).map(|data| {
                    io.stdin = Input::new(data);
                    None
                }),
                RedirOp::Out | RedirOp::Append => File::options()
                    .write(true)
                    .create(true)
                    .truncate(r.op == RedirOp::Out)
                    .append(r.op == RedirOp::Append)
                    .open(&path)
                    .map(|f| Some(Out::File(Rc::new(f)))),
            };
            match opened {
                Ok(Some(out)) if r.fd == 1 => io.out = out,
                Ok(Some(out)) => io.err = out,
                Ok(None) => {}
                Err(e) => {
                    let what = if r.op == RedirOp::In {
                        "open"
                    } else {
                        "create"
                    };
                    io.err
                        .write(format!("sh: cannot {what} {target}: {e}\n").as_bytes());
                    return Ok(Err(2));
                }
            }
        }
        Ok(Ok(io))
    }

    /// Expand a word into fields: unquoted expansion results are split at
    /// blanks, and a word that expands to nothing unquoted gives no field.
    fn expand(&mut self, w: &Word, io: &Io) -> R<Vec<String>> {
        let mut fields = Vec::new();
        let mut cur: Option<String> = None;
        for part in &w.0 {
            let (text, split) = match part {
                Part::Lit { text, .. } => (text.clone(), false),
                Part::Param { quoted, .. } | Part::Subst { quoted, .. } => {
                    (self.expand_part(part, io)?, !quoted)
                }
            };
            if !split {
                cur.get_or_insert_with(String::new).push_str(&text);
                continue;
            }
            for c in text.chars() {
                if matches!(c, ' ' | '\t' | '\n') {
                    fields.extend(cur.take());
                } else {
                    cur.get_or_insert_with(String::new).push(c);
                }
            }
        }
        fields.extend(cur);
        Ok(fields)
    }

    /// Expand a word to one string, without field splitting (assignments, redirections).
    fn expand_one(&mut self, w: &Word, io: &Io) -> R<String> {
        let mut s = String::new();
        for part in &w.0 {
            s.push_str(&self.expand_part(part, io)?);
        }
        Ok(s)
    }

    fn expand_part(&mut self, part: &Part, io: &Io) -> R<String> {
        match part {
            Part::Lit { text, .. } => Ok(text.clone()),
            // ${NAME:-DEFAULT} uses DEFAULT when NAME is unset or empty.
            Part::Param { name, default, .. } => {
                match (self.get(name).unwrap_or_default(), default) {
                    (v, Some(d)) if v.is_empty() => self.expand_one(d, io),
                    (v, _) => Ok(v),
                }
            }
            Part::Subst { cmd, .. } => {
                let out = Out::buffer();
                let sub = Io {
                    stdin: Input::new(Vec::new()),
                    out: out.clone(),
                    err: io.err.clone(),
                };
                self.clone().run_simple(cmd, &sub)?;
                let mut s = String::from_utf8_lossy(&out.take()).into_owned();
                while s.ends_with('\n') {
                    s.pop();
                }
                Ok(s)
            }
        }
    }

    /// Run `hledger` (or any program found in PATH) as a child process.
    pub fn external(&mut self, argv: &[String], assigns: &[(String, String)], io: &Io) -> R<i32> {
        let mut env = self.env.clone();
        env.extend(assigns.iter().cloned());
        let name = &argv[0];
        let Some(program) =
            find_in_path(name, env.get("PATH").map_or("", |p| p.as_str()), &self.cwd)
        else {
            io.err
                .write(format!("sh: 1: {name}: not found\n").as_bytes());
            return Ok(127);
        };
        let pipe_error = |e: std::io::Error| unsupported(&format!("could not run {name}: {e}"));
        let mut cmd = std::process::Command::new(program);
        cmd.arg0(name)
            .args(&argv[1..])
            .env_clear()
            .envs(&env)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped());
        // Output going to buffers is read through pipes; a shared buffer
        // (2>&1) gets one pipe for both streams, keeping their order.
        let mut readers = Vec::new();
        let mut shared = None;
        for (fd, out) in [(1, &io.out), (2, &io.err)] {
            let stdio = match out {
                Out::File(f) => Stdio::from(f.try_clone().map_err(pipe_error)?),
                Out::Buffer(_) => match &shared {
                    Some(w) if io.out.same(&io.err) => {
                        Stdio::from(std::io::PipeWriter::try_clone(w).map_err(pipe_error)?)
                    }
                    _ => {
                        let (r, w) = std::io::pipe().map_err(pipe_error)?;
                        readers.push((out.clone(), r));
                        let stdio = Stdio::from(w.try_clone().map_err(pipe_error)?);
                        shared = Some(w);
                        stdio
                    }
                },
            };
            if fd == 1 {
                cmd.stdout(stdio);
            } else {
                cmd.stderr(stdio);
            }
        }
        let mut child = cmd.spawn().map_err(pipe_error)?;
        // Close our copies of the pipes' write ends, so reads end when the child exits.
        drop(cmd);
        drop(shared);
        let input = io.stdin.copy();
        let mut stdin = child.stdin.take();
        let outputs: Vec<Vec<u8>> = std::thread::scope(|s| {
            s.spawn(move || {
                // The child may exit without reading; a broken pipe is fine.
                if let Some(mut w) = stdin.take() {
                    let _ = w.write_all(&input);
                }
            });
            let handles: Vec<_> = readers
                .iter_mut()
                .map(|(_, r)| {
                    s.spawn(move || {
                        let mut data = Vec::new();
                        let _ = r.read_to_end(&mut data);
                        data
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let status = child.wait().map_err(pipe_error)?;
        for ((out, _), data) in readers.iter().zip(outputs) {
            out.write(&data);
        }
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }

    /// `./csvtest.sh ARGS` and `./ssvtest.sh ARGS`, as the corpus's scripts of
    /// those names do it: stdin holds the data, a `RULES` line, then the rules.
    fn csvtest(&mut self, args: &[String], io: &Io, ssv: bool) -> R<i32> {
        let input = io.stdin.take();
        let lines: Vec<&[u8]> = input.split_inclusive(|&b| b == b'\n').collect();
        let is_rules = |l: &&[u8]| l.starts_with(b"RULES");
        // sed '/^RULES/,$d' keeps the lines before the first RULES line;
        // sed '1,/^RULES/d' keeps those after the first RULES line after line 1.
        let data_end = lines.iter().position(is_rules).unwrap_or(lines.len());
        let rules_start = lines
            .iter()
            .skip(1)
            .position(is_rules)
            .map_or(lines.len(), |i| i + 2);
        let pid = self.pid;
        let (data_file, rules_file) = if ssv {
            (format!("t.{pid}"), format!("t.{pid}.rules"))
        } else {
            (format!("t.{pid}.csv"), format!("t.{pid}.csv.rules"))
        };
        let write = |name: &str, lines: &[&[u8]]| std::fs::write(self.path(name), lines.concat());
        write(&data_file, &lines[..data_end])
            .and_then(|()| write(&rules_file, &lines[rules_start.min(lines.len())..]))
            .map_err(|e| unsupported(&format!("csvtest could not write its files: {e}")))?;
        let mut argv: Vec<String> = if ssv {
            vec![
                "hledger".into(),
                "-f".into(),
                format!("ssv:{data_file}"),
                "print".into(),
            ]
        } else {
            vec![
                "hledger".into(),
                "-f".into(),
                format!("csv:{data_file}"),
                "--rules-file".into(),
                rules_file.clone(),
                "print".into(),
            ]
        };
        argv.extend(args.iter().cloned());
        let err = Out::buffer();
        let inner = Io {
            stdin: Input::new(Vec::new()),
            out: io.out.clone(),
            err: err.clone(),
        };
        let status = self.external(&argv, &[], &inner);
        let _ = std::fs::remove_file(self.path(&data_file));
        let _ = std::fs::remove_file(self.path(&rules_file));
        let status = status?;
        // sed -e "s/t\.$$\.csv/input/": the first occurrence on each line.
        let needle = data_file.as_bytes();
        for line in err.take().split_inclusive(|&b| b == b'\n') {
            match line.windows(needle.len()).position(|w| w == needle) {
                Some(i) => {
                    io.err.write(&line[..i]);
                    io.err.write(b"input");
                    io.err.write(&line[i + needle.len()..]);
                }
                None => io.err.write(line),
            }
        }
        Ok(status)
    }

    /// `sh -c SCRIPT` or `sh FILE`: a new shell with the exported variables.
    fn sh(&mut self, args: &[String], assigns: &[(String, String)], io: &Io) -> R<i32> {
        let script = match args {
            [c, script] if c == "-c" => script.clone(),
            [file] => std::fs::read_to_string(self.path(file))
                .map_err(|e| unsupported(&format!("sh {file}: {e}")))?,
            _ => return Err(unsupported("sh arguments")),
        };
        let list = syntax::parse(&script).map_err(Flow::Unsupported)?;
        let mut env = self.env.clone();
        env.extend(assigns.iter().cloned());
        let mut sub = Shell {
            cwd: self.cwd.clone(),
            env,
            vars: BTreeMap::new(),
            status: 0,
            pid: self.pid,
        };
        match sub.run_list(&list, io) {
            Ok(s) | Err(Flow::Exit(s)) => Ok(s),
            Err(u) => Err(u),
        }
    }

    /// `cd [DIR]`, keeping the logical path as sh does.
    fn cd(&mut self, args: &[String], io: &Io) -> i32 {
        let dir = match args.first() {
            Some(d) => d.clone(),
            None => self.get("HOME").unwrap_or_default(),
        };
        let mut path = PathBuf::new();
        for c in self.cwd.join(&dir).components() {
            match c {
                Component::ParentDir => {
                    path.pop();
                }
                Component::CurDir => {}
                c => path.push(c),
            }
        }
        if !path.is_dir() {
            io.err
                .write(format!("sh: 1: cd: can't cd to {dir}\n").as_bytes());
            return 2;
        }
        self.set("PWD", path.to_string_lossy().into_owned());
        self.cwd = path;
        0
    }
}

pub fn unsupported(what: &str) -> Flow {
    Flow::Unsupported(what.to_string())
}

fn find_in_path(name: &str, path: &str, cwd: &Path) -> Option<PathBuf> {
    path.split(':').find_map(|dir| {
        let candidate = cwd.join(dir).join(name);
        let meta = std::fs::metadata(&candidate).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

/// sh replaces a word with unquoted `*` or `?` by the file names it matches; a
/// word that matches none stays as written. Only that second case is supported.
pub fn check_glob(w: &Word, dir: &Path) -> Result<(), String> {
    let is_glob =
        |p: &Part| matches!(p, Part::Lit { text, quoted: false } if text.contains(['*', '?', '[']));
    if !w.0.iter().any(is_glob) {
        return Ok(());
    }
    let mut re = String::from("^");
    for p in &w.0 {
        match p {
            Part::Lit { text, quoted } => {
                for c in text.chars() {
                    match c {
                        '/' => return Err("pathname expansion of a path".into()),
                        '[' if !quoted => return Err("pathname expansion with `[`".into()),
                        '*' if !quoted => re.push_str("[^/]*"),
                        '?' if !quoted => re.push_str("[^/]"),
                        c => re.push_str(&regex::escape(&c.to_string())),
                    }
                }
            }
            _ => return Err("pathname expansion of an expansion".into()),
        }
    }
    re.push('$');
    let re = regex::Regex::new(&re).unwrap();
    let dotted = re.as_str().starts_with(r"^\.");
    let matches = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (dotted || !name.starts_with('.')) && re.is_match(&name)
        });
    if matches {
        Err("pathname expansion that matches files".into())
    } else {
        Ok(())
    }
}
