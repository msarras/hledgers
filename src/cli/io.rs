//! A command's output, a warning before a large one, and the error for an
//! output format a command lacks (writeOutputLazyText, withTitle,
//! warnIfLargeMultiPeriodReport and unsupportedOutputFormatError in
//! Hledger.Cli.Utils).

use std::io::Write;

use super::opts::CliOpts;

/// Write a command's output to standard output, or to the -o file, or with
/// --view to the view file, then open it (writeOutput).
pub fn write_output(opts: &CliOpts, text: &str) -> Result<(), String> {
    write_output_with(opts, text, false)
}

/// Like [`write_output`], but on standard output through a pager when
/// that is wanted and possible (writeOutputLazyText).
pub fn write_output_paged(opts: &CliOpts, text: &str) -> Result<(), String> {
    write_output_with(opts, text, true)
}

fn write_output_with(opts: &CliOpts, text: &str, paged: bool) -> Result<(), String> {
    let file = match opts.output_file.as_deref() {
        None | Some("-") if opts.raw.bool("view") => Some(view_file(opts)?),
        None | Some("-") => None,
        Some(f) => Some(crate::read::journal::expand_home_path(f)?),
    };
    match file {
        None if paged => run_pager(opts, text),
        None => {
            print_stdout(text);
            Ok(())
        }
        Some(path) => {
            std::fs::write(&path, text).map_err(|e| format!("{path}: withFile: {e}"))?;
            if opts.raw.bool("view")
                && let Err(tried) = super::docs::launch(&path)
            {
                crate::console::warn(
                    &opts.input.prog_args,
                    &format!("could not open a viewer (tried: {})", tried.join(", ")),
                );
            }
            Ok(())
        }
    }
}

/// view.FMT in hledger's cache directory, made (or emptied), readable only
/// by the user, after saying where it is (viewFile).
fn view_file(opts: &CliOpts) -> Result<String, String> {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|d| d.starts_with('/'))
        .unwrap_or_else(|| format!("{}/.cache", crate::platform::home_dir()));
    let dir = format!("{base}/hledger");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
    let f = format!("{dir}/view.{}", opts.output_format());
    eprintln!("writing {f} (if it's sensitive, delete it after viewing)");
    crate::platform::private_file_options()
        .open(&f)
        .map_err(|e| format!("{f}: {e}"))?;
    Ok(f)
}

/// Show some text on standard output, for help, documentation and
/// reports, through a pager when it does not fit the terminal and a pager
/// can be used (runPager).
pub fn run_pager(opts: &CliOpts, text: &str) -> Result<(), String> {
    let Some(pager) = pager_for(opts, text) else {
        print_stdout(text);
        return Ok(());
    };
    let mut cmd = crate::platform::shell_command(&pager);
    cmd.stdin(std::process::Stdio::piped());
    let is_less = pager
        .rsplit('/')
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case("less"));
    if is_less {
        let less = less_var_value(
            std::env::var("HLEDGER_LESS").ok().as_deref(),
            std::env::var("LESS").ok().as_deref(),
            opts.spec.opts.color,
        );
        if !less_is_working(&less) {
            crate::console::warn(
                &opts.input.prog_args,
                "less --version fails with current LESS settings; disabling. Check 'hledger setup' for details.\n",
            );
            print_stdout(text);
            return Ok(());
        }
        cmd.env("LESS", less);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                // a pager quitting early is not an error
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
        Err(_) => print_stdout(text),
    }
    Ok(())
}

/// The pager to show this text with, if any (maybePagerFor): none with
/// --pager=no, an output file, inside Emacs (but its vterm), when stdout is
/// not a terminal, or when the text fits the terminal; otherwise $PAGER,
/// less or more, whichever is found first.
fn pager_for(opts: &CliOpts, text: &str) -> Option<String> {
    let to_file = opts.output_file.as_deref().is_some_and(|f| f != "-");
    let emacs = std::env::var("INSIDE_EMACS").is_ok_and(|e| !e.contains("vterm"));
    if opts.pager == Some(false) || to_file || emacs {
        return None;
    }
    let (w, h) = terminal_size::terminal_size_of(std::io::stdout())?;
    let lines: Vec<&str> = text.lines().collect();
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if lines.len() <= usize::from(h.0) && width <= usize::from(w.0) {
        return None;
    }
    let env_pager = std::env::var("PAGER").ok();
    (env_pager.iter().map(String::as_str))
        .chain(["less", "more"])
        .find(|p| find_executable(p))
        .map(str::to_string)
}

/// Whether a program can be run: a path to an executable file, or a name
/// found in PATH (findExecutable).
fn find_executable(p: &str) -> bool {
    use crate::platform::is_executable;
    if p.contains(std::path::is_separator) {
        return is_executable(std::path::Path::new(p));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| is_executable(&d.join(p)))
}

/// The LESS value to run less with (lessVarValue): HLEDGER_LESS if set,
/// else LESS with hledger's preferred options added, or just those.
pub fn less_var_value(hledger_less: Option<&str>, less: Option<&str>, color: bool) -> String {
    let mut extra = vec![
        "--chop-long-lines",
        "--hilite-unread",
        "--ignore-case",
        "--no-init",
        "--quit-if-one-screen",
        "--shift=8",
        "--squeeze-blank-lines",
        "--use-backslash",
    ];
    if color {
        extra.push("--RAW-CONTROL-CHARS");
    }
    match (hledger_less, less) {
        (Some(h), _) => h.to_string(),
        (None, Some(l)) => {
            let existing: Vec<&str> = l.split_whitespace().collect();
            let new: Vec<&str> = extra
                .into_iter()
                .filter(|o| !existing.contains(o))
                .collect();
            if new.is_empty() {
                l.to_string()
            } else {
                format!("{l} {}", new.join(" "))
            }
        }
        (None, None) => extra.join(" "),
    }
}

/// Whether `less --version` succeeds quietly, within 0.3 s, with this LESS
/// value (the opposite of lessIsWorking, which returns whether it fails).
pub fn less_is_working(less: &str) -> bool {
    let child = std::process::Command::new("less")
        .arg("--version")
        .env("LESS", less)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                return status.success() && err.is_empty();
            }
            Ok(None) if start.elapsed() < std::time::Duration::from_millis(300) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Write text to standard output; a reader that stops early is not an error.
pub fn print_stdout(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
}

/// The report's title, if --title gives one, and a blank line, before the
/// output (withTitle).
pub fn with_title(opts: &CliOpts, body: String) -> String {
    let title = opts.spec.opts.effective_title("");
    if title.is_empty() {
        body
    } else {
        format!("{title}\n\n{body}")
    }
}

/// These items one per line, after the report's title if any: the output
/// of the commands listing names (T.unlines and writeOutputWithTitle).
pub fn lines_with_title<S: AsRef<str>>(
    opts: &CliOpts,
    items: impl IntoIterator<Item = S>,
) -> String {
    let mut out = String::new();
    for i in items {
        out.push_str(i.as_ref());
        out.push('\n');
    }
    with_title(opts, out)
}

/// Warn if the report has a report interval and more than 10000 periods,
/// which might be a mistyped date (warnIfLargeMultiPeriodReport).
pub fn warn_if_large_multi_period_report(opts: &CliOpts, j: &crate::model::Journal) {
    if opts.spec.opts.interval == crate::dates::Interval::NoInterval {
        return;
    }
    let Ok((span, periods)) = opts.spec.report_span(j) else {
        return;
    };
    let n = periods.map_or(0, |p| p.periods().count());
    if n > 10000 {
        crate::console::warn(
            &opts.input.prog_args,
            &format!(
                "this report has {n} periods ({span}), and may use a lot of memory.\n\
                 If not intended, press control-C now, and check for a mistyped date, or narrow the report."
            ),
        );
    }
}

/// The error for an output format a command doesn't support
/// (unsupportedOutputFormatError).
pub fn unsupported_output_format_error(fmt: &str) -> String {
    let mut msg = format!(
        "Sorry, output format \"{fmt}\" is unrecognised or not yet supported for this kind of report."
    );
    let ext = fmt.rsplit('.').next().unwrap_or("");
    if fmt.contains('.')
        && [
            "txt",
            "beancount",
            "csv",
            "json",
            "html",
            "ledger",
            "sql",
            "tsv",
            "fods",
            "svg",
        ]
        .contains(&ext)
    {
        msg.push_str(&format!(
            "\nTo write to a file named {fmt}, use -o (--output-file), not -O (--output-format)."
        ));
    }
    msg
}
