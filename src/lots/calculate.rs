//! Lot calculation and the stages around it (journalCalculateLots and its
//! helpers, journalCheckLotsMethodCoherence, journalAddOrCheckGainPostings
//! and journalStripBalancerCopiedBases from Hledger.Data.Lots;
//! journalCheckLotsTagValues from Hledger.Data.Journal).
//!
//! Transactions are walked in date order, keeping each commodity's lots:
//! acquisitions open a lot, named by its date, label and cost, as a
//! subaccount of the acquiring account; disposals select lots by the
//! account's reduction method, splitting into a posting per lot; transfers
//! move the selected lots to the destination accounts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::classify::{amount_cost_to_unit_cost, fold_map};
use super::{
    LotDirection, LotId, LotState, ReductionMethod, account_uses_no_lots, append_postings_read_as,
    commodity_uses_lots, has_balancer_copied_basis, is_acquire_posting, is_classified_posting,
    is_dispose_posting, is_gain_posting, is_lot_posting, is_none_lots_tag_value,
    is_transfer_from_posting, is_transfer_to_posting, is_unclassified_lotful_posting,
    lot_costs_match, lot_direction_or_long, merge_cost_basis_for_method, posting_at_err_prefix,
    posting_err_prefix, resolve_reduction_method, show_lot_name, show_lot_name_for_method,
    style_lot_cb_cost, txn_err_prefix, unclassified_lot_warning, widen_lot_cb_cost,
};
use crate::account::{
    AccountName, AccountType, DEFAULT_GAIN_ACCOUNT, account_name_type, lot_base_account,
    parent_account_names,
};
use crate::amount::render::{
    AmountFormat, show_mixed_amount_one_line, show_mixed_amounts_distinctly,
};
use crate::amount::{
    Amount, AmountCost, CommodityStyles, CostBasis, MixedAmount, Precision, Rounding, Sym,
    styles_with_rounding,
};
use crate::dates::Day;
use crate::decimal::Decimal;
use crate::model::errors::{
    account_tag_error_excerpt, commodity_tag_error_excerpt, transaction_error_excerpt,
};
use crate::model::{
    BalanceAssertion, FEESPLIT_POSTING_TAG, GENERATED_POSTING_TAG, Journal,
    LOT_PARENT_ASSERTION_TAG, LOTSPLIT_POSTING_TAG, Posting, Tag, Transaction,
};

const METHODS: &str = "FIFO, LIFO, HIFO, AVERAGE, SPECID, FIFOALL, LIFOALL, HIFOALL, AVERAGEALL";

/// A lot selected for a disposal or transfer: its id, its stored amount,
/// and the quantity taken from it.
type Selected = (LotId, Amount, Decimal);

/// One commodity's lots, each lot's balance in each account.
type CommodityLots = BTreeMap<LotId, BTreeMap<AccountName, Amount>>;

/// One commodity's transfer postings in a transaction: the indices of the
/// sources and of the destinations.
type TransferGroup = (Sym, Vec<usize>, Vec<usize>);

fn tag(name: &str, value: &str) -> Tag {
    (name.to_string(), value.to_string())
}

/// Check the lots: tag values of commodity and account declarations: a
/// commodity's may be empty, NONE is only for accounts, and the others
/// must name a reduction method (journalCheckLotsTagValues).
pub fn journal_check_lots_tag_values(j: &Journal) -> Result<(), String> {
    let unrecognised = |(f, l, ex): (String, usize, String), v: &str| {
        format!(
            "{f}:{l}:\n{ex}\nunrecognised lots: tag value {}.\nUse {METHODS}, or nothing (meaning FIFO)\n",
            crate::text::show_string(v)
        )
    };
    for (sym, tags) in &j.declared_commodity_tags {
        let Some(comm) = j.declared_commodities.get(sym) else {
            continue;
        };
        for (k, v) in tags {
            if k.to_lowercase() != "lots" || v.trim_matches(crate::text::is_space).is_empty() {
                continue;
            }
            let (f, l, ex) = commodity_tag_error_excerpt(comm);
            if is_none_lots_tag_value(v) {
                return Err(format!(
                    "{f}:{l}:\n{ex}\nlots: NONE is not supported on commodity declarations.\n\
                     To disable lot tracking of this commodity in particular accounts,\n\
                     add a lots: NONE tag to those accounts' declarations instead;\n\
                     to disable it everywhere, remove the commodity's lots: tag.\n"
                ));
            }
            if ReductionMethod::parse(v).is_none() {
                return Err(unrecognised((f, l, ex), v));
            }
        }
    }
    for (a, info) in &j.declared_accounts {
        for (k, v) in &info.tags {
            if k.to_lowercase() != "lots" || is_none_lots_tag_value(v) {
                continue;
            }
            let (f, l, ex) = account_tag_error_excerpt(a, info);
            if v.trim_matches(crate::text::is_space).is_empty() {
                return Err(format!(
                    "{f}:{l}:\n{ex}\n\
                     An account lots: tag sets the disposal order for lot-tracked commodities there,\n\
                     so it needs a value, one of {METHODS};\n\
                     or NONE, to disable lot tracking in this account.\n\
                     (A commodity lots: tag enables lot tracking, and can also set the disposal order.)\n"
                ));
            }
            if ReductionMethod::parse(v).is_none() {
                return Err(unrecognised((f, l, ex), v));
            }
        }
    }
    Ok(())
}

/// Check that a global (`*ALL`) reduction method, used by any account
/// holding a lot-tracked commodity (in one lot direction), is used by all
/// of them (journalCheckLotsMethodCoherence).
pub fn journal_check_lots_method_coherence(j: &Journal) -> Result<(), String> {
    let mut holdings: BTreeMap<(Sym, LotDirection), BTreeSet<&str>> = BTreeMap::new();
    for r in j.postings() {
        let acct = lot_base_account(&r.p.account);
        for a in r.p.amount.amounts_raw() {
            if a.cost_basis.is_some()
                || (commodity_uses_lots(j, &a.commodity) && !account_uses_no_lots(j, acct))
            {
                holdings
                    .entry((a.commodity, lot_direction_or_long(j, acct)))
                    .or_default()
                    .insert(acct);
            }
        }
    }
    for ((c, _), accts) in holdings {
        let rs: Vec<(&str, ReductionMethod, String)> = accts
            .iter()
            .map(|a| {
                let (m, src) = resolve_reduction_method(j, a, &c);
                (*a, m, src)
            })
            .collect();
        let Some((gacct, gmethod, gsrc)) = rs.iter().find(|(_, m, _)| m.is_global()) else {
            continue;
        };
        let conflicts: Vec<&(&str, ReductionMethod, String)> =
            rs.iter().filter(|(_, m, _)| m != gmethod).collect();
        if conflicts.is_empty() {
            continue;
        }
        let excerpt = method_declaration(j, gacct, &c)
            .map(|(f, l, ex)| format!("{f}:{l}:\n{ex}\n"))
            .unwrap_or_default();
        let lines: String = conflicts
            .iter()
            .map(|(a, m, src)| format!("  {a} uses {m} ({src})\n"))
            .collect();
        return Err(format!(
            "{excerpt}{c} uses the global method {gmethod} ({gsrc}) in {gacct},\n\
             but a different method elsewhere:\n{lines}\
             A global (*ALL) method must be used by every account holding the commodity.\n\
             Declare it once on the commodity declaration, and remove conflicting account lots: tags."
        ));
    }
    Ok(())
}

/// The declaration giving an account its method for a commodity, as an
/// excerpt: the nearest account declaration with a method-valued lots:
/// tag, or else the commodity's declaration.
fn method_declaration(j: &Journal, acct: &str, c: &Sym) -> Option<crate::model::errors::Excerpt> {
    let is_method_tag =
        |(k, v): &Tag| k.to_lowercase() == "lots" && ReductionMethod::parse(v).is_some();
    let declared = std::iter::once(acct)
        .chain(parent_account_names(acct))
        .find_map(|a| {
            let (name, info) = j.declared_accounts.iter().find(|(n, _)| &**n == a)?;
            info.tags.iter().any(is_method_tag).then_some((name, info))
        });
    match declared {
        Some((name, info)) => Some(account_tag_error_excerpt(name, info)),
        None => j
            .declared_commodities
            .get(c)
            .map(commodity_tag_error_excerpt),
    }
}

/// Calculate the lots: walk the transactions in date order, processing
/// their acquire, dispose and transfer postings, which get lot subaccounts
/// (journalCalculateLots). `tied` has the classified postings of each
/// transaction that could involve lots, as error messages find postings;
/// the others, with none, are left as they are. The transactions end up in
/// date order.
pub fn journal_calculate_lots(
    verbose: bool,
    mut j: Journal,
    mut tied: Vec<Option<Vec<Posting>>>,
) -> Result<Journal, String> {
    let txns = std::mem::take(&mut j.txns);
    if !txns.iter().any(|t| t.postings.iter().any(is_lot_posting)) {
        for t in &txns {
            for (i, p) in t.postings.iter().enumerate() {
                if is_unclassified_lotful_posting(&j, p) {
                    return Err(append_postings_read_as(
                        t,
                        unclassified_lot_warning(&j, t, i, p),
                    ));
                }
            }
        }
        j.txns = txns;
        return Ok(j);
    }
    validate_user_labels(&txns)?;
    let calc = Calc {
        j: &j,
        styles: styles_with_rounding(Rounding::No, &j.commodity_styles()),
        verbose,
        needs_labels: find_dates_needing_labels(&txns),
    };
    // the transactions in date order, each taken from its place
    let mut order: Vec<usize> = (0..txns.len()).collect();
    order.sort_by_key(|&i| txns[i].date);
    let mut txns: Vec<Option<Transaction>> = txns.into_iter().map(Some).collect();
    let mut ls = LotState::new();
    let mut out = Vec::with_capacity(txns.len());
    for i in order {
        let t = txns[i].take().expect("each transaction once");
        let Some(postings) = tied.get_mut(i).and_then(Option::take) else {
            out.push(t);
            continue;
        };
        let tied = Transaction {
            postings,
            ..t.clone()
        };
        let t2 = calc
            .process_transaction(&mut ls, &t, &tied)
            .map_err(|e| append_postings_read_as(&t, e))?;
        out.push(t2);
    }
    for (i, t) in out.iter_mut().enumerate() {
        for p in &mut t.postings {
            p.txn = Some(i as u32);
        }
    }
    j.txns = out;
    Ok(j)
}

/// Check that the labels users gave their acquisitions make unique lot ids
/// (validateUserLabels).
fn validate_user_labels(txns: &[Transaction]) -> Result<(), String> {
    let mut by_key: BTreeMap<(Sym, Day, std::sync::Arc<str>), Vec<&Transaction>> = BTreeMap::new();
    for t in txns {
        for p in t.postings.iter().filter(|p| is_acquire_posting(p)) {
            for a in p.amount.amounts_raw() {
                let Some(cb) = &a.cost_basis else { continue };
                let Some(l) = &cb.label else { continue };
                let key = (a.commodity, cb.date.unwrap_or(t.date), l.clone());
                by_key.entry(key).or_default().insert(0, t);
            }
        }
    }
    match by_key.into_iter().find(|(_, ts)| ts.len() > 1) {
        None => Ok(()),
        Some(((c, d, l), ts)) => {
            let (f, line, ex) = transaction_error_excerpt(ts[0], None);
            Err(format!(
                "{f}:{line}:\n{ex}\nlot id is not unique: commodity {c}, date {d}, label \"{l}\"\n"
            ))
        }
    }
}

/// The commodities and dates with several unlabelled acquisitions, which
/// need labels generated (findDatesNeedingLabels).
fn find_dates_needing_labels(txns: &[Transaction]) -> BTreeSet<(Sym, Day)> {
    let mut counts: BTreeMap<(Sym, Day), usize> = BTreeMap::new();
    for t in txns {
        for p in t.postings.iter().filter(|p| is_acquire_posting(p)) {
            for a in p.amount.amounts_raw() {
                if a.cost_basis.as_ref().is_some_and(|cb| cb.label.is_some()) {
                    continue;
                }
                let date = a
                    .cost_basis
                    .as_ref()
                    .and_then(|cb| cb.date)
                    .unwrap_or(t.date);
                *counts.entry((a.commodity, date)).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, _)| k)
        .collect()
}

/// A label for a lot on a date with several: the first four-digit number
/// not already a label of one of that date's lots (generateLabel).
fn generate_label(lots: Option<&CommodityLots>, date: Day) -> std::sync::Arc<str> {
    let used: BTreeSet<&str> = lots
        .into_iter()
        .flat_map(|lots| lots.range(LotId { date, label: None }..))
        .take_while(|(id, _)| id.date == date)
        .filter_map(|(id, _)| id.label.as_deref())
        .collect();
    let n = (1..)
        .find(|n| !used.contains(format!("{n:04}").as_str()))
        .unwrap_or(1);
    std::sync::Arc::from(format!("{n:04}"))
}

/// Add an amount to a lot's balance in an account, adding to any there
/// (addLotState).
fn add_lot_state(c: &Sym, id: LotId, acct: &str, amt: Amount, ls: &mut LotState) {
    let accts = ls.entry(*c).or_default().entry(id).or_default();
    match accts.get_mut(acct) {
        Some(old) => {
            let quantity = &amt.quantity + &old.quantity;
            *old = Amount { quantity, ..amt };
        }
        None => {
            accts.insert(AccountName::from(acct), amt);
        }
    }
}

/// Take consumed quantities from lots in an account, removing a lot's
/// account entry when it reaches zero (reduceLotState).
fn reduce_lot_state(acct: &str, c: &Sym, consumed: &[Selected], ls: &mut LotState) {
    let Some(lots) = ls.get_mut(c) else { return };
    for (id, _, q) in consumed {
        let Some(accts) = lots.get_mut(id) else {
            continue;
        };
        let Some(a) = accts.get_mut(acct) else {
            continue;
        };
        if a.quantity <= *q {
            accts.remove(acct);
        } else {
            a.quantity = &a.quantity - q;
        }
    }
}

fn is_wildcard_selector(cb: &CostBasis) -> bool {
    cb.date.is_none() && cb.label.is_none() && cb.cost.is_none()
}

/// Does a lot match a selector: each part the selector has matches the
/// lot's (lotMatchesSelector)?
fn lot_matches_selector(sel: &CostBasis, a: &Amount) -> bool {
    let Some(lot) = &a.cost_basis else {
        return false;
    };
    let cost_ok = match (&sel.cost, &lot.cost) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(s), Some(l)) => lot_costs_match(s, l),
    };
    cost_ok
        && sel.date.is_none_or(|d| lot.date == Some(d))
        && sel
            .label
            .as_ref()
            .is_none_or(|l| lot.label.as_ref() == Some(l))
}

/// A lot's cost basis, from its id and stored amount (lotIdToCb).
fn lot_id_to_cb(id: &LotId, a: &Amount) -> CostBasis {
    CostBasis {
        date: Some(id.date),
        label: id.label.clone(),
        cost: a.cost_basis.as_ref().and_then(|cb| cb.cost.clone()),
    }
}

/// Lots in the order a method takes them: oldest first, newest first, or
/// highest cost first (oldest first among equals).
fn order_lots<'a>(
    method: ReductionMethod,
    mut lots: Vec<(&'a LotId, &'a Amount)>,
) -> Vec<(&'a LotId, &'a Amount)> {
    match method.base_ordering() {
        ReductionMethod::Lifo => lots.reverse(),
        ReductionMethod::Hifo => {
            let cost = |a: &Amount| {
                a.cost_basis
                    .as_ref()
                    .and_then(|cb| cb.cost.as_ref())
                    .map_or(Decimal::ZERO, |c| c.quantity.clone())
            };
            lots.sort_by_key(|x| std::cmp::Reverse(cost(x.1)));
        }
        _ => {}
    }
    lots
}

/// The lots consumed, in order, for a quantity.
fn consume(qty: &Decimal, lots: Vec<(&LotId, &Amount)>) -> Vec<Selected> {
    let mut remaining = qty.clone();
    let mut out = Vec::new();
    for (id, a) in lots {
        if remaining.is_zero() {
            break;
        }
        if remaining >= a.quantity {
            out.push((id.clone(), a.clone(), a.quantity.clone()));
            remaining = &remaining - &a.quantity;
        } else {
            out.push((id.clone(), a.clone(), remaining));
            break;
        }
    }
    out
}

/// Lots one per line, at most 10, then a count of any more, and the total.
fn show_lot_list(lots: &BTreeMap<&LotId, &Amount>, c: &Sym) -> String {
    let mut out: String = lots
        .iter()
        .take(10)
        .map(|(id, a)| {
            format!(
                "\n  {}  {}",
                show_lot_name(&lot_id_to_cb(id, a)),
                a.quantity
            )
        })
        .collect();
    match lots.len().saturating_sub(10) {
        0 => {}
        1 => out.push_str("\n  ...and 1 more lot"),
        n => out.push_str(&format!("\n  ...and {n} more lots")),
    }
    let total = lots.values().fold(Decimal::ZERO, |s, a| &s + &a.quantity);
    out.push_str(&format!("\n  Total: {total} {c}"));
    out
}

/// A summary of a commodity's lots in accounts other than this one.
fn show_other_account_lots(all: &CommodityLots, acct: &str, c: &Sym) -> String {
    let mut by_acct: BTreeMap<&AccountName, (Decimal, usize)> = BTreeMap::new();
    for accts in all.values() {
        for (a, amt) in accts.iter().filter(|(a, _)| &***a != acct) {
            let e = by_acct.entry(a).or_insert((Decimal::ZERO, 0));
            *e = (&amt.quantity + &e.0, e.1 + 1);
        }
    }
    if by_acct.is_empty() {
        return String::new();
    }
    let mut out = format!("\nLots of {c} in other accounts:");
    for (a, (q, n)) in &by_acct {
        let lots = if *n == 1 { "lot" } else { "lots" };
        out.push_str(&format!("\n  {a}: {q} {c} in {n} {lots}"));
    }
    if by_acct.len() >= 2 {
        let total = by_acct.values().fold(Decimal::ZERO, |s, (q, _)| &s + q);
        out.push_str(&format!("\n  Total: {total} {c}"));
    }
    out
}

/// Select the lots of a commodity in an account to take a quantity from,
/// by the reduction method, among those the selector matches; the lots
/// are those of the lot direction (selectLots): the account's own, `flat`,
/// and all of them, made only for an error or a global method.
#[allow(clippy::too_many_arguments)]
fn select_lots(
    (method, source): (ReductionMethod, &str),
    pos: &dyn Fn() -> String,
    operation: &str,
    date: Day,
    direction: LotDirection,
    acct: &str,
    c: &Sym,
    qty: &Decimal,
    selector: &CostBasis,
    flat: BTreeMap<&LotId, &Amount>,
    lots: &dyn Fn() -> CommodityLots,
) -> Result<Vec<Selected>, String> {
    let method_line = format!("\nUsing {method} ({source}).");
    if method == ReductionMethod::Specid && is_wildcard_selector(selector) {
        return Err(format!(
            "{}SPECID requires an explicit lot selector{method_line}",
            pos()
        ));
    }
    let matching: BTreeMap<&LotId, &Amount> = flat
        .iter()
        .filter(|(_, a)| lot_matches_selector(selector, a))
        .map(|(id, a)| (*id, *a))
        .collect();
    if matching.is_empty() {
        let msg = if flat.is_empty() {
            let short_hint = if direction == LotDirection::Long && operation == "disposal" {
                "\n(To record a short position, use a liability account.)"
            } else {
                ""
            };
            format!(
                "no {c} lots available for {operation} from account {acct} on {date}{}{short_hint}",
                show_other_account_lots(&lots(), acct, c)
            )
        } else {
            format!(
                "no lots matching {} for commodity {c} in account {acct} on {date}\n\
                 Available lots in this account:{}",
                show_lot_name(selector),
                show_lot_list(&flat, c)
            )
        };
        return Err(pos() + &msg);
    }
    if method == ReductionMethod::Specid && matching.len() > 1 {
        return Err(format!(
            "{}lot selector is ambiguous, matches {} lots in account {acct}:{}{method_line}",
            pos(),
            matching.len(),
            show_lot_list(&matching, c)
        ));
    }
    let available = matching
        .values()
        .fold(Decimal::ZERO, |s, a| &s + &a.quantity);
    if available < *qty {
        let matched = if is_wildcard_selector(selector) {
            String::new()
        } else {
            format!(
                "\nLots matching {}:{}",
                show_lot_name(selector),
                show_lot_list(&matching, c)
            )
        };
        return Err(format!(
            "{}Insufficient lots for commodity {c} in account {acct}: need {qty} but only {available} available{matched}{}",
            pos(),
            show_other_account_lots(&lots(), acct, c)
        ));
    }
    let selected = consume(qty, order_lots(method, matching.into_iter().collect()));
    if method.is_global() {
        validate_global_compliance(method, pos, acct, qty, selector, &lots(), &selected)
            .map_err(|e| e + &method_line)?;
    }
    Ok(selected)
}

/// Check that the lots selected in an account are those a global ordering
/// across all accounts would take first (validateGlobalCompliance).
#[allow(clippy::too_many_arguments)]
fn validate_global_compliance(
    method: ReductionMethod,
    pos: &dyn Fn() -> String,
    acct: &str,
    qty: &Decimal,
    selector: &CostBasis,
    lots: &CommodityLots,
    selected: &[Selected],
) -> Result<(), String> {
    let flat: Vec<(&LotId, Amount)> = lots
        .iter()
        .filter_map(|(id, accts)| {
            let mut amts = accts.values();
            let first = amts.next()?;
            let rest = amts.fold(Decimal::ZERO, |s, a| &s + &a.quantity);
            Some((
                id,
                Amount {
                    quantity: &first.quantity + &rest,
                    ..first.clone()
                },
            ))
        })
        .filter(|(_, a)| lot_matches_selector(selector, a))
        .collect();
    let global = consume(
        qty,
        order_lots(method, flat.iter().map(|(id, a)| (*id, a)).collect()),
    );
    let selected_ids: BTreeSet<&LotId> = selected.iter().map(|(id, _, _)| id).collect();
    let elsewhere: BTreeSet<&LotId> = global
        .iter()
        .map(|(id, _, _)| id)
        .filter(|id| !selected_ids.contains(id))
        .collect();
    if elsewhere.is_empty() {
        return Ok(());
    }
    let mut by_acct: BTreeMap<&AccountName, Vec<(&LotId, &Amount)>> = BTreeMap::new();
    for id in &elsewhere {
        for (a, amt) in lots.get(*id).into_iter().flatten() {
            by_acct.entry(a).or_default().insert(0, (*id, amt));
        }
    }
    let accts: String = by_acct
        .iter()
        .map(|(a, ls)| {
            let ls: Vec<String> = ls
                .iter()
                .map(|(id, amt)| {
                    format!(
                        "{}  {}",
                        show_lot_name(&lot_id_to_cb(id, amt)),
                        amt.quantity
                    )
                })
                .collect();
            format!("\n  {a}: {}", ls.join(", "))
        })
        .collect();
    let sel: String = selected
        .iter()
        .map(|(id, a, q)| format!("\n  {}  {q}", show_lot_name(&lot_id_to_cb(id, a))))
        .collect();
    Err(format!(
        "{}{method}: lot(s) on other account(s) have higher priority than the lots in {acct}:{accts}\n\
         Selected from {acct}:{sel}\n\
         Consider disposing from the account(s) listed above first, or use {} for per-account scope.",
        pos(),
        method.base_ordering()
    ))
}

/// Under the average methods, add an acquisition to its pool: the new
/// average cost per unit, which every lot in the pool now has
/// (updatePoolOnAcquire).
fn update_pool_on_acquire(
    pos: &dyn Fn() -> String,
    in_pool: &dyn Fn(&str) -> bool,
    c: &Sym,
    acq_qty: &Decimal,
    acq_cost: &Amount,
    ls: &mut LotState,
) -> Result<Amount, String> {
    let lots = ls.entry(*c).or_default();
    let existing: Vec<(&Decimal, &Amount)> = lots
        .values()
        .flat_map(|accts| accts.iter().filter(|(a, _)| in_pool(a)))
        .filter_map(|(_, a)| Some((&a.quantity, a.cost_basis.as_ref()?.cost.as_deref()?)))
        .collect();
    if existing
        .first()
        .is_some_and(|(_, first)| first.commodity != acq_cost.commodity)
    {
        return Err(pos() + "cannot average lots with different cost commodities");
    }
    let total_qty = acq_qty + &existing.iter().fold(Decimal::ZERO, |s, (q, _)| &s + q);
    let total_cost = &(acq_qty * &acq_cost.quantity)
        + &existing
            .iter()
            .fold(Decimal::ZERO, |s, (q, c)| &s + &(*q * &c.quantity));
    if total_qty.is_zero() {
        return Err(pos() + "cannot average lots with zero total quantity");
    }
    let avg = Amount {
        quantity: &total_cost / &total_qty,
        ..acq_cost.clone()
    };
    for accts in lots.values_mut() {
        for (a, amt) in accts.iter_mut() {
            if in_pool(a)
                && let Some(cb) = &mut amt.cost_basis
            {
                cb.cost = Some(Box::new(avg.clone()));
            }
        }
    }
    Ok(avg)
}

/// When a posting to a plain account becomes lot subaccount postings and
/// had a balance assertion, move the assertion to a new zero posting to
/// the account, made subaccount-inclusive (preserveParentAssertion).
fn preserve_parent_assertion(
    verbose: bool,
    orig_acct: &AccountName,
    assertion: Option<&BalanceAssertion>,
    mut ps: Vec<Posting>,
) -> Vec<Posting> {
    let Some(ba) = assertion else { return ps };
    if lot_base_account(orig_acct) != &**orig_acct {
        return ps;
    }
    for p in &mut ps {
        p.assertion = None;
    }
    let mut carrier = Posting {
        account: orig_acct.clone(),
        amount: MixedAmount::from_amount(Amount {
            quantity: Decimal::ZERO,
            ..ba.amount.clone()
        }),
        assertion: Some(Box::new(BalanceAssertion {
            inclusive: true,
            ..ba.clone()
        })),
        ..Posting::default()
    };
    carrier.add_hidden_and_maybe_visible_tag(false, verbose, tag(GENERATED_POSTING_TAG, ""));
    carrier.add_hidden_and_maybe_visible_tag(false, verbose, tag(LOT_PARENT_ASSERTION_TAG, ""));
    ps.push(carrier);
    ps
}

/// The lot calculation's settings, from the journal.
struct Calc<'j> {
    j: &'j Journal,
    /// The journal's commodity styles, without rounding, for lot names.
    styles: CommodityStyles,
    verbose: bool,
    needs_labels: BTreeSet<(Sym, Day)>,
}

impl Calc<'_> {
    /// Several fragments of one posting tagged as per-lot splits; one
    /// left alone.
    fn tag_if_multi(&self, mut ps: Vec<Posting>) -> Vec<Posting> {
        if ps.len() != 1 {
            for p in &mut ps {
                p.add_hidden_and_maybe_visible_tag(
                    false,
                    self.verbose,
                    tag(LOTSPLIT_POSTING_TAG, ""),
                );
            }
        }
        ps
    }

    /// A commodity's lots in accounts of this lot direction
    /// (lotStateForDirection).
    fn lots_for_direction(&self, dir: LotDirection, c: &Sym, ls: &LotState) -> CommodityLots {
        ls.get(c)
            .map(|lots| {
                lots.iter()
                    .map(|(id, accts)| {
                        let accts = accts
                            .iter()
                            .filter(|(a, _)| lot_direction_or_long(self.j, a) == dir)
                            .map(|(a, amt)| (a.clone(), amt.clone()))
                            .collect();
                        (id.clone(), accts)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A commodity's lots in an account, if it is of this lot direction,
    /// with their amounts there (the account's part of lots_for_direction).
    fn account_lots<'l>(
        &self,
        dir: LotDirection,
        c: &Sym,
        acct: &str,
        ls: &'l LotState,
    ) -> BTreeMap<&'l LotId, &'l Amount> {
        if lot_direction_or_long(self.j, acct) != dir {
            return BTreeMap::new();
        }
        (ls.get(c).into_iter().flatten())
            .filter_map(|(id, accts)| accts.get(acct).map(|a| (id, a)))
            .collect()
    }

    /// A lot subaccount of a posting's base account.
    fn lot_account(&self, method: ReductionMethod, acct: &str, cb: &CostBasis) -> AccountName {
        let name = show_lot_name_for_method(method, &style_lot_cb_cost(&self.styles, cb));
        AccountName::from(format!("{}:{name}", lot_base_account(acct)))
    }

    /// Process a transaction's lot postings: transfers to or from equity
    /// (lots closed or opened by closing balances), or else fee disposals,
    /// then transfer groups, acquisitions and disposals in posting order
    /// (processTransaction).
    fn process_transaction(
        &self,
        ls: &mut LotState,
        t: &Transaction,
        tied: &Transaction,
    ) -> Result<Transaction, String> {
        let ps = &t.postings;
        let froms: Vec<usize> = (0..ps.len())
            .filter(|&i| is_transfer_from_posting(&ps[i]))
            .collect();
        let tos: Vec<usize> = (0..ps.len())
            .filter(|&i| is_transfer_to_posting(&ps[i]))
            .collect();
        let equity_other = ps.iter().any(|p| {
            !is_transfer_from_posting(p)
                && !is_transfer_to_posting(p)
                && self
                    .j
                    .account_type(lot_base_account(&p.account))
                    .is_some_and(|ty| ty.is_subtype_of(AccountType::Equity))
        });
        let mut out = Vec::with_capacity(ps.len());
        if !froms.is_empty() && tos.is_empty() && equity_other {
            for p in ps {
                if is_transfer_from_posting(p) {
                    out.extend(self.reduce_lot_transfer_to_equity(t, tied, ls, p)?);
                } else {
                    out.push(p.clone());
                }
            }
        } else if froms.is_empty() && !tos.is_empty() && equity_other {
            for (i, p) in ps.iter().enumerate() {
                if is_transfer_to_posting(p) {
                    out.push(self.process_acquire_posting(t, ls, i, p)?);
                } else {
                    out.push(p.clone());
                }
            }
        } else {
            let mut expanded: BTreeMap<usize, Vec<Posting>> = BTreeMap::new();
            for (i, p) in ps.iter().enumerate() {
                if is_dispose_posting(p) && p.has_tag(FEESPLIT_POSTING_TAG) {
                    expanded.insert(i, self.process_dispose_posting(t, tied, ls, p)?);
                }
            }
            let mut groups: BTreeMap<usize, TransferGroup> =
                group_indexed_transfer_postings(t, &froms, &tos)?
                    .into_iter()
                    .filter_map(|g| {
                        let first = g.1.iter().chain(&g.2).min().copied()?;
                        Some((first, g))
                    })
                    .collect();
            for (i, p) in ps.iter().enumerate() {
                if let Some(g) = groups.remove(&i) {
                    for (k, v) in self.process_transfer_group(t, tied, ls, g)? {
                        expanded.entry(k).or_insert(v);
                    }
                }
                if let Some(e) = expanded.remove(&i) {
                    out.extend(e);
                } else if is_acquire_posting(p) {
                    out.push(self.process_acquire_posting(t, ls, i, p)?);
                } else if is_dispose_posting(p) {
                    out.extend(self.process_dispose_posting(t, tied, ls, p)?);
                } else if is_unclassified_lotful_posting(self.j, p) {
                    return Err(unclassified_lot_warning(self.j, t, i, p));
                } else {
                    out.push(p.clone());
                }
            }
        }
        Ok(Transaction {
            postings: out,
            ..t.clone()
        })
    }

    /// Reduce the lots a transfer-from posting sends to equity (closing
    /// balances), the posting rewritten onto the selected lots'
    /// subaccounts (reduceLotTransferToEquity).
    fn reduce_lot_transfer_to_equity(
        &self,
        t: &Transaction,
        tied: &Transaction,
        ls: &mut LotState,
        p: &Posting,
    ) -> Result<Vec<Posting>, String> {
        let dir = lot_direction_or_long(self.j, &p.account);
        let outflows: Vec<(&Amount, &CostBasis)> = p
            .amount
            .amounts_raw()
            .iter()
            .filter(|a| dir.is_outflow(a))
            .filter_map(|a| Some((a, a.cost_basis.as_deref()?)))
            .collect();
        let [(a, cb)] = outflows.as_slice() else {
            return Ok(vec![p.clone()]);
        };
        let c = &a.commodity;
        let acct = lot_base_account(&p.account);
        let (method, source) = resolve_reduction_method(self.j, &p.account, c);
        let selected = select_lots(
            (method, &source),
            &|| posting_err_prefix(tied, p),
            "transfer",
            t.date,
            dir,
            acct,
            c,
            &a.quantity.abs(),
            cb,
            self.account_lots(dir, c, acct, ls),
            &|| self.lots_for_direction(dir, c, ls),
        )?;
        let sign = -dir.sign();
        let ps: Vec<Posting> = selected
            .iter()
            .map(|(id, stored, q)| {
                let lot_cb = self.lot_cb_of(t, id, stored, c)?;
                Ok(Posting {
                    account: self.lot_account(method, acct, &lot_cb),
                    amount: MixedAmount::from_amount(Amount {
                        cost_basis: Some(Box::new(lot_cb)),
                        ..(*a).clone().with_quantity(&sign * q)
                    }),
                    original: Some(Box::new(p.original().clone())),
                    ..p.clone()
                })
            })
            .collect::<Result<_, String>>()?;
        reduce_lot_state(acct, c, &selected, ls);
        let ps = self.tag_if_multi(ps);
        Ok(preserve_parent_assertion(
            self.verbose,
            &p.account,
            p.assertion.as_deref(),
            ps,
        ))
    }

    /// A lot's cost basis, from its id and stored amount (lotCbOf).
    fn lot_cb_of(
        &self,
        t: &Transaction,
        id: &LotId,
        stored: &Amount,
        c: &Sym,
    ) -> Result<CostBasis, String> {
        match stored.cost_basis.as_ref().and_then(|cb| cb.cost.as_ref()) {
            Some(cost) => Ok(CostBasis {
                date: Some(id.date),
                label: id.label.clone(),
                cost: Some(cost.clone()),
            }),
            None => Err(format!(
                "{}lot {id} for commodity {c} has no cost basis (internal error)",
                txn_err_prefix(t)
            )),
        }
    }

    /// Open a lot for an acquire posting, named by its date, any label (or
    /// one generated, if needed to tell same-date lots apart) and cost
    /// (from its basis, or its transacted cost), as a subaccount of its
    /// account (processAcquirePosting).
    fn process_acquire_posting(
        &self,
        t: &Transaction,
        ls: &mut LotState,
        idx: usize,
        p: &Posting,
    ) -> Result<Posting, String> {
        let j = self.j;
        let pos = || posting_at_err_prefix(t, idx);
        let amts = p.amount.amounts_raw();
        let dir = lot_direction_or_long(j, &p.account);
        let with_basis: Vec<&Amount> = amts.iter().filter(|a| a.cost_basis.is_some()).collect();
        let (lot_amt, cb, is_bare) = match with_basis.as_slice() {
            [a] => (
                (*a).clone(),
                a.cost_basis.as_deref().cloned().unwrap_or_default(),
                false,
            ),
            _ => match amts
                .iter()
                .filter(|a| !dir.is_outflow(a))
                .collect::<Vec<_>>()
                .as_slice()
            {
                [a] => ((*a).clone(), CostBasis::default(), true),
                _ => return Err(pos() + "acquire posting has no cost basis"),
            },
        };
        let c = &lot_amt.commodity;
        let date = cb.date.unwrap_or(t.date);
        let orig_amt = match &p.original {
            Some(o) => o
                .amount
                .amounts_raw()
                .iter()
                .find(|a| &a.commodity == c)
                .unwrap_or(&lot_amt),
            None => &lot_amt,
        };
        let lot_basis = match (&cb.cost, &orig_amt.cost, &lot_amt.cost) {
            (Some(cost), _, _) => (**cost).clone(),
            (None, Some(AmountCost::Unit(cost)), _) => (**cost).clone(),
            (None, _, Some(cost)) => amount_cost_to_unit_cost(&lot_amt.quantity, cost),
            (None, _, None) if is_bare => {
                return Err(format!(
                    "{}{c} is lotful but this acquire posting has no cost basis or price.\nNo lot will be created.",
                    pos()
                ));
            }
            (None, _, None) => {
                let name = show_lot_name(&cb);
                let example = if name == "{}" {
                    "{$50}".to_string()
                } else {
                    format!("{}, $50}}", &name[..name.len() - 1])
                };
                return Err(format!(
                    "{}This posting creates a new {c} lot, but its lot annotation {name} has no cost,\n\
                     and there is no price (@ or @@) to infer one from.\n\
                     Please add a per-unit cost to the annotation, eg {example}.",
                    pos()
                ));
            }
        };
        let inferred = cb.cost.is_none();
        let needs_label = self.needs_labels.contains(&(*c, date));
        let label = cb
            .label
            .clone()
            .or_else(|| needs_label.then(|| generate_label(ls.get(c), date)));
        let id0 = LotId { date, label };
        let exists = |id: &LotId| ls.get(c).is_some_and(|lots| lots.contains_key(id));
        let id = if cb.label.is_none() && exists(&id0) {
            LotId {
                date,
                label: Some(generate_label(ls.get(c), date)),
            }
        } else {
            id0
        };
        let duplicate = ls
            .get(c)
            .and_then(|lots| lots.get(&id))
            .is_some_and(|accts| !accts.is_empty());
        let base = lot_base_account(&p.account);
        let (method, _) = resolve_reduction_method(j, &p.account, c);
        let stored_basis = if method.is_average() {
            let in_pool = |a: &str| {
                if method.is_global() {
                    lot_direction_or_long(j, a) == dir
                } else {
                    a == base
                }
            };
            update_pool_on_acquire(&pos, &in_pool, c, &lot_amt.quantity.abs(), &lot_basis, ls)?
        } else {
            lot_basis.clone()
        };
        let widen = |cb: CostBasis| {
            if inferred {
                widen_lot_cb_cost(&self.styles, cb)
            } else {
                cb
            }
        };
        let full_cb = widen(style_lot_cb_cost(
            &self.styles,
            &CostBasis {
                date: Some(date),
                label: id.label.clone(),
                cost: Some(Box::new(stored_basis)),
            },
        ));
        let lot_name = show_lot_name_for_method(method, &full_cb);
        let expected = AccountName::from(format!("{base}:{lot_name}"));
        if base != &*p.account && merge_cost_basis_for_method(method, &cb, &full_cb).is_err() {
            return Err(format!(
                "{}lot subaccount {} does not match the resolved lot {expected}",
                pos(),
                p.account
            ));
        }
        if duplicate {
            return Err(format!(
                "{}duplicate lot id: {lot_name} for commodity {c}",
                pos()
            ));
        }
        let posting_amt = if inferred {
            let filled = widen(style_lot_cb_cost(
                &self.styles,
                &CostBasis {
                    cost: Some(Box::new(lot_basis)),
                    ..cb.clone()
                },
            ));
            Amount {
                cost_basis: Some(Box::new(filled)),
                ..lot_amt.clone()
            }
        } else {
            lot_amt.clone()
        };
        let state_amt = Amount {
            quantity: lot_amt.quantity.abs(),
            cost_basis: Some(Box::new(full_cb)),
            ..lot_amt.clone()
        };
        add_lot_state(c, id, base, state_amt, ls);
        Ok(Posting {
            account: expected,
            amount: MixedAmount::from_amount(posting_amt),
            original: Some(Box::new(p.original().clone())),
            ..p.clone()
        })
    }

    /// Dispose of the lots a dispose posting selects, by its account's
    /// reduction method, splitting it into a posting per lot
    /// (processDisposePosting).
    fn process_dispose_posting(
        &self,
        t: &Transaction,
        tied: &Transaction,
        ls: &mut LotState,
        p: &Posting,
    ) -> Result<Vec<Posting>, String> {
        let j = self.j;
        let show_pos = || txn_err_prefix(t);
        let amts = p.amount.amounts_raw();
        let dir = lot_direction_or_long(j, &p.account);
        let with_basis: Vec<&Amount> = amts.iter().filter(|a| a.cost_basis.is_some()).collect();
        let (lot_amt, cb, is_bare) = match with_basis.as_slice() {
            [a] => (
                *a,
                a.cost_basis.as_deref().cloned().unwrap_or_default(),
                false,
            ),
            _ => match amts
                .iter()
                .filter(|a| dir.is_outflow(a))
                .collect::<Vec<_>>()
                .as_slice()
            {
                [a] => (*a, CostBasis::default(), true),
                _ => return Err(show_pos() + "dispose posting has no cost basis"),
            },
        };
        let c = &lot_amt.commodity;
        let holds_lots = |a: &str| super::lot_direction(j, a).is_some();
        let non_lot_receipts = t
            .postings
            .iter()
            .filter(|q| !holds_lots(&q.account))
            .flat_map(|q| q.amount.amounts_raw())
            .filter(|a| &a.commodity == c && a.quantity > Decimal::ZERO)
            .fold(Decimal::ZERO, |s, a| &s + &a.quantity);
        let priceless_disposals = t
            .postings
            .iter()
            .filter(|q| is_dispose_posting(q))
            .flat_map(|q| q.amount.amounts_raw())
            .filter(|a| &a.commodity == c && a.is_negative() && a.cost.is_none())
            .fold(Decimal::ZERO, |s, a| &s + &-&a.quantity);
        let in_kind = non_lot_receipts > Decimal::ZERO && non_lot_receipts == priceless_disposals;
        if lot_amt.cost.is_none() && !is_bare && !in_kind {
            let price = if dir == LotDirection::Short {
                "buying price, to cover the short"
            } else {
                "selling price"
            };
            return Err(format!(
                "{}dispose posting has no transacted price ({price}) for {c}",
                show_pos()
            ));
        }
        if !dir.is_outflow(lot_amt) {
            let what = if dir == LotDirection::Short {
                "non-positive quantity (a short position is covered by a positive posting)"
            } else {
                "non-negative quantity"
            };
            return Err(format!("{}dispose posting has {what} for {c}", show_pos()));
        }
        let (method, source) = resolve_reduction_method(j, &p.account, c);
        let scope = lot_base_account(&p.account);
        if is_bare && method == ReductionMethod::Specid {
            return Err(format!(
                "{}SPECID requires a lot selector on dispose postings\nUsing SPECID ({source}).",
                show_pos()
            ));
        }
        let selected = select_lots(
            (method, &source),
            &|| posting_err_prefix(tied, p),
            "disposal",
            t.date,
            dir,
            scope,
            c,
            &lot_amt.quantity.abs(),
            &cb,
            self.account_lots(dir, c, scope, ls),
            &|| self.lots_for_direction(dir, c, ls),
        )?;
        let close_sign = -dir.sign();
        let explicit = scope != &*p.account;
        let mut ps = Vec::with_capacity(selected.len());
        for (id, stored, q) in &selected {
            let Some(basis) = stored.cost_basis.as_ref().and_then(|cb| cb.cost.clone()) else {
                return Err(format!(
                    "{}lot {id} for commodity {c} has no cost basis (internal error)",
                    show_pos()
                ));
            };
            let lot_cb = CostBasis {
                date: Some(id.date),
                label: id.label.clone(),
                cost: Some(basis),
            };
            let expected = self.lot_account(method, scope, &lot_cb);
            if explicit && merge_cost_basis_for_method(method, &cb, &lot_cb).is_err() {
                return Err(format!(
                    "{}lot subaccount {} does not match the resolved lot {expected}",
                    show_pos(),
                    p.account
                ));
            }
            let mut amt = Amount {
                cost_basis: Some(Box::new(lot_cb)),
                ..lot_amt.clone().with_quantity(&close_sign * q)
            };
            if lot_amt.cost.is_none() {
                amt.cost = None;
            } else if selected.len() > 1 {
                amt.cost = lot_amt.cost.as_ref().map(|cost| {
                    AmountCost::Unit(Box::new(amount_cost_to_unit_cost(&lot_amt.quantity, cost)))
                });
            }
            ps.push(Posting {
                account: expected,
                amount: MixedAmount::from_amount(amt),
                original: Some(Box::new(p.original().clone())),
                ..p.clone()
            });
        }
        let ps = self.tag_if_multi(ps);
        reduce_lot_state(scope, c, &selected, ls);
        let no_other_lots = ls
            .get(c)
            .is_none_or(|lots| lots.values().all(|accts| !accts.contains_key(scope)));
        if ps.len() == 1 && no_other_lots {
            Ok(ps)
        } else {
            Ok(preserve_parent_assertion(
                self.verbose,
                &p.account,
                p.assertion.as_deref(),
                ps,
            ))
        }
    }

    /// Process one commodity's transfer group: select lots from each
    /// source posting's account, then share them out among the destination
    /// postings in order, re-adding them under the destination accounts.
    /// Returns each posting index's resulting postings
    /// (processTransferGroup).
    fn process_transfer_group(
        &self,
        t: &Transaction,
        tied: &Transaction,
        ls: &mut LotState,
        (c, ifroms, itos): TransferGroup,
    ) -> Result<BTreeMap<usize, Vec<Posting>>, String> {
        let j = self.j;
        let show_pos = || txn_err_prefix(t);
        let check_no_transacted_price = |p: &Posting| {
            if p.original()
                .amount
                .amounts_raw()
                .iter()
                .any(|a| a.cost.is_some())
            {
                Err(show_pos() + "lot transfers should have no transacted price")
            } else {
                Ok(())
            }
        };
        let mut from_done: Vec<(usize, Amount, Vec<Selected>)> = Vec::new();
        for &i in &ifroms {
            let from_p = &t.postings[i];
            let amts = from_p.amount.amounts_raw();
            let with_basis: Vec<&Amount> = amts.iter().filter(|a| a.cost_basis.is_some()).collect();
            let (from_amt, from_cb) = match with_basis.as_slice() {
                [a] => (
                    (*a).clone(),
                    a.cost_basis.as_deref().cloned().unwrap_or_default(),
                ),
                _ => match amts
                    .iter()
                    .filter(|a| a.is_negative())
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    [a] => ((*a).clone(), CostBasis::default()),
                    _ => return Err(show_pos() + "transfer-from posting has no cost basis"),
                },
            };
            check_no_transacted_price(from_p)?;
            if !from_amt.is_negative() {
                return Err(format!(
                    "{}transfer-from posting has non-negative quantity for {c}",
                    show_pos()
                ));
            }
            let (method, source) = resolve_reduction_method(j, &from_p.account, &c);
            let base = lot_base_account(&from_p.account);
            let selected = select_lots(
                (method, &source),
                &|| posting_err_prefix(tied, from_p),
                "transfer",
                t.date,
                LotDirection::Long,
                base,
                &c,
                &-&from_amt.quantity,
                &from_cb,
                self.account_lots(LotDirection::Long, &c, base, ls),
                &|| self.lots_for_direction(LotDirection::Long, &c, ls),
            )?;
            reduce_lot_state(base, &c, &selected, ls);
            from_done.push((i, from_amt, selected));
        }
        let mut queue: VecDeque<(LotId, Amount, Decimal, Amount)> = from_done
            .iter()
            .flat_map(|(_, fa, sel)| {
                sel.iter()
                    .map(move |(id, a, q)| (id.clone(), a.clone(), q.clone(), fa.clone()))
            })
            .collect();
        let mut results: BTreeMap<usize, Vec<Posting>> = BTreeMap::new();
        for &i in &itos {
            let to_p = &t.postings[i];
            check_no_transacted_price(to_p)?;
            let positives: Vec<&Decimal> = to_p
                .amount
                .amounts_raw()
                .iter()
                .filter(|a| a.commodity == c && a.quantity > Decimal::ZERO)
                .map(|a| &a.quantity)
                .collect();
            let [to_qty] = positives.as_slice() else {
                return Err(format!(
                    "{}transfer-to posting has no single positive {c} amount",
                    show_pos()
                ));
            };
            let to_cbs: Vec<&CostBasis> = to_p
                .amount
                .amounts_raw()
                .iter()
                .filter_map(|a| a.cost_basis.as_deref())
                .collect();
            let to_cb = match to_cbs.as_slice() {
                [cb] => Some(*cb),
                _ => None,
            };
            let portions = draw_from_queue(to_qty, &mut queue);
            let (to_method, _) = resolve_reduction_method(j, &to_p.account, &c);
            let to_base = lot_base_account(&to_p.account);
            let total = portions
                .iter()
                .fold(Decimal::ZERO, |s, (_, _, q, _)| &s + q);
            if total != **to_qty {
                return Err(
                    show_pos() + "could not distribute transferred lots exactly (internal error)"
                );
            }
            let mut to_ps = Vec::with_capacity(portions.len());
            for (id, stored, q, from_amt) in &portions {
                let lot_cb = self.lot_cb_of(t, id, stored, &c)?;
                if let Some(to_cb) = to_cb {
                    validate_to_cb(t, to_method, to_cb, &lot_cb)?;
                }
                to_ps.push(Posting {
                    account: self.lot_account(to_method, to_base, &lot_cb),
                    amount: MixedAmount::from_amount(Amount {
                        cost_basis: Some(Box::new(lot_cb)),
                        ..from_amt.clone().with_quantity(q.clone())
                    }),
                    original: Some(Box::new(to_p.original().clone())),
                    ..to_p.clone()
                });
            }
            for (id, stored, q, _) in portions {
                self.add_transferred_lot(t, to_method, to_base, ls, &c, id, stored, q)?;
            }
            let to_ps = self.tag_if_multi(to_ps);
            results.insert(
                i,
                preserve_parent_assertion(
                    self.verbose,
                    &to_p.account,
                    to_p.assertion.as_deref(),
                    to_ps,
                ),
            );
        }
        if !queue.is_empty() {
            return Err(
                show_pos() + "could not distribute transferred lots exactly (internal error)"
            );
        }
        for (i, from_amt, selected) in from_done {
            let from_p = &t.postings[i];
            let (method, _) = resolve_reduction_method(j, &from_p.account, &c);
            let ps = selected
                .iter()
                .map(|(id, stored, q)| {
                    let lot_cb = self.lot_cb_of(t, id, stored, &c)?;
                    Ok(Posting {
                        account: self.lot_account(method, &from_p.account, &lot_cb),
                        amount: MixedAmount::from_amount(Amount {
                            cost_basis: Some(Box::new(lot_cb)),
                            ..from_amt.clone().with_quantity(-q)
                        }),
                        original: Some(Box::new(from_p.original().clone())),
                        ..from_p.clone()
                    })
                })
                .collect::<Result<Vec<Posting>, String>>()?;
            let ps = self.tag_if_multi(ps);
            results.insert(
                i,
                preserve_parent_assertion(
                    self.verbose,
                    &from_p.account,
                    from_p.assertion.as_deref(),
                    ps,
                ),
            );
        }
        Ok(results)
    }

    /// Re-add a transferred lot under its destination account, first
    /// re-averaging the destination's pool under an average method
    /// (addTransferredLot).
    #[allow(clippy::too_many_arguments)]
    fn add_transferred_lot(
        &self,
        t: &Transaction,
        method: ReductionMethod,
        dest: &str,
        ls: &mut LotState,
        c: &Sym,
        id: LotId,
        stored: Amount,
        q: Decimal,
    ) -> Result<(), String> {
        if !method.is_average() {
            add_lot_state(
                c,
                id,
                dest,
                Amount {
                    quantity: q,
                    ..stored
                },
                ls,
            );
            return Ok(());
        }
        let lot_cb = self.lot_cb_of(t, &id, &stored, c)?;
        let carried = lot_cb.cost.as_deref().cloned().unwrap_or_else(Amount::null);
        let in_pool = |a: &str| {
            if method.is_global() {
                lot_direction_or_long(self.j, a) == LotDirection::Long
            } else {
                a == dest
            }
        };
        let avg = update_pool_on_acquire(&|| txn_err_prefix(t), &in_pool, c, &q, &carried, ls)?;
        let amt = Amount {
            quantity: q,
            cost_basis: Some(Box::new(CostBasis {
                cost: Some(Box::new(avg)),
                ..lot_cb
            })),
            ..stored
        };
        add_lot_state(c, id, dest, amt, ls);
        Ok(())
    }
}

/// Take fragments totalling a quantity from the front of the queue,
/// splitting the last if needed (drawFromQueue).
fn draw_from_queue(
    need: &Decimal,
    queue: &mut VecDeque<(LotId, Amount, Decimal, Amount)>,
) -> Vec<(LotId, Amount, Decimal, Amount)> {
    let mut need = need.clone();
    let mut taken = Vec::new();
    while !need.is_zero() {
        let Some((id, a, q, fa)) = queue.pop_front() else {
            break;
        };
        if need >= q {
            need = &need - &q;
            taken.push((id, a, q, fa));
        } else {
            queue.push_front((id.clone(), a.clone(), &q - &need, fa.clone()));
            taken.push((id, a, need, fa));
            break;
        }
    }
    taken
}

/// Check that a transfer destination's lot annotation, if it has one,
/// matches the source lot (validateToCb).
fn validate_to_cb(
    t: &Transaction,
    method: ReductionMethod,
    to_cb: &CostBasis,
    lot_cb: &CostBasis,
) -> Result<(), String> {
    let date_mismatch = to_cb.date.is_some_and(|d| lot_cb.date != Some(d));
    let label_mismatch = to_cb
        .label
        .as_ref()
        .is_some_and(|l| lot_cb.label.as_ref() != Some(l));
    let cost_mismatch = !method.is_average()
        && matches!((&to_cb.cost, &lot_cb.cost), (Some(c), Some(lc)) if !lot_costs_match(c, lc));
    if date_mismatch || label_mismatch || cost_mismatch {
        return Err(format!(
            "{}Destination lot info {} does not match the source lot {}.\n\
             Transfers must preserve the source lot's identity.\n\
             Remove the destination lot annotation, or make it match the source lot's info.\n",
            txn_err_prefix(t),
            show_lot_name(to_cb),
            show_lot_name(lot_cb)
        ));
    }
    Ok(())
}

/// Group a transaction's transfer postings by commodity, checking that each
/// group has both sides and equal from and to totals, each side sorted by
/// its lot annotation (groupIndexedTransferPostings).
fn group_indexed_transfer_postings(
    t: &Transaction,
    froms: &[usize],
    tos: &[usize],
) -> Result<Vec<TransferGroup>, String> {
    if froms.is_empty() && tos.is_empty() {
        return Ok(Vec::new());
    }
    let commodity = |label: &str, i: usize| -> Result<Sym, String> {
        let p = &t.postings[i];
        let amts = p.amount.amounts_raw();
        let with_basis: Vec<&Sym> = amts
            .iter()
            .filter(|a| a.cost_basis.is_some())
            .map(|a| &a.commodity)
            .collect();
        match (with_basis.as_slice(), amts) {
            ([c], _) => Ok(**c),
            (_, [a]) => Ok(a.commodity),
            _ => Err(format!(
                "{}This {label} posting has amounts in several commodities ({}).\n\
                 In a lot transfer, each posting should have just one commodity;\n\
                 please write these amounts on separate postings.",
                posting_at_err_prefix(t, i),
                show_mixed_amount_one_line(&p.amount)
            )),
        }
    };
    let group = |label: &str, ips: &[usize]| -> Result<BTreeMap<Sym, Vec<usize>>, String> {
        let mut m: BTreeMap<Sym, Vec<usize>> = BTreeMap::new();
        for &i in ips {
            m.entry(commodity(label, i)?).or_default().push(i);
        }
        Ok(m)
    };
    let from_groups = group("transfer-from", froms)?;
    let to_groups = group("transfer-to", tos)?;
    let comms: BTreeSet<&Sym> = from_groups.keys().chain(to_groups.keys()).collect();
    let sort_key = |&i: &usize| {
        let cbs: Vec<&CostBasis> = t.postings[i]
            .amount
            .amounts_raw()
            .iter()
            .filter_map(|a| a.cost_basis.as_deref())
            .collect();
        match cbs.as_slice() {
            [cb] => (
                cb.date,
                cb.label.clone(),
                cb.cost.as_ref().map(|a| (a.commodity, a.quantity.clone())),
            ),
            _ => (None, None, None),
        }
    };
    let mut groups = Vec::new();
    for comm in comms {
        let (fs, ts) = match (from_groups.get(comm), to_groups.get(comm)) {
            (None, _) => {
                return Err(format!(
                    "{}transfer-to posting for {comm} has no matching transfer-from posting",
                    txn_err_prefix(t)
                ));
            }
            (_, None) => {
                return Err(format!(
                    "{}transfer-from posting for {comm} has no matching transfer-to posting",
                    txn_err_prefix(t)
                ));
            }
            (Some(fs), Some(ts)) => (fs, ts),
        };
        let total = |ips: &[usize]| {
            ips.iter()
                .flat_map(|&i| t.postings[i].amount.amounts_raw())
                .filter(|a| &a.commodity == comm)
                .fold(Decimal::ZERO, |s, a| &s + &a.quantity.abs())
        };
        let (from_total, to_total) = (total(fs), total(ts));
        if from_total != to_total {
            let advice = if to_total > from_total {
                "More was received than sent, which a fee can't explain;\n\
                 check the entry for sign errors or rounding adjustments."
            } else {
                "If the difference is a fee, you can either\n\
                 - record the fee expense in the lot-tracked commodity, with posting(s)\n  \
                 adding up to the missing quantity\n\
                 - or split the sending posting into a transfer part and fee part(s)\n  \
                 matching the fee expense(s)."
            };
            return Err(format!(
                "{}Mismatched transfer quantities for lot-tracked commodity {comm}: \
                 {from_total} transferred out but {to_total} received.\n{advice}",
                txn_err_prefix(t)
            ));
        }
        let (mut fs, mut ts) = (fs.clone(), ts.clone());
        fs.sort_by_key(sort_key);
        ts.sort_by_key(sort_key);
        groups.push((*comm, fs, ts));
    }
    Ok(groups)
}

/// Remove the cost bases the balancer copied into inferred amounts from
/// the postings classification left unclassified
/// (journalStripBalancerCopiedBases).
pub fn journal_strip_balancer_copied_bases(j: &mut Journal) {
    crate::par::for_each_mut(j.txns.len(), &mut j.txns, |t| {
        for p in &mut t.postings {
            if has_balancer_copied_basis(p) && !is_classified_posting(p) {
                p.amount = p.amount.map(|a| Amount {
                    cost_basis: None,
                    ..a.clone()
                });
            }
        }
    });
}

/// In each disposal with a transacted price, add a realised gain posting
/// to the Gain account, or fill in or check the user's gain postings
/// (journalAddOrCheckGainPostings).
pub fn journal_add_or_check_gain_postings(
    verbose: bool,
    mut j: Journal,
) -> Result<Journal, String> {
    let gain_account =
        AccountName::from(j.account_for_type(DEFAULT_GAIN_ACCOUNT, AccountType::Gain));
    let mut txns = std::mem::take(&mut j.txns);
    let g = Gains {
        account_types: &j.account_types,
        gain_account,
        verbose,
    };
    for (i, t) in txns.iter_mut().enumerate() {
        let t2 = g
            .add_or_check(t)
            .map_err(|e| append_postings_read_as(t, e))?;
        if let Some(mut t2) = t2 {
            for p in &mut t2.postings {
                p.txn = Some(i as u32);
            }
            *t = t2;
        }
    }
    j.txns = txns;
    Ok(j)
}

struct Gains<'a> {
    account_types: &'a BTreeMap<AccountName, AccountType>,
    gain_account: AccountName,
    verbose: bool,
}

impl Gains<'_> {
    fn is_gain_typed(&self, p: &Posting) -> bool {
        p.is_real() && account_name_type(self.account_types, &p.account) == Some(AccountType::Gain)
    }

    fn is_gain(&self, p: &Posting) -> bool {
        p.is_real() && (is_gain_posting(p) || self.is_gain_typed(p))
    }

    /// The transaction with gain postings added, filled in or corrected,
    /// or None if unchanged.
    fn add_or_check(&self, t: &Transaction) -> Result<Option<Transaction>, String> {
        let ps = &t.postings;
        if ps.iter().any(is_gain_posting) {
            let amountless = ps
                .iter()
                .filter(|p| self.is_gain(p) && !p.has_amount())
                .count();
            return match amountless {
                0 => self.check_gain(t),
                1 => Ok(Some(self.fill_gain(t))),
                _ => Err(txn_err_prefix(t)
                    + "This disposal has more than one amountless gain posting.\n\
                       At most one gain posting can have its amount inferred."),
            };
        }
        let dispose_has_price = |p: &Posting| {
            is_dispose_posting(p) && p.amount.amounts_raw().iter().any(|a| a.cost.is_some())
        };
        if !ps.iter().any(dispose_has_price) {
            return Ok(None);
        }
        if ps
            .iter()
            .any(|p| self.is_gain_typed(p) && !p.original().has_amount())
        {
            return Err(txn_err_prefix(t)
                + "This disposal has an amountless gain posting, which can't be inferred here.\n\
                   Write the amount explicitly, or omit the posting entirely\n\
                   (hledger will then infer the realised gain from the cost basis).");
        }
        if ps.iter().any(|p| self.is_gain_typed(p)) {
            return self.check_gain(t);
        }
        let gain = disposal_gain(t);
        if gain.is_zero() {
            return Ok(None);
        }
        let mut gp = Posting {
            account: self.gain_account.clone(),
            amount: set_local_gain_precision(t, -gain),
            ..Posting::default()
        };
        gp.add_hidden_and_maybe_visible_tag(false, self.verbose, tag(GENERATED_POSTING_TAG, ""));
        gp.add_hidden_and_maybe_visible_tag(true, self.verbose, tag("_ptype", "gain"));
        let mut t2 = t.clone();
        t2.postings.push(gp);
        Ok(Some(t2))
    }

    /// Set the one amountless gain posting's amount to the calculated gain,
    /// less the other gain postings' amounts.
    fn fill_gain(&self, t: &Transaction) -> Transaction {
        let gain = disposal_gain(t);
        let written = fold_map(
            t.postings
                .iter()
                .filter(|p| self.is_gain(p) && p.has_amount())
                .map(|p| p.amount.clone()),
        );
        let amt = set_local_gain_precision(t, &-gain + &-written);
        let mut t2 = t.clone();
        for p in &mut t2.postings {
            if self.is_gain(p) && !p.has_amount() {
                p.keep_original();
                p.amount = amt.clone();
            }
        }
        t2
    }

    /// Check that the gain postings add up to the calculated gain, within
    /// the entry's precision; a difference below that is taken off the last
    /// gain posting (in its commodity), so the gain is exact.
    fn check_gain(&self, t: &Transaction) -> Result<Option<Transaction>, String> {
        let gain = disposal_gain(t);
        let written = fold_map(
            t.postings
                .iter()
                .filter(|p| self.is_gain(p))
                .map(|p| p.amount.clone()),
        );
        let diff = &written + &gain;
        if !set_local_gain_precision(t, diff.clone()).looks_zero() {
            let fmt = AmountFormat {
                zero_commodity: true,
                ..AmountFormat::ONE_LINE_NO_COST
            };
            let (w, c) = show_mixed_amounts_distinctly(&fmt, &written, &-&gain);
            return Err(format!(
                "{}This disposal's realised gain amount is wrong.\n  written:    {w}\n  calculated: {c}",
                txn_err_prefix(t)
            ));
        }
        let nonzero: Vec<&Amount> = diff
            .amounts_raw()
            .iter()
            .filter(|a| !a.quantity.is_zero())
            .collect();
        if nonzero.is_empty() {
            return Ok(None);
        }
        let mut ps = t.postings.clone();
        for a in nonzero.into_iter().rev() {
            let in_commodity = |q: &Posting| {
                self.is_gain(q)
                    && q.amount
                        .amounts_raw()
                        .iter()
                        .any(|b| b.commodity == a.commodity)
            };
            let is_gain = |q: &Posting| self.is_gain(q);
            let target: &dyn Fn(&Posting) -> bool = if ps.iter().any(in_commodity) {
                &in_commodity
            } else {
                &is_gain
            };
            if let Some(k) = ps.iter().rposition(target) {
                let q = &mut ps[k];
                q.keep_original();
                q.amount =
                    set_local_gain_precision(t, &q.amount + &-MixedAmount::from_amount(a.clone()));
            }
        }
        Ok(Some(Transaction {
            postings: ps,
            ..t.clone()
        }))
    }
}

/// The realised gain of a transaction's disposals: for each amount, not in
/// an acquire posting, with both a cost basis and a transacted cost, its
/// value at cost basis less its value at transacted cost
/// (postingDisposalGain, amountBasisVsTransactedGap).
fn disposal_gain(t: &Transaction) -> MixedAmount {
    fold_map(t.postings.iter().map(|p| {
        if is_acquire_posting(p) || !p.has_amount() {
            return MixedAmount::null();
        }
        fold_map(p.amount.amounts_raw().iter().map(|a| {
            let basis = a.cost_basis.as_ref().and_then(|cb| cb.cost.as_ref());
            let (Some(basis), Some(cost)) = (basis, &a.cost) else {
                return MixedAmount::null();
            };
            let basis_val = MixedAmount::from_amount(Amount {
                quantity: &a.quantity * &basis.quantity,
                ..(**basis).clone()
            });
            let transacted_val = MixedAmount::from_amount(match cost {
                AmountCost::Unit(c) => Amount {
                    quantity: &a.quantity * &c.quantity,
                    ..(**c).clone()
                },
                AmountCost::Total(c) => (**c).clone(),
            });
            &basis_val + &-transacted_val
        }))
    }))
}

/// Each amount with its commodity's precision in the entry, if at least 1,
/// or else 2 decimal places if it has cents, or none
/// (setLocalGainPrecision).
fn set_local_gain_precision(t: &Transaction, m: MixedAmount) -> MixedAmount {
    let styles = t.commodity_styles();
    m.map(|a| {
        let precision = match styles.get(&a.commodity).map(|s| s.precision) {
            Some(Precision::Fixed(n)) if n >= 1 => Precision::Fixed(n),
            _ => {
                let q2 = a.quantity.round_to(2);
                Precision::Fixed(if q2.round_to(0) == q2 { 0 } else { 2 })
            }
        };
        a.clone().with_precision(precision)
    })
}

pub(crate) mod tests {
    use super::*;
    use crate::model::render::{PostingLayout, show_transaction};
    use crate::read::InputOpts;

    /// This journal read and finalised as `hledger -f f.j` reads it, as
    /// the transactions `print --lots -x` shows on or after `from` (the
    /// lot subaccounts and the amounts as finalised), or the error.
    fn printed(text: &str, from: Day) -> Result<String, String> {
        let iopts = InputOpts {
            today: Day::from_ymd(2026, 6, 1).unwrap(),
            ..InputOpts::default()
        };
        let (j, st) = crate::read::journal::parse_journal(&iopts, "f.j", text)?;
        let j = crate::read::finalise::journal_finalise(&iopts, "f.j", text, j, st)?;
        let styles = j.commodity_styles();
        Ok((j.txns.iter())
            .filter(|t| t.date >= from)
            .map(|t| {
                let mut t = t.clone();
                t.style_amounts(&styles);
                show_transaction(&t, PostingLayout::default(), false)
            })
            .collect())
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    // The expected outputs are the reference hledger's on the same journals
    // (`print --lots -x`, or the error after the program name).

    /// Classification decides which transactions can involve lots, and the lot
    /// calculation takes its answer (c9fa087): an amount of a lotful commodity
    /// or with a cost basis gets a lot subaccount, others are left alone.
    #[cfg_attr(test, test)]
    pub(crate) fn lotful_commodities_and_cost_bases_get_lots() {
        let journal = concat!(
            "commodity AAPL  ; lots:\n",
            "\n",
            "2026-01-01 buy declared\n",
            "    assets:stocks    10 AAPL @ $50\n",
            "    assets:cash\n",
            "\n",
            "2026-01-02 buy with basis\n",
            "    assets:stocks    5 MSFT {$20}\n",
            "    assets:cash\n",
            "\n",
            "2026-01-03 plain\n",
            "    assets:stocks    3 GOOG @ $10\n",
            "    assets:cash\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap(),
            concat!(
                "2026-01-01 buy declared\n",
                "    assets:stocks:{2026-01-01, $50}               10 AAPL {$50} @ $50\n",
                "    assets:cash                                $-500\n",
                "\n",
                "2026-01-02 buy with basis\n",
                "    assets:stocks:{2026-01-02, $20}                5 MSFT {$20} @ $20\n",
                "    assets:cash                                $-100\n",
                "\n",
                "2026-01-03 plain\n",
                "    assets:stocks                                  3 GOOG @ $10\n",
                "    assets:cash                                 $-30\n",
                "\n",
            )
        );
    }

    /// A disposal method on an account does not make a commodity lotful.
    #[cfg_attr(test, test)]
    pub(crate) fn an_account_method_alone_makes_no_lots() {
        let journal = concat!(
            "account assets:stocks  ; lots: FIFO\n",
            "\n",
            "2026-01-01 buy, account tag only\n",
            "    assets:stocks    10 AAPL @ $50\n",
            "    assets:cash\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap(),
            concat!(
                "2026-01-01 buy, account tag only\n",
                "    assets:stocks                                 10 AAPL @ $50\n",
                "    assets:cash                                $-500\n",
                "\n",
            )
        );
    }

    /// FIFO disposes of the oldest lots first.
    #[cfg_attr(test, test)]
    pub(crate) fn fifo_disposes_oldest_lots_first() {
        let journal = concat!(
            "commodity AAPL  ; lots: FIFO\n",
            "\n",
            "2026-01-15 buy cheap\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy dear\n",
            "    assets:stocks    10 AAPL {$70}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 buy middle\n",
            "    assets:stocks    10 AAPL {$60}\n",
            "    assets:cash\n",
            "\n",
            "2026-04-01 sell\n",
            "    assets:stocks   -15 AAPL {} @ $65\n",
            "    assets:cash      $975\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 4, 1)).unwrap(),
            concat!(
                "2026-04-01 sell\n",
                "    assets:stocks:{2026-01-15, $50}              -10 AAPL {2026-01-15, $50} @ $65\n",
                "    assets:stocks:{2026-02-01, $70}               -5 AAPL {2026-02-01, $70} @ $65\n",
                "    assets:cash                                 $975\n",
                "    revenue:gains                              $-125\n",
                "\n",
            )
        );
    }

    /// LIFO disposes of the newest lots first.
    #[cfg_attr(test, test)]
    pub(crate) fn lifo_disposes_newest_lots_first() {
        let journal = concat!(
            "commodity AAPL  ; lots: LIFO\n",
            "\n",
            "2026-01-15 buy cheap\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy dear\n",
            "    assets:stocks    10 AAPL {$70}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 buy middle\n",
            "    assets:stocks    10 AAPL {$60}\n",
            "    assets:cash\n",
            "\n",
            "2026-04-01 sell\n",
            "    assets:stocks   -15 AAPL {} @ $65\n",
            "    assets:cash      $975\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 4, 1)).unwrap(),
            concat!(
                "2026-04-01 sell\n",
                "    assets:stocks:{2026-03-01, $60}              -10 AAPL {2026-03-01, $60} @ $65\n",
                "    assets:stocks:{2026-02-01, $70}               -5 AAPL {2026-02-01, $70} @ $65\n",
                "    assets:cash                                 $975\n",
                "    revenue:gains                               $-25\n",
                "\n",
            )
        );
    }

    /// HIFO disposes of the highest cost lots first.
    #[cfg_attr(test, test)]
    pub(crate) fn hifo_disposes_highest_cost_lots_first() {
        let journal = concat!(
            "commodity AAPL  ; lots: HIFO\n",
            "\n",
            "2026-01-15 buy cheap\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy dear\n",
            "    assets:stocks    10 AAPL {$70}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 buy middle\n",
            "    assets:stocks    10 AAPL {$60}\n",
            "    assets:cash\n",
            "\n",
            "2026-04-01 sell\n",
            "    assets:stocks   -15 AAPL {} @ $65\n",
            "    assets:cash      $975\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 4, 1)).unwrap(),
            concat!(
                "2026-04-01 sell\n",
                "    assets:stocks:{2026-02-01, $70}              -10 AAPL {2026-02-01, $70} @ $65\n",
                "    assets:stocks:{2026-03-01, $60}               -5 AAPL {2026-03-01, $60} @ $65\n",
                "    assets:cash                                 $975\n",
                "    revenue:gains                                $25\n",
                "\n",
            )
        );
    }

    /// AVERAGE disposes of the oldest lots first, at the average cost.
    #[cfg_attr(test, test)]
    pub(crate) fn average_disposes_oldest_lots_first() {
        let journal = concat!(
            "commodity AAPL  ; lots: AVERAGE\n",
            "\n",
            "2026-01-15 buy cheap\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy dear\n",
            "    assets:stocks    10 AAPL {$70}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 buy middle\n",
            "    assets:stocks    10 AAPL {$60}\n",
            "    assets:cash\n",
            "\n",
            "2026-04-01 sell\n",
            "    assets:stocks   -15 AAPL {} @ $65\n",
            "    assets:cash      $975\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 4, 1)).unwrap(),
            concat!(
                "2026-04-01 sell\n",
                "    assets:stocks:{2026-01-15}                   -10 AAPL {2026-01-15, $60} @ $65\n",
                "    assets:stocks:{2026-02-01}                    -5 AAPL {2026-02-01, $60} @ $65\n",
                "    assets:cash                                 $975\n",
                "    revenue:gains                               $-75\n",
                "\n",
            )
        );
    }

    /// A disposal naming a lot takes that one; a transfer moves lots oldest
    /// first, keeping their names under the destination.
    #[cfg_attr(test, test)]
    pub(crate) fn a_named_lot_is_disposed_and_a_transfer_takes_the_oldest() {
        let journal = concat!(
            "commodity AAPL  ; lots:\n",
            "\n",
            "2026-01-15 buy cheap\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy dear\n",
            "    assets:stocks    10 AAPL {$70}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 sell the dear lot\n",
            "    assets:stocks   -4 AAPL {2026-02-01, $70} @ $65\n",
            "    assets:cash      $260\n",
            "    revenue:gains\n",
            "\n",
            "2026-03-02 move\n",
            "    assets:stocks   -12 AAPL {}\n",
            "    assets:ira       12 AAPL {}\n",
        );
        assert_eq!(
            printed(journal, day(2026, 3, 1)).unwrap(),
            concat!(
                "2026-03-01 sell the dear lot\n",
                "    assets:stocks:{2026-02-01, $70}               -4 AAPL {2026-02-01, $70} @ $65\n",
                "    assets:cash                                 $260\n",
                "    revenue:gains                                $20\n",
                "\n",
                "2026-03-02 move\n",
                "    assets:stocks:{2026-01-15, $50}              -10 AAPL {2026-01-15, $50}\n",
                "    assets:stocks:{2026-02-01, $70}               -2 AAPL {2026-02-01, $70}\n",
                "    assets:ira:{2026-01-15, $50}                  10 AAPL {2026-01-15, $50}\n",
                "    assets:ira:{2026-02-01, $70}                   2 AAPL {2026-02-01, $70}\n",
                "\n",
            )
        );
    }

    /// Disposing of more than all the lots hold is an error.
    #[cfg_attr(test, test)]
    pub(crate) fn disposing_more_than_the_lots_hold_is_an_error() {
        let journal = concat!(
            "commodity AAPL  ; lots:\n",
            "\n",
            "2026-01-15 buy\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 sell too many\n",
            "    assets:stocks   -15 AAPL {} @ $55\n",
            "    assets:cash      $825\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap_err(),
            concat!(
                "f.j:7:\n",
                "7 | 2026-02-01 sell too many\n",
                "  |     assets:stocks                                -15 AAPL {} @ $55\n",
                "  |     assets:cash                                 $825\n",
                "  |     revenue:gains\n",
                "\n",
                "Insufficient lots for commodity AAPL in account assets:stocks: need 15 but only 10 available\n",
                "\n",
                "Postings were read as: dispose, unclassified, gain.",
            )
        );
    }

    /// Disposing of more than the lots matching a cost hold is an error,
    /// listing them.
    #[cfg_attr(test, test)]
    pub(crate) fn disposing_more_than_the_matching_lots_hold_is_an_error() {
        let journal = concat!(
            "2026-01-15 buy\n",
            "    assets:stocks    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 sell too many\n",
            "    assets:stocks   -15 AAPL {$50} @ $55\n",
            "    assets:cash      $825\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap_err(),
            concat!(
                "f.j:6:\n",
                "  | 2026-02-01 sell too many\n",
                "6 |     assets:stocks                                -15 AAPL {$50} @ $55\n",
                "  |     assets:cash                                 $825\n",
                "  |     revenue:gains\n",
                "\n",
                "Insufficient lots for commodity AAPL in account assets:stocks: need 15 but only 10 available\n",
                "Lots matching {$50}:\n",
                "  {2026-01-15, $50}  10\n",
                "  Total: 10 AAPL\n",
                "\n",
                "Postings were read as: dispose, unclassified, gain.",
            )
        );
    }

    /// Disposing of a lotful commodity with no lots is an error.
    #[cfg_attr(test, test)]
    pub(crate) fn disposing_with_no_lots_is_an_error() {
        let journal = concat!(
            "commodity AAPL  ; lots:\n",
            "\n",
            "2026-02-01 sell none\n",
            "    assets:stocks   -5 AAPL {} @ $55\n",
            "    assets:cash      $275\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap_err(),
            concat!(
                "f.j:3:\n",
                "3 | 2026-02-01 sell none\n",
                "  |     assets:stocks                                 -5 AAPL {} @ $55\n",
                "  |     assets:cash                                 $275\n",
                "  |     revenue:gains\n",
                "\n",
                "no AAPL lots available for disposal from account assets:stocks on 2026-02-01\n",
                "(To record a short position, use a liability account.)\n",
                "\n",
                "Postings were read as: dispose, unclassified, gain.",
            )
        );
    }

    /// FIFOALL refuses a disposal while another account has an older lot.
    #[cfg_attr(test, test)]
    pub(crate) fn fifoall_wants_the_oldest_lot_of_any_account_first() {
        let journal = concat!(
            "commodity AAPL  ; lots: FIFOALL\n",
            "\n",
            "2026-01-15 buy\n",
            "    assets:a    10 AAPL {$50}\n",
            "    assets:cash\n",
            "\n",
            "2026-02-01 buy\n",
            "    assets:b    10 AAPL {$60}\n",
            "    assets:cash\n",
            "\n",
            "2026-03-01 sell from b\n",
            "    assets:b   -5 AAPL {} @ $65\n",
            "    assets:cash      $325\n",
            "    revenue:gains\n",
        );
        assert_eq!(
            printed(journal, day(2026, 1, 1)).unwrap_err(),
            concat!(
                "f.j:11:\n",
                "11 | 2026-03-01 sell from b\n",
                "   |     assets:b                                      -5 AAPL {} @ $65\n",
                "   |     assets:cash                                 $325\n",
                "   |     revenue:gains\n",
                "\n",
                "FIFOALL: lot(s) on other account(s) have higher priority than the lots in assets:b:\n",
                "  assets:a: {2026-01-15, $50}  10\n",
                "Selected from assets:b:\n",
                "  {2026-02-01, $60}  5\n",
                "Consider disposing from the account(s) listed above first, or use FIFO for per-account scope.\n",
                "Using FIFOALL (from commodity tag on AAPL).\n",
                "\n",
                "Postings were read as: dispose, unclassified, gain.",
            )
        );
    }

    #[test]
    fn labels_skip_used_numbers() {
        let d = Day::from_ymd(2026, 1, 1).unwrap();
        let mut lots = CommodityLots::new();
        assert_eq!(&*generate_label(Some(&lots), d), "0001");
        lots.insert(
            LotId {
                date: d,
                label: Some("0001".into()),
            },
            BTreeMap::new(),
        );
        assert_eq!(&*generate_label(Some(&lots), d), "0002");
    }
}
