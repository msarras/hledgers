//! A command's output, and a warning before a large one
//! (writeOutputLazyText, withTitle and warnIfLargeMultiPeriodReport in
//! Hledger.Cli.Utils).

use std::io::Write;

use super::opts::CliOpts;

/// Write a command's output to standard output, or to the -o file
/// (writeOutputLazyText).
pub fn write_output(opts: &CliOpts, text: &str) -> Result<(), String> {
    match opts.output_file.as_deref() {
        None | Some("-") => {
            let mut stdout = std::io::stdout().lock();
            // a reader that stops early (hledger ... | head) is not an error
            let _ = stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.flush());
            Ok(())
        }
        Some(f) => {
            let path = crate::read::journal::expand_home_path(f)?;
            std::fs::write(&path, text).map_err(|e| format!("{path}: withFile: {e}"))
        }
    }
}

/// Show some text on standard output, for help and documentation, which
/// --output-file does not redirect (runPager).
pub fn run_pager(text: &str) -> Result<(), String> {
    print_stdout(text);
    Ok(())
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
    let (span, periods) = opts.spec.report_span(j);
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
