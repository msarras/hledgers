//! Program name and version (Hledger.Cli.Version).

/// What `--version` prints, after the program's name: the hledger version
/// whose behaviour this program reproduces, then this program's own version.
const VERSION: &str = concat!("1.99.6 (hledgers ", env!("CARGO_PKG_VERSION"), ")");

/// The program's name and version, as `--version` prints them
/// (prognameandversion).
pub fn progname_and_version(progname: &str) -> String {
    format!("{progname} {VERSION}")
}
