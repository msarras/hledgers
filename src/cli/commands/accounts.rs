//! The accounts command (Hledger.Cli.Commands.Accounts): the account names
//! used by the matched postings or declared, as a flat list (full names,
//! depth-clipped, maybe with leading components dropped) or a tree
//! (indented leaf names, with their parents).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::account::{
    account_leaf_name, account_name_drop, account_name_level, clip_account_name,
    expand_account_name, sort_account_names_by_declaration,
};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::lines_with_title;
use crate::cli::opts::{CliOpts, Declarables, find_matched_by_argument, flat_flag, tree_flag};
use crate::model::{Journal, Posting, PostingRef, Tag};
use crate::query::Query;
use crate::reports::opts::AccountListMode;

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "accounts",
    aliases: &["acc"],
    flags: &[
        Flag::new(&["used"], NONE, "", "list accounts used").short(&['u']),
        Flag::new(&["declared"], NONE, "", "list accounts declared").short(&['d']),
        Flag::new(
            &["undeclared"],
            NONE,
            "",
            "list accounts used but not declared",
        ),
        Flag::new(&["unused"], NONE, "", "list accounts declared but not used"),
        Flag::new(
            &["find"],
            NONE,
            "",
            "list the first account matched by the first argument (a case-insensitive infix regexp)",
        ),
        Flag::new(
            &["directives"],
            NONE,
            "",
            "show as account directives, for use in journals",
        ),
        Flag::new(
            &["locations"],
            NONE,
            "",
            "also show where accounts were declared",
        ),
        Flag::new(&["types"], NONE, "", "also show account types when known"),
        flat_flag(false),
        tree_flag(false),
        Flag::new(
            &["drop"],
            REQ,
            "N",
            "flat mode: omit N leading account name parts",
        ),
        Flag::new(
            &["positions"],
            NONE,
            "",
            "deprecated, use --locations instead",
        )
        .sets(&["locations"])
        .hidden(),
    ],
    args: Some("[QUERY..]"),
    doc: crate::command_doc!("Accounts"),
    ..Mode::DEFAULT
};

/// Run the accounts command (accounts).
pub fn accounts(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let ropts = &opts.spec.opts;
    let query = &opts.spec.query;
    let tree = ropts.account_list_mode == AccountListMode::Tree;
    // a depth limit clips and excludes names later, not here
    let nodepthq = query.clone().filter(|q| !q.is_depth());
    // the acct: terms are applied again after clipping
    let acctq = query.clone().filter(Query::is_acct);
    let depth = query.clone().filter(Query::is_depth).depth();
    let types = |a: &str| j.account_type(a);

    // 1. the accounts to show: posted to by the matched postings, whose
    // tags for this are their accounts' tags only (journalPostingsKeepAccountTagsOnly)
    let matched_used = nub(j.txns.iter().flat_map(|t| {
        t.postings
            .iter()
            .filter(|p| {
                let p1 = Posting {
                    tags: j.inherited_account_tags(&p.account),
                    ..(*p).clone()
                };
                nodepthq.matches_posting_extra(&types, PostingRef::new(t, &p1))
            })
            .map(|p| &*p.account)
    }));
    let declared: Vec<&str> = j.declared_account_names().map(|a| &**a).collect();
    let inherited_tags: HashMap<&str, Vec<Tag>> = declared
        .iter()
        .map(|a| (*a, j.inherited_account_tags(a)))
        .collect();
    let tags = |a: &str| inherited_tags.get(a).map_or(&[][..], Vec::as_slice);
    let matched_declared = nub(declared
        .iter()
        .copied()
        .filter(|a| nodepthq.matches_account_extra(&types, &tags, a)));
    // unused and undeclared are relative to all accounts used or declared,
    // so that eg a date: query can't make a declared account look undeclared
    let accts: Vec<&str> = match opts.declarables()? {
        None => matched_declared.into_iter().chain(matched_used).collect(),
        Some(Declarables::Used) => matched_used,
        Some(Declarables::Declared) => matched_declared,
        Some(Declarables::Undeclared) => {
            let all: HashSet<&str> = declared.iter().copied().collect();
            matched_used
                .into_iter()
                .filter(|a| !all.contains(a))
                .collect()
        }
        Some(Declarables::Unused) => {
            let all: HashSet<&str> = j.postings().map(|p| &*p.p.account).collect();
            matched_declared
                .into_iter()
                .filter(|a| !all.contains(a))
                .collect()
        }
        Some(Declarables::FindFirst) => {
            // journalAccountNamesDeclaredOrImplied
            let names: BTreeSet<&str> = declared
                .iter()
                .copied()
                .chain(j.postings().flat_map(|p| expand_account_name(&p.p.account)))
                .collect();
            vec![find_matched_by_argument(raw, "account", names)?]
        }
    };

    // 2. sorted by declaration order within each group of siblings, with
    // the parents in tree mode
    let declared = (j.declared_accounts.iter()).map(|(a, info)| (&**a, info.order));
    let sorted = sort_account_names_by_declaration(declared, tree, accts);

    // 3. clipped to the depth limit, keeping (in flat mode) those still
    // matched by the acct: terms
    let mut seen = HashSet::new();
    let shown: Vec<&str> = sorted
        .into_iter()
        .map(|a| clip_account_name(&depth, a))
        .filter(|a| !a.is_empty() && seen.insert(*a))
        .filter(|a| tree || acctq.matches_account(a))
        .collect();

    // 4. rendered, with the types, aliases and declaration places asked for
    let drop = ropts.drop;
    let names: Vec<String> = shown
        .iter()
        .map(|a| {
            let dropped = account_name_drop(drop, a);
            if tree {
                let indent = 2 * account_name_level(a).saturating_sub(drop).saturating_sub(1);
                format!("{}{}", " ".repeat(indent), account_leaf_name(&dropped))
            } else {
                dropped
            }
        })
        .collect();
    let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let directives = raw.bool("directives");
    let lines = shown.iter().zip(&names).map(|(a, name)| {
        let mut items = Vec::new();
        if raw.bool("types")
            && let Some(t) = j.account_type(a)
        {
            items.push(format!("type: {t}"));
        }
        if directives {
            for (n, v) in j.declared_account_tags.get(*a).into_iter().flatten() {
                if n.to_lowercase() == "alias" {
                    items.push(format!("alias: {v}"));
                }
            }
        }
        if raw.bool("locations") {
            items.push(match j.declared_accounts.iter().find(|(d, _)| &**d == *a) {
                Some((_, info)) => format!(
                    "declared at {}, overall declaration order {}",
                    info.pos, info.order
                ),
                None => "undeclared".to_string(),
            });
        }
        let keyword = if directives { "account " } else { "" };
        if items.is_empty() {
            format!("{keyword}{name}")
        } else {
            let pad = " ".repeat(width - name.chars().count());
            format!("{keyword}{name}{pad}    ; {}", items.join(", "))
        }
    });
    Ok(lines_with_title(opts, lines))
}

/// The names without duplicates, in their first occurrences' order.
fn nub<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    names.into_iter().filter(|a| seen.insert(*a)).collect()
}
