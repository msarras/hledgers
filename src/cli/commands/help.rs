//! The help command (Hledger.Cli.Commands.Help): parts of hledger's
//! documentation.

use crate::cli::args::{Arity, Flag, General, Mode};

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "help",
    aliases: &["h"],
    flags: &[
        // the help-* names avoid a clash with the --info and --man flags
        Flag::new(&[], NONE, "", "use info when showing the manual")
            .short(&['i'])
            .sets(&["help-i"]),
        Flag::new(&[], NONE, "", "use man when showing the manual")
            .short(&['m'])
            .sets(&["help-m"]),
        Flag::new(
            &[],
            NONE,
            "",
            "use less (or $PAGER) when showing the manual",
        )
        .short(&['p'])
        .sets(&["help-p"]),
        Flag::new(&[], NONE, "", "use a web browser when showing the manual")
            .short(&['w'])
            .sets(&["help-w"]),
        Flag::new(&[], NONE, "", "just list the manual topics matching TOPIC")
            .short(&['l'])
            .sets(&["help-l"]),
        Flag::new(
            &["builtins"],
            NONE,
            "",
            "with commands: show only builtin commands",
        ),
        Flag::new(
            &["addons"],
            NONE,
            "",
            "with commands: show only addon commands",
        ),
        Flag::new(
            &["aliases"],
            NONE,
            "",
            "with commands: show only command aliases",
        ),
    ],
    general: General::Groups3,
    args: Some("[TOPIC]"),
    doc: crate::command_doc!("Help"),
    ..Mode::DEFAULT
};
