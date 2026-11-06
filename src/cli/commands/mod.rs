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
pub mod diff;
pub mod files;
pub mod help;
pub mod holdings;
pub mod import;
pub mod notes;
pub mod payees;
pub mod prices;
pub mod print;
pub mod quickref;
pub mod register;
pub mod rewrite;
pub mod roi;
pub mod run;
pub mod stats;
pub mod tags;
pub mod transactions;

use super::args::{Arity, Flag, General, Hidden, Mode};
use super::opts::CONF_FLAGS;

/// The mode used when no command is given (mainmode): the config file
/// flags shown first, the legacy flags hidden.
pub static MAIN_MODE: Mode = Mode {
    name: "hledger [COMMAND]",
    flags: CONF_FLAGS,
    hidden: Hidden::Legacy,
    args: Some("[ARGS]"),
    doc: "hledger [COMMAND]\n\nhledger's main command line interface. Run with no ARGS to see a quick reference, or `hledger help commands` to list commands.\n",
    ..Mode::DEFAULT
};

/// The builtin commands in upstream's order, with their official short names.
/// Command-specific flags and actions arrive with the milestones that implement them.
pub static BUILTIN_COMMANDS: &[&Mode] = &[
    &accounts::MODE,
    &activity::MODE,
    &ADD_MODE,
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
    &diff::MODE,
    &files::MODE,
    &GET_MODE,
    &help::MODE,
    &holdings::MODE,
    &import::MODE,
    &compound::INCOMESTATEMENT.mode,
    &notes::MODE,
    &payees::MODE,
    &prices::MODE,
    &print::MODE,
    &register::MODE,
    &rewrite::MODE,
    &roi::MODE,
    &run::RUN_MODE,
    &run::REPL_MODE,
    &SETUP_MODE,
    &stats::MODE,
    &tags::MODE,
    &transactions::MODE,
];

// The modes of the commands milestone 8b implements.

static ADD_MODE: Mode = Mode {
    name: "add",
    flags: &[
        Flag::new(
            &["no-new-accounts"],
            Arity::None,
            "",
            "don't allow creating new accounts",
        ),
        print::LAYOUT_FLAG,
    ],
    general: General::Group2,
    hidden: Hidden::Conf,
    args: Some("[-f JOURNALFILE] [DATE [DESCRIPTION [ACCOUNT1 [ETC..]]]]]"),
    doc: crate::command_doc!("Add"),
    ..Mode::DEFAULT
};

static GET_MODE: Mode = Mode {
    name: "get",
    flags: &[
        Flag::new(
            &["transactions"],
            Arity::None,
            "",
            "fetch transactions only",
        ),
        Flag::new(&["prices"], Arity::None, "", "fetch prices only"),
        Flag::new(
            &["dry-run"],
            Arity::None,
            "",
            "just print the commands that would be run",
        ),
    ],
    general: General::Groups2,
    args: None,
    doc: crate::command_doc!("Get"),
    ..Mode::DEFAULT
};

static SETUP_MODE: Mode = Mode {
    name: "setup",
    general: General::Group3,
    hidden: Hidden::None,
    args: None,
    doc: crate::command_doc!("Setup"),
    ..Mode::DEFAULT
};

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
