//! The tags command (Hledger.Cli.Commands.Tags): the tag names, or
//! values, used in the matched transactions and account declarations, or
//! declared.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::lines_with_title;
use crate::cli::opts::{CliOpts, Declarables, find_matched_by_argument};
use crate::model::{Journal, Tag};
use crate::query::{Query, parse_query_list};
use crate::regex::to_regex_ci;
use crate::reports::opts::usage_error;

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "tags",
    aliases: &[],
    flags: &[
        Flag::new(&["used"], NONE, "", "list tags used"),
        Flag::new(&["declared"], NONE, "", "list tags declared"),
        Flag::new(&["undeclared"], NONE, "", "list tags used but not declared"),
        Flag::new(&["unused"], NONE, "", "list tags declared but not used"),
        Flag::new(
            &["find"],
            NONE,
            "",
            "list the first tag whose name is matched by the first argument (a case-insensitive infix regexp)",
        ),
        Flag::new(
            &["values"],
            NONE,
            "",
            "list tag values instead of tag names",
        ),
        Flag::new(
            &["parsed"],
            NONE,
            "",
            "show them in the order they were parsed (mostly), including duplicates",
        ),
        Flag::new(
            &["directives"],
            NONE,
            "",
            "show tag names as tag directives, for use in journals",
        ),
    ],
    args: Some("[TAGREGEX [QUERY..]]"),
    doc: crate::command_doc!("Tags"),
    ..Mode::DEFAULT
};

/// Run the tags command (tags): the first argument is a regular
/// expression the tag names must match, the others a query selecting the
/// transactions, and (unless it is about transactions only) the account
/// declarations, whose tags are listed with the declared tags it matches;
/// or with --used, --declared, --undeclared, --unused or --find just
/// those. Sorted without duplicates unless --parsed; with --values their
/// non-empty values (all with --empty) instead.
pub fn tags(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let ropts = &opts.spec.opts;
    let args: Vec<String> = raw.args().map(str::to_string).collect();
    let tagpat = args.first().map(|a| to_regex_ci(a)).transpose()?;
    let values = raw.bool("values");
    let directives = raw.bool("directives") && !values;
    let (query, _) = parse_query_list(opts.spec.today, &args[args.len().min(1)..])
        .map_err(|e| usage_error(&e))?;
    let q = Query::And(vec![ropts.query_from_flags(), query]).simplify();

    // the tags of the matched account declarations (their own, though
    // they are matched with their inherited ones), unless the query is
    // about something only transactions have, like a date or amount
    let mut used: Vec<Tag> = Vec::new();
    if !q.is_transaction_related() {
        let types = |a: &str| j.account_type(a);
        let inherited: HashMap<&str, Vec<Tag>> = j
            .declared_account_names()
            .map(|a| (&**a, j.inherited_account_tags(a)))
            .collect();
        let tags = |a: &str| inherited.get(a).map_or(&[][..], Vec::as_slice);
        for a in j.declared_account_names() {
            if q.matches_account_extra(&types, &tags, a) {
                used.extend(
                    j.declared_account_tags
                        .get(a)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        }
    }
    let declared: Vec<Tag> = j
        .declared_tags
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|n| (n.to_string(), String::new()))
        .collect();
    let j = opts.spec.journal_apply_valuation(j)?;
    for t in j.txns.iter().filter(|t| q.matches_transaction(t)) {
        used.extend(t.all_tags().cloned());
    }
    let filtered_declared: Vec<&Tag> = declared.iter().filter(|t| q.matches_tag(t)).collect();
    let tags: Vec<&Tag> = match opts.declarables()? {
        None => filtered_declared.into_iter().chain(&used).collect(),
        Some(Declarables::Used) => used.iter().collect(),
        Some(Declarables::Declared) => declared.iter().collect(),
        Some(Declarables::Undeclared) => {
            let names: HashSet<&str> = filtered_declared.iter().map(|t| t.0.as_str()).collect();
            used.iter()
                .filter(|t| !names.contains(t.0.as_str()))
                .collect()
        }
        Some(Declarables::Unused) => {
            let names: HashSet<&str> = used.iter().map(|t| t.0.as_str()).collect();
            filtered_declared
                .into_iter()
                .filter(|t| !names.contains(t.0.as_str()))
                .collect()
        }
        Some(Declarables::FindFirst) => {
            // the name first, then its first occurrence, so that --values
            // and --parsed still work in a stable way
            let all: Vec<&Tag> = declared.iter().chain(&used).collect();
            let names: BTreeSet<&str> = all.iter().map(|t| t.0.as_str()).collect();
            let name = find_matched_by_argument(raw, "tag name", names)?;
            all.into_iter().filter(|t| t.0 == name).take(1).collect()
        }
    };
    let results = tags.into_iter().filter_map(|(n, v)| {
        if tagpat.as_ref().is_some_and(|r| !r.is_match(n)) || values && v.is_empty() && !ropts.empty
        {
            return None;
        }
        Some(if values {
            v.clone()
        } else if directives {
            format!("tag {n}")
        } else {
            n.clone()
        })
    });
    Ok(if raw.bool("parsed") {
        lines_with_title(opts, results)
    } else {
        lines_with_title(opts, results.collect::<BTreeSet<_>>())
    })
}
