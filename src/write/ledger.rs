//! Transactions and journals as Ledger data (Hledger.Write.Ledger): lot
//! cost bases in Ledger's syntax, and with --export, the directives Ledger
//! does not support commented out.

use crate::amount::render::AmountFormat;
use crate::model::Transaction;
use crate::text::is_space;
use crate::write::journal::{PostingLayout, show_transaction_with};

/// A transaction as journal text with lot cost bases in Ledger's syntax,
/// `{COST} [DATE] (LABEL)` (showTransactionLedger).
pub fn show_transaction_ledger(t: &Transaction) -> String {
    let fmt = AmountFormat {
        ledger_lot_syntax: true,
        ..AmountFormat::DEFAULT
    };
    show_transaction_with(&fmt, t, PostingLayout::default(), false)
}

/// A directive as Ledger can read it: commented out, after a note, if it is
/// one of the hledger forms Ledger does not support (ledgerDirective).
pub fn ledger_directive(txt: &str) -> String {
    if !is_ledger_incompatible(txt) {
        return txt.to_string();
    }
    let mut out = "; not supported as-is:\n".to_string();
    for l in txt.lines() {
        out.push_str("; ");
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Whether a directive is one of the hledger forms Ledger does not support
/// (isLedgerIncompatible): decimal-mark, a one-line commodity directive with
/// an amount, a periodic transaction rule with a description, or an auto
/// posting rule with `*N` amount multipliers.
fn is_ledger_incompatible(txt: &str) -> bool {
    let mut lines = txt.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let mut words = first.split(is_space).filter(|w| !w.is_empty());
    match (words.next(), words.next()) {
        (Some("decimal-mark"), _) => true,
        (Some("commodity"), Some(sym)) => sym.chars().any(|c| c.is_ascii_digit()),
        (Some("~"), _) => {
            let rest = first
                .chars()
                .skip(1)
                .take_while(|&c| c != ';')
                .collect::<String>();
            crate::text::strip(&rest).contains("  ")
        }
        (Some("="), _) => lines.any(|l| {
            let after = l.find('*').map_or("", |i| &l[i + 1..]);
            after.chars().next().is_some_and(|c| c.is_ascii_digit())
        }),
        _ => false,
    }
}
