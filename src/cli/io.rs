//! A command's output (writeOutputLazyText in Hledger.Cli.Utils).

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
