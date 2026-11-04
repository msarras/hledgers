//! Program name and version (Hledger.Cli.Version).

/// What `--version` prints: the hledger version whose behaviour this program
/// reproduces, then this program's own version.
pub const PROGNAME_AND_VERSION: &str =
    concat!("hledger 1.99.6 (hledgers ", env!("CARGO_PKG_VERSION"), ")");
