//! The payees command (Hledger.Cli.Commands.Payees): the payee names used
//! by the matched transactions or declared.

use std::collections::BTreeSet;

use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::lines_with_title;
use crate::cli::opts::{CliOpts, Declarables, find_matched_by_argument};
use crate::model::{Journal, Transaction};

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "payees",
    aliases: &[],
    flags: &[
        Flag::new(&["used"], NONE, "", "list payees used"),
        Flag::new(&["declared"], NONE, "", "list payees declared"),
        Flag::new(
            &["undeclared"],
            NONE,
            "",
            "list payees used but not declared",
        ),
        Flag::new(&["unused"], NONE, "", "list payees declared but not used"),
        Flag::new(
            &["find"],
            NONE,
            "",
            "list the first payee matched by the first argument (a case-insensitive infix regexp)",
        ),
        Flag::new(
            &["directives"],
            NONE,
            "",
            "show as payee directives, for use in journals",
        ),
    ],
};

/// Run the payees command (payees): the payees of the transactions the
/// query matches and the declared payees it matches, or with --used,
/// --declared, --undeclared, --unused or --find just those, sorted.
pub fn payees(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let q = &opts.spec.query;
    let all_used: BTreeSet<&str> = j.txns.iter().map(Transaction::payee).collect();
    let all_declared: BTreeSet<&str> = j.declared_payees.iter().map(|(p, _)| p.as_str()).collect();
    let used: BTreeSet<&str> = j
        .txns
        .iter()
        .filter(|t| q.matches_transaction(t))
        .map(Transaction::payee)
        .collect();
    let declared: BTreeSet<&str> = all_declared
        .iter()
        .copied()
        .filter(|p| q.matches_payee(p))
        .collect();
    // unused and undeclared are relative to all payees used or declared,
    // so that eg a date: query can't make a declared payee look undeclared
    let payees: Vec<&str> = match opts.declarables()? {
        None => declared.union(&used).copied().collect(),
        Some(Declarables::Used) => used.into_iter().collect(),
        Some(Declarables::Declared) => declared.into_iter().collect(),
        Some(Declarables::Undeclared) => used.difference(&all_declared).copied().collect(),
        Some(Declarables::Unused) => declared.difference(&all_used).copied().collect(),
        Some(Declarables::FindFirst) => {
            let all = all_used.union(&all_declared).copied();
            vec![find_matched_by_argument(&opts.raw, "payee", all)?]
        }
    };
    // as directives, a name containing a semicolon double-quoted
    let directives = opts.raw.bool("directives");
    Ok(lines_with_title(
        opts,
        payees
            .into_iter()
            .map(|p| match (directives, p.contains(';')) {
                (false, _) => p.to_string(),
                (true, false) => format!("payee {p}"),
                (true, true) => format!("payee \"{p}\""),
            }),
    ))
}
