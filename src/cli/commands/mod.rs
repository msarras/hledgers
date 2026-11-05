//! The builtin commands (Hledger.Cli.Commands), found by name or alias.

pub mod check;
pub mod files;
pub mod quickref;

use super::args::Mode;

/// The mode used when no command is given.
pub static MAIN_MODE: Mode = command("hledger", &[]);

/// The builtin commands in upstream's order, with their official short names.
/// Command-specific flags and actions arrive with the milestones that implement them.
pub static BUILTIN_COMMANDS: &[&Mode] = &[
    &command("accounts", &["acc"]),
    &command("activity", &[]),
    &command("add", &[]),
    &command("aregister", &["areg"]),
    &command("balance", &["bal"]),
    &command("balancesheetequity", &["bse"]),
    &command("balancesheet", &["bs"]),
    &command("cashflow", &["cf"]),
    &check::MODE,
    &command("close", &["equity"]),
    &command("codes", &[]),
    &command("commodities", &["comm"]),
    &command("descriptions", &["desc"]),
    &command("diff", &[]),
    &files::MODE,
    &command("get", &[]),
    &command("help", &[]),
    &command("holdings", &[]),
    &command("import", &[]),
    &command("incomestatement", &["is"]),
    &command("notes", &[]),
    &command("payees", &[]),
    &command("prices", &[]),
    &command("print", &[]),
    &command("register", &["reg"]),
    &command("rewrite", &[]),
    &command("roi", &[]),
    &command("run", &[]),
    &command("repl", &[]),
    &command("setup", &[]),
    &command("stats", &[]),
    &command("tags", &[]),
    &command("transactions", &["tx"]),
];

const fn command(name: &'static str, aliases: &'static [&'static str]) -> Mode {
    Mode {
        name,
        aliases,
        flags: &[],
    }
}

/// The builtin command with this exact name or alias.
pub fn find(name: &str) -> Option<&'static Mode> {
    BUILTIN_COMMANDS
        .iter()
        .find(|m| m.name == name || m.aliases.contains(&name))
        .copied()
}
