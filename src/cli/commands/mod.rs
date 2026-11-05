//! The builtin commands (Hledger.Cli.Commands), found by name or alias.

pub mod accounts;
pub mod activity;
pub mod aregister;
pub mod balance;
pub mod check;
pub mod close;
pub mod codes;
pub mod commodities;
pub mod compound;
pub mod descriptions;
pub mod files;
pub mod notes;
pub mod payees;
pub mod prices;
pub mod print;
pub mod quickref;
pub mod register;
pub mod rewrite;
pub mod stats;
pub mod tags;
pub mod transactions;

use super::args::Mode;

/// The mode used when no command is given.
pub static MAIN_MODE: Mode = command("hledger", &[]);

/// The builtin commands in upstream's order, with their official short names.
/// Command-specific flags and actions arrive with the milestones that implement them.
pub static BUILTIN_COMMANDS: &[&Mode] = &[
    &accounts::MODE,
    &activity::MODE,
    &command("add", &[]),
    &aregister::MODE,
    &balance::MODE,
    &compound::BALANCESHEETEQUITY.mode,
    &compound::BALANCESHEET.mode,
    &compound::CASHFLOW.mode,
    &check::MODE,
    &close::MODE,
    &codes::MODE,
    &commodities::MODE,
    &descriptions::MODE,
    &command("diff", &[]),
    &files::MODE,
    &command("get", &[]),
    &command("help", &[]),
    &command("holdings", &[]),
    &command("import", &[]),
    &compound::INCOMESTATEMENT.mode,
    &notes::MODE,
    &payees::MODE,
    &prices::MODE,
    &print::MODE,
    &register::MODE,
    &rewrite::MODE,
    &command("roi", &[]),
    &command("run", &[]),
    &command("repl", &[]),
    &command("setup", &[]),
    &stats::MODE,
    &tags::MODE,
    &transactions::MODE,
];

const fn command(name: &'static str, aliases: &'static [&'static str]) -> Mode {
    Mode {
        name,
        aliases,
        flags: &[],
    }
}

/// The error for something a later milestone provides.
pub fn not_yet(what: &str, milestone: &str) -> String {
    format!("{what} is not supported yet (hledgers milestone {milestone})")
}

/// The builtin command with this exact name or alias.
pub fn find(name: &str) -> Option<&'static Mode> {
    BUILTIN_COMMANDS
        .iter()
        .find(|m| m.name == name || m.aliases.contains(&name))
        .copied()
}
