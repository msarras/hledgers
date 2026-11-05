//! The commodities command (Hledger.Cli.Commands.Commodities): the
//! commodity symbols used in transactions or P directives, or declared.

use std::collections::BTreeSet;

use crate::amount::Amount;
use crate::amount::render::{AmountFormat, show_amount_b};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::lines_with_title;
use crate::cli::opts::{CliOpts, Declarables, find_matched_by_argument};
use crate::decimal::Decimal;
use crate::model::Journal;
use crate::query::Query;

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "commodities",
    aliases: &["comm"],
    flags: &[
        Flag::new(&["used"], NONE, "", "list commodities used in transactions"),
        Flag::new(
            &["priced"],
            NONE,
            "",
            "list commodities appearing in P directives",
        ),
        Flag::new(
            &["declared"],
            NONE,
            "",
            "list commodities declared by commodity directives",
        ),
        Flag::new(
            &["undeclared"],
            NONE,
            "",
            "list commodities used or priced but not declared",
        ),
        Flag::new(
            &["unused"],
            NONE,
            "",
            "list commodities declared but not used or priced",
        ),
        Flag::new(
            &["find"],
            NONE,
            "",
            "list the first commodity matched by the first argument (a case-insensitive infix regexp)",
        ),
        Flag::new(
            &["directives"],
            NONE,
            "",
            "show as commodity directives (with their display styles), for use in journals",
        ),
    ],
};

/// Run the commodities command (commodities): the commodities declared,
/// used in transactions (postings and their costs) or in P directives, or
/// with --used, --priced, --declared, --undeclared, --unused or --find
/// just those, sorted. A date query narrows the used and priced ones to
/// the matching transactions and directives, and leaves out the
/// declarations (which have no date) except for --declared and --unused;
/// the other query terms select from the names.
pub fn commodities(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let query = &opts.spec.query;
    let dateq = query.clone().filter(Query::is_date);
    let nondateq = query.clone().filter(|q| !q.is_date());
    let used_matched: BTreeSet<&str> = j
        .txns
        .iter()
        .filter(|t| dateq.matches_transaction(t))
        .flat_map(|t| &t.postings)
        .flat_map(|p| p.amount.amounts_raw())
        .flat_map(Amount::and_cost)
        .map(|a| &*a.commodity)
        .collect();
    let priced_matched: BTreeSet<&str> = j
        .price_directives
        .iter()
        .filter(|pd| dateq.matches_price_directive(pd))
        .flat_map(|pd| [&*pd.commodity, &*pd.amount.commodity])
        .collect();
    let refd_matched: BTreeSet<&str> = used_matched.union(&priced_matched).copied().collect();
    let refd_all: BTreeSet<&str> = j
        .posting_and_cost_amounts()
        .map(|a| &*a.commodity)
        .chain(
            j.price_directives
                .iter()
                .flat_map(|pd| [&*pd.commodity, &*pd.amount.commodity]),
        )
        .collect();
    let declared: BTreeSet<&str> = j.declared_commodities.keys().map(|c| &**c).collect();
    let filt = |cs: BTreeSet<&str>| -> Vec<String> {
        let tags = |c: &str| {
            j.declared_commodity_tags
                .get(c)
                .map_or(&[][..], Vec::as_slice)
        };
        cs.into_iter()
            .filter(|c| nondateq.matches_commodity_extra(&tags, c))
            .map(str::to_string)
            .collect()
    };
    // --priced is this command's own, so its exclusivity is checked here
    let names: Vec<String> = match (raw.bool("priced"), opts.declarables()?) {
        (true, None) => filt(priced_matched),
        (true, Some(_)) => {
            return Err("please pick at most one of --used, --priced, --declared, --undeclared, --unused, --find".to_string());
        }
        (false, None) if dateq.is_null() => filt(declared.union(&refd_all).copied().collect()),
        (false, None) => filt(refd_matched),
        (false, Some(Declarables::Used)) => filt(used_matched),
        (false, Some(Declarables::Declared)) => filt(declared),
        (false, Some(Declarables::Undeclared)) => {
            filt(refd_matched.difference(&declared).copied().collect())
        }
        (false, Some(Declarables::Unused)) => {
            filt(declared.difference(&refd_all).copied().collect())
        }
        // a name lookup over all of them, the query not applying
        (false, Some(Declarables::FindFirst)) => {
            let all = declared.union(&refd_all).copied();
            vec![find_matched_by_argument(raw, "commodity", all)?.to_string()]
        }
    };
    if !raw.bool("directives") {
        return Ok(lines_with_title(opts, names));
    }
    let styles = j.commodity_styles();
    let directive = |c: &str| {
        let style = styles.get(c).cloned().unwrap_or_default();
        let mark = style.decimal_mark.unwrap_or('.');
        let sample = Amount {
            commodity: c.into(),
            quantity: Decimal::from_i64(1000),
            style,
            ..Amount::null()
        };
        let fmt = AmountFormat {
            force_decimal_mark: true,
            ..AmountFormat::DEFAULT
        };
        format!(
            "commodity {}",
            ensure_decimal_mark(c, mark, show_amount_b(&fmt, &sample).s)
        )
    };
    Ok(lines_with_title(opts, names.iter().map(|c| directive(c))))
}

/// A rendered amount with a decimal mark after its last digit, if its
/// number (the text without the commodity symbol) has none, as a commodity
/// directive requires.
fn ensure_decimal_mark(c: &str, mark: char, t: String) -> String {
    let number = if c.is_empty() {
        t.clone()
    } else {
        t.replace(c, "")
    };
    if number.contains(mark) {
        return t;
    }
    match t.char_indices().rev().find(|(_, ch)| ch.is_ascii_digit()) {
        Some((i, ch)) => {
            let n = i + ch.len_utf8();
            format!("{}{mark}{}", &t[..n], &t[n..])
        }
        None => t,
    }
}
