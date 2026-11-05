//! A command's output (writeOutputLazyText and withTitle in
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
