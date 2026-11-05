//! Finalising a parsed journal (journalFinalise in Hledger.Read.Common, with
//! the stages of Hledger.Data.Journal): a pipeline of stages in upstream's
//! order that infers missing information, checks it, and adds derived data.
//!
//! The parser keeps every list in file order, so upstream's journalReverse
//! has nothing to do here. The stages of milestone 2c (and the lot stages of
//! milestone 6) are present, returning the journal unchanged until then.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::InputOpts;
use super::journal::ParseState;
use crate::account::{AccountName, AccountType, account_name_infer_type, expand_account_names};
use crate::amount::{Amount, CommodityStyles, Rounding, add_amount_style, styles_with_rounding};
use crate::model::{Journal, Tag};

/// Finalise a journal parsed from this file and text (journalFinalise).
pub fn journal_finalise(
    iopts: &InputOpts,
    file: &str,
    text: &str,
    mut j: Journal,
    st: ParseState,
) -> Result<Journal, String> {
    j.global_commodity_styles = iopts.balancing.commodity_styles.clone().unwrap_or_default();
    j.default_commodity = st.default_commodity;
    // setup
    journal_set_last_read_time(&mut j);
    journal_add_file(&mut j, file, text);
    // account types and amount styles
    journal_add_account_types(&mut j);
    journal_style_amounts(&mut j)?;
    let mut j = journal_apply_commodity_aliases(j)?;
    // forecast and account tags
    j = journal_add_forecast(iopts, j);
    journal_postings_add_account_tags(&mut j);
    // pre-balancing cost and equity tagging
    j = journal_tag_costs_and_equity_and_maybe_infer_costs(iopts, false, j)?;
    // lot cost basis and transacted cost inference, gain posting tagging
    j = journal_infer_basis_from_account_names(j)?;
    j = journal_infer_postings_transacted_cost(j);
    j = journal_check_acquire_basis(j)?;
    j = journal_tag_gain_postings(j)?;
    // auto postings
    if iopts.auto && !j.txn_modifiers.is_empty() {
        j = journal_add_auto_postings(iopts, j)?;
    }
    // transaction balancing
    j = journal_check_ordereddates(j)?;
    let (mut j, assertion_error) = journal_balance_transactions_and_defer_assertions(iopts, j)?;
    // lot classification
    j = journal_classify_lot_postings(j);
    // post-balancing enrichment
    journal_infer_commodity_styles(&mut j);
    journal_postings_add_commodity_tags(&mut j);
    // cost inference
    if iopts.infer_costs {
        j = journal_tag_costs_and_equity_and_maybe_infer_costs(iopts, true, j)?;
    }
    // market prices and renumbering
    j = journal_infer_market_prices_from_transactions(j);
    journal_renumber_account_declarations(&mut j);
    // lots and capital gains
    j = journal_calculate_and_check_lots(j)?;
    // equity inference
    if iopts.infer_equity {
        j = journal_infer_equity_from_costs(iopts, j);
    }
    match assertion_error {
        Some(e) => Err(e),
        None => Ok(j),
    }
}

/// Note when the journal was read (journalSetLastReadTime).
fn journal_set_last_read_time(j: &mut Journal) {
    j.last_read_time = Some(std::time::SystemTime::now());
}

/// Record the main file and its text, before the files it included
/// (journalAddFile).
fn journal_add_file(j: &mut Journal, file: &str, text: &str) {
    j.files.insert(0, (file.to_string(), text.to_string()));
}

/// Find the type of every account, declared or used: its own declared
/// type, or else a declared type inherited from its parent, or else the
/// type its name suggests (unless some account was declared with that
/// type), or else an inferred type inherited from its parent
/// (journalAddAccountTypes, journalAccountTypes).
pub fn journal_add_account_types(j: &mut Journal) {
    let declared: BTreeMap<&str, AccountType> = j
        .declared_account_types
        .iter()
        .flat_map(|(t, accts)| accts.iter().map(move |a| (&**a, *t)))
        .collect();
    let declared_types: BTreeSet<AccountType> = j.declared_account_types.keys().copied().collect();
    let names = journal_account_names(j);
    // each account's type and whether it was declared (inherited or not)
    let mut types: BTreeMap<&str, (AccountType, bool)> = BTreeMap::new();
    for a in names.iter().map(|a| &**a) {
        let parent = a.rfind(':').and_then(|i| types.get(&a[..i]).copied());
        let t = declared
            .get(a)
            .map(|t| (*t, true))
            .or(parent.filter(|(_, d)| *d))
            .or(account_name_infer_type(a)
                .filter(|t| !declared_types.contains(t))
                .map(|t| (t, false)))
            .or(parent);
        if let Some(t) = t {
            types.insert(a, t);
        }
    }
    j.account_types = types
        .into_iter()
        .map(|(a, (t, _))| (AccountName::from(a), t))
        .collect();
}

/// The account names declared or posted to, with all their parents, sorted
/// (journalAccountNamesDeclaredOrImplied).
pub fn journal_account_names(j: &Journal) -> Vec<AccountName> {
    let used: BTreeSet<&str> = j
        .txns
        .iter()
        .flat_map(|t| t.postings.iter().map(|p| &*p.account))
        .collect();
    let mut names: BTreeSet<&str> = expand_account_names(used).into_iter().collect();
    names.extend(j.declared_accounts.iter().map(|(a, _)| &**a));
    names.into_iter().map(AccountName::from).collect()
}

/// Infer the commodity styles, and apply them to the posting amounts
/// without changing their precisions (journalStyleAmounts).
fn journal_style_amounts(j: &mut Journal) -> Result<(), String> {
    journal_infer_commodity_styles(j);
    let styles = styles_with_rounding(Rounding::No, &j.commodity_styles());
    for t in &mut j.txns {
        t.style_amounts(&styles);
    }
    Ok(())
}

/// Infer each commodity's display style from the amounts of the postings
/// and of the D and P directives, the directives' winning; with the
/// precision from the amounts that were written (journalInferCommodityStyles).
pub fn journal_infer_commodity_styles(j: &mut Journal) {
    // amounts are added last first, so the first one's style wins
    let (mut all, mut explicit) = (CommodityStyles::new(), CommodityStyles::new());
    for p in j.txns.iter().rev().flat_map(|t| t.postings.iter().rev()) {
        for a in p.amount.amounts_raw().iter().rev() {
            add_amount_style(a, &mut all);
            if p.is_explicit_amount() {
                add_amount_style(a, &mut explicit);
            }
        }
    }
    let default_amount = j.default_commodity.as_ref().map(|(c, s)| Amount {
        commodity: c.clone(),
        style: s.clone(),
        ..Amount::null()
    });
    let directive_amounts: Vec<&Amount> = default_amount
        .iter()
        .chain(j.price_directives.iter().map(|p| &p.amount))
        .collect();
    for a in directive_amounts.iter().rev() {
        add_amount_style(a, &mut all);
        add_amount_style(a, &mut explicit);
    }
    for (c, s) in all {
        let s = match explicit.get(&c) {
            Some(e) if e.precision != s.precision => Arc::new(crate::amount::AmountStyle {
                precision: e.precision,
                ..(*s).clone()
            }),
            _ => s,
        };
        j.inferred_commodity_styles.insert(c, s);
    }
}

/// Convert amounts in commodity aliases to their commodity
/// (journalApplyCommodityAliases).
// M2c: commodity aliases; the journal is unchanged until then.
fn journal_apply_commodity_aliases(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Add forecast transactions, if --forecast is given (journalAddForecast).
// M2c: periodic transactions and forecasting; the journal is unchanged until then.
fn journal_add_forecast(_iopts: &InputOpts, j: Journal) -> Journal {
    j
}

/// Add to each posting the tags of its account and its account's parents,
/// where it does not have them already; they are queryable but not shown
/// (journalPostingsAddAccountTags).
fn journal_postings_add_account_tags(j: &mut Journal) {
    if j.declared_account_tags.is_empty() {
        return;
    }
    let mut inherited: BTreeMap<AccountName, Vec<Tag>> = BTreeMap::new();
    for t in &mut j.txns {
        for p in &mut t.postings {
            let tags = inherited.entry(p.account.clone()).or_insert_with(|| {
                let mut tags: Vec<Tag> = Vec::new();
                let a = &*p.account;
                for name in std::iter::once(a).chain(crate::account::parent_account_names(a)) {
                    for tag in j.declared_account_tags.get(name).into_iter().flatten() {
                        if !tags.contains(tag) {
                            tags.push(tag.clone());
                        }
                    }
                }
                tags
            });
            if !tags.is_empty() {
                p.add_tags(tags.iter().cloned());
            }
        }
    }
}

/// Tag equity conversion postings and redundant costs, and with
/// `infer_costs`, infer costs from equity postings
/// (journalTagCostsAndEquityAndMaybeInferCosts).
// M2c: cost and equity tagging and cost inference; the journal is unchanged until then.
fn journal_tag_costs_and_equity_and_maybe_infer_costs(
    _iopts: &InputOpts,
    _infer_costs: bool,
    j: Journal,
) -> Result<Journal, String> {
    Ok(j)
}

/// Infer cost bases from lot subaccount names
/// (journalInferBasisFromAccountNames).
// M6: lots; the journal is unchanged until then.
fn journal_infer_basis_from_account_names(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Infer a transacted cost from the cost basis in acquire-shaped postings
/// (journalInferPostingsTransactedCost).
// M6: lots; the journal is unchanged until then.
fn journal_infer_postings_transacted_cost(j: Journal) -> Journal {
    j
}

/// Check that acquire-shaped postings' cost bases and costs agree
/// (journalCheckAcquireBasis).
// M6: lots; the journal is unchanged until then.
fn journal_check_acquire_basis(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Tag user-written gain postings in disposals (journalTagGainPostings).
// M6: lots; the journal is unchanged until then.
fn journal_tag_gain_postings(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Add auto postings (journalAddAutoPostings).
// M2c: auto postings; the journal is unchanged until then.
fn journal_add_auto_postings(_iopts: &InputOpts, j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// With `hledger check ordereddates`, check that transactions are in date
/// order (journalCheckOrdereddates).
// M2c: the check family; the journal is unchanged until then.
fn journal_check_ordereddates(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Balance the transactions, inferring missing amounts and balance
/// assignments, and check balance assertions, returning the first
/// assertion failure separately so that later errors are reported first
/// (journalBalanceTransactionsAndDeferAssertions).
// M2c: balancing, amount inference, assertions and assignments; the journal is unchanged until then.
fn journal_balance_transactions_and_defer_assertions(
    _iopts: &InputOpts,
    j: Journal,
) -> Result<(Journal, Option<String>), String> {
    Ok((j, None))
}

/// Detect and classify lot postings (journalClassifyLotPostings).
// M6: lots; the journal is unchanged until then.
fn journal_classify_lot_postings(j: Journal) -> Journal {
    j
}

/// Add to each posting the tags of its amounts' commodities, where it does
/// not have them already (journalPostingsAddCommodityTags).
fn journal_postings_add_commodity_tags(j: &mut Journal) {
    if j.declared_commodity_tags.values().all(Vec::is_empty) {
        return;
    }
    for t in &mut j.txns {
        for p in &mut t.postings {
            let tags: Vec<Tag> = p
                .commodities()
                .iter()
                .flat_map(|c| j.declared_commodity_tags.get(c).into_iter().flatten())
                .cloned()
                .collect();
            if !tags.is_empty() {
                p.add_tags(tags);
            }
        }
    }
}

/// Infer market prices from the costs in transactions
/// (journalInferMarketPricesFromTransactions).
// M2c: market prices from transactions; the journal is unchanged until then.
fn journal_infer_market_prices_from_transactions(j: Journal) -> Journal {
    j
}

/// Number the account declarations in order across all files
/// (journalRenumberAccountDeclarations).
pub fn journal_renumber_account_declarations(j: &mut Journal) {
    for (i, (_, info)) in j.declared_accounts.iter_mut().enumerate() {
        info.order = i + 1;
    }
}

/// Check lot tags and methods, calculate lots and add or check gain
/// postings, and remove the balancer's copied bases (journalCheckLotsTagValues,
/// journalCheckLotsMethodCoherence, journalCalculateLots,
/// journalAddOrCheckGainPostings, journalStripBalancerCopiedBases).
// M6: lots; the journal is unchanged until then.
fn journal_calculate_and_check_lots(j: Journal) -> Result<Journal, String> {
    Ok(j)
}

/// Infer equity postings from costs (journalInferEquityFromCosts).
// M2c: equity inference; the journal is unchanged until then.
fn journal_infer_equity_from_costs(_iopts: &InputOpts, j: Journal) -> Journal {
    j
}

/// Number the transactions from 1 and record in each posting its
/// transaction's position (journalNumberAndTieTransactions).
pub fn journal_number_and_tie_transactions(j: &mut Journal) {
    for (i, t) in j.txns.iter_mut().enumerate() {
        t.index = i as u32 + 1;
        for p in &mut t.postings {
            p.txn = Some(i as u32);
        }
    }
}

/// Two journals read from separate files combined, the second's parse state
/// kept (journalConcat).
pub fn journal_concat(mut j1: Journal, j2: Journal) -> Journal {
    j1.declared_payees.extend(j2.declared_payees);
    j1.declared_tags.extend(j2.declared_tags);
    j1.declared_accounts.extend(j2.declared_accounts);
    for (a, tags) in j2.declared_account_tags {
        j1.declared_account_tags.entry(a).or_default().extend(tags);
    }
    for (t, accts) in j2.declared_account_types {
        j1.declared_account_types
            .entry(t)
            .or_default()
            .extend(accts);
    }
    j1.account_types.extend(j2.account_types);
    for (k, v) in j2.global_commodity_styles {
        j1.global_commodity_styles.entry(k).or_insert(v);
    }
    for (c, d) in j2.declared_commodities {
        match j1.declared_commodities.remove(&c) {
            None => {
                j1.declared_commodities.insert(c, d);
            }
            Some(d1) => {
                j1.declared_commodities.insert(c, commodity_combine(d1, d));
            }
        }
    }
    for (c, tags) in j2.declared_commodity_tags {
        j1.declared_commodity_tags
            .entry(c)
            .or_default()
            .extend(tags);
    }
    for (k, v) in j2.inferred_commodity_styles {
        j1.inferred_commodity_styles.entry(k).or_insert(v);
    }
    j1.price_directives.extend(j2.price_directives);
    j1.inferred_market_prices.extend(j2.inferred_market_prices);
    j1.txn_modifiers.extend(j2.txn_modifiers);
    j1.periodic_txns.extend(j2.periodic_txns);
    j1.txns.extend(j2.txns);
    j1.items.extend(j2.items);
    j1.files.extend(j2.files);
    j1.default_commodity = j2.default_commodity;
    j1.last_read_time = j1.last_read_time.max(j2.last_read_time);
    journal_renumber_account_declarations(&mut j1);
    j1
}

/// A commodity declared in two files, the first declaration preferred
/// (commodityCombine).
pub fn commodity_combine(
    c1: crate::model::Commodity,
    c2: crate::model::Commodity,
) -> crate::model::Commodity {
    let mut tags = c1.tags.clone();
    for t in c2.tags {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    let mut aliases = c2.aliases;
    for (a, p) in c1.aliases.iter() {
        if !aliases
            .iter()
            .any(|(b, _)| b.commodity == a.commodity && b.quantity == a.quantity)
        {
            aliases.push((a.clone(), p.clone()));
        }
    }
    crate::model::Commodity {
        format: c1.format.clone().or(c2.format),
        tags,
        aliases,
        ..c1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::{Precision, Side};
    use crate::dates::Day;

    fn read(s: &str) -> Journal {
        let iopts = InputOpts {
            today: Day::from_ymd(2026, 1, 1).unwrap(),
            ..InputOpts::default()
        };
        let (j, st) = super::super::journal::parse_journal(&iopts, "f.j", s).unwrap();
        journal_finalise(&iopts, "f.j", s, j, st).unwrap()
    }

    #[test]
    fn account_types() {
        let j = read(concat!(
            "account myassets  ; type:A\n",
            "2000-01-01\n  myassets:bank  1\n  revenues:gain  -1\n  expenses:food\n  other\n",
        ));
        let t = |a: &str| j.account_types.get(a).copied();
        assert_eq!(t("myassets:bank"), Some(AccountType::Asset));
        assert_eq!(t("revenues:gain"), Some(AccountType::Gain));
        assert_eq!(t("expenses"), Some(AccountType::Expense));
        assert_eq!(t("other"), None);
        // a type declared for one account is not inferred for others
        let j = read("account a  ; type:X\n2000-01-01\n  expenses:x  1\n  a  -1\n");
        assert_eq!(j.account_types.get("expenses:x"), None);
    }

    #[test]
    fn commodity_styles_and_tags() {
        let j = read(concat!(
            "commodity EUR 1.000,00\n",
            "account a  ; t1: v\n",
            "2000-01-01\n  a:b  $1.5\n  c  $-1,000.555\n  d  EUR 5\n",
            "P 2000-01-01 X 1.0 Y\n",
        ));
        let styles = j.commodity_styles();
        let usd = &styles["$"];
        assert_eq!((usd.side, usd.precision), (Side::L, Precision::Fixed(3)));
        assert_eq!(styles["EUR"].decimal_mark, Some(','));
        assert_eq!(styles["Y"].precision, Precision::Fixed(1));
        // amounts get their commodity's style but keep their precision
        let a = &j.txns[0].postings[2].amount.amounts_raw()[0];
        assert_eq!(
            (a.style.decimal_mark, a.style.precision),
            (Some(','), Precision::Fixed(0))
        );
        assert_eq!(j.txns[0].postings[0].tags, [("t1".into(), "v".into())]);
        assert!(j.txns[0].postings[1].tags.is_empty());
        assert_eq!(j.files[0].0, "f.j");
    }
}
