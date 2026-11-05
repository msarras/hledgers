//! Balancing transactions (Hledger.Data.Balancing): inferring a missing
//! posting amount or a conversion cost, checking that each transaction's
//! postings sum to zero at the entry's precision, and the rounding of
//! amounts calculated in amount expressions.

use std::cell::OnceCell;
use std::collections::BTreeSet;

use super::BalancingOpts;
use crate::account::{AccountName, Realness};
use crate::amount::render::{AmountFormat, show_mixed_amount_b};
use crate::amount::{
    Amount, AmountCost, CommodityStyles, MixedAmount, Precision, Rounding, Sym,
    styles_with_rounding,
};
use crate::decimal::Decimal;
use crate::model::{
    COST_POSTING_TAG, GENERATED_POSTING_TAG, IMBALANCE_POSTING_TAG, Journal, Posting, Transaction,
    source_pos_pair_pretty,
};

/// The internal tag of postings whose amount was calculated with `*` or `/`
/// and changed by rounding; the balancer removes it (roundedPostingTagName).
pub const ROUNDED_POSTING_TAG: &str = "_rounded-posting";

/// Round the amounts calculated with `*` or `/` in a transaction's posting
/// amount expressions, each to the most decimal places its commodity has in
/// the entry's written amounts (balance assignments counting with their
/// assigned amounts), or its own precision if that is more. Postings
/// changed by this are tagged, so that an unbalanced entry's error can
/// mention the rounding (transactionRoundCalculatedAmounts).
pub fn transaction_round_calculated_amounts(mut t: Transaction) -> Transaction {
    let assigned: Vec<Posting> = t
        .postings
        .iter()
        .map(|p| match &p.assertion {
            Some(ba) if p.has_balance_assignment() => Posting {
                amount: MixedAmount::from_amount(ba.amount.clone()),
                ..p.clone()
            },
            _ => p.clone(),
        })
        .collect();
    let styles = Transaction {
        postings: assigned,
        ..Transaction::default()
    }
    .commodity_styles();
    for p in &mut t.postings {
        if posting_round_calculated_amounts(&styles, p) {
            p.add_tags([(ROUNDED_POSTING_TAG.to_string(), String::new())]);
        }
    }
    t
}

/// Round the amounts in this posting calculated with `*` or `/`, which are
/// those with more decimal places than their precision, half to even, to
/// their commodity's precision in these styles or their own if that is more.
/// Returns whether any quantity changed (postingRoundCalculatedAmounts).
pub fn posting_round_calculated_amounts(styles: &CommodityStyles, p: &mut Posting) -> bool {
    let is_calculated = |a: &Amount| match a.style.precision {
        Precision::Fixed(n) => a.internal_precision() > n,
        Precision::Natural => false,
    };
    if !p.amount.amounts_raw().iter().any(is_calculated) {
        return false;
    }
    let rounded = |a: &Amount| {
        let places = match (
            a.style.precision,
            styles.get(&a.commodity).map(|s| s.precision),
        ) {
            (Precision::Fixed(n), Some(Precision::Fixed(m))) => n.max(m),
            (Precision::Fixed(n), _) => n,
            (Precision::Natural, _) => a.internal_precision(),
        };
        let a = a.clone().with_precision(Precision::Fixed(places));
        Amount {
            quantity: a.rounded_quantity(),
            ..a
        }
    };
    let changed = p
        .amount
        .amounts_raw()
        .iter()
        .any(|a| is_calculated(a) && rounded(a).quantity != a.quantity);
    p.amount = p.amount.map(|a| {
        if is_calculated(a) {
            rounded(a)
        } else {
            a.clone()
        }
    });
    changed
}

/// Whether a posting is a disposal's realised gain posting, which balancing
/// sets aside, unless the transaction has equity conversion postings
/// (isSetAsideGainPosting).
fn set_aside_test(t: &Transaction) -> impl Fn(&Posting) -> bool + use<> {
    let has_cost_postings = t.postings.iter().any(|p| p.has_tag(COST_POSTING_TAG));
    move |p: &Posting| {
        !has_cost_postings && p.tags.iter().any(|(n, v)| n == "_ptype" && v == "gain")
    }
}

/// A posting's amount as it counts towards its transaction's balance: at
/// cost, or without its cost if that is represented by equity conversion
/// postings, or nothing if it is set aside (postingBalancingAmountWith).
fn posting_balancing_amount(set_aside: bool, p: &Posting) -> MixedAmount {
    if p.has_tag(COST_POSTING_TAG) {
        p.amount.clone().strip_costs()
    } else if set_aside {
        MixedAmount::null()
    } else {
        p.amount.clone().to_cost()
    }
}

/// The problems that keep this transaction from looking balanced, if any
/// (transactionCheckBalanced). For the real postings and, separately, the
/// balanced virtual postings, the amounts at cost must sum to zero, or to
/// something that looks zero at the entry's own precisions, or with
/// `--txn-tolerance=display`, at the display precisions. When two or more
/// look non-zero but have the same sign, that is reported instead.
pub fn transaction_check_balanced(bopts: &BalancingOpts, t: &Transaction) -> Vec<String> {
    let set_aside = set_aside_test(t);
    let of =
        |r: Realness| -> Vec<&Posting> { t.postings.iter().filter(|p| p.realness == r).collect() };
    let (rps, bvps) = (of(Realness::Real), of(Realness::BalancedVirtual));
    let balancing = |p: &Posting| posting_balancing_amount(set_aside(p), p);
    let local_styles = OnceCell::new();
    let looks_zero_globally = |a: &MixedAmount| match &bopts.commodity_styles {
        Some(styles) => a.clone().styled(styles).looks_zero(),
        None => a.looks_zero(),
    };
    let looks_zero = |a: &MixedAmount| {
        a.is_zero()
            || if bopts.txn_balancing_display {
                looks_zero_globally(a)
            } else {
                let styles = local_styles
                    .get_or_init(|| styles_with_rounding(Rounding::Hard, &t.commodity_styles()));
                a.clone().styled(styles).looks_zero()
            }
    };
    let signs_ok = |ps: &[&Posting]| {
        let nonzeros: Vec<MixedAmount> = ps
            .iter()
            .map(|p| balancing(p))
            .filter(|a| !looks_zero(a))
            .collect();
        let signs: BTreeSet<bool> = nonzeros
            .iter()
            .filter_map(MixedAmount::is_negative)
            .collect();
        nonzeros.len() < 2 || signs.len() > 1
    };
    let show = |a: &MixedAmount| {
        let fmt = AmountFormat {
            zero_commodity: true,
            ..AmountFormat::ONE_LINE_NO_COST
        };
        show_mixed_amount_b(
            &fmt,
            &a.clone()
                .with_full_precision()
                .with_full_precision_up_to(None),
        )
        .s
    };
    let message = |ps: &[&Posting], real: bool| -> String {
        let sum: MixedAmount = ps.iter().map(|p| balancing(p)).sum();
        if looks_zero(&sum) {
            return String::new();
        }
        let (kind, colon, plus, eq) = if real {
            ("real", "", "  +  ", "  =  ")
        } else {
            ("balanced virtual", ":", " + ", " = ")
        };
        if !signs_ok(ps) {
            return format!("The {kind} postings all have the same sign.");
        }
        let parts: Vec<String> = ps
            .iter()
            .filter(|p| !set_aside(p))
            .map(|p| show(&balancing(p)))
            .collect();
        let mut msg = format!(
            "The {kind} postings' sum should be 0 but is{colon} {}\n  {}{eq}{}",
            show(&sum),
            parts.join(plus),
            show(&sum)
        );
        if ps.iter().any(|p| set_aside(p)) {
            msg.push_str("  (excluding gain postings)");
        }
        if ps.iter().any(|p| p.has_tag(ROUNDED_POSTING_TAG)) {
            msg.push_str(
                "\nNote, amounts calculated with * or / are rounded; to split unevenly, leave one amount blank.",
            );
        }
        if looks_zero_globally(&sum) {
            msg.push_str(concat!(
                "\nNote, hledger <1.50 accepted this entry because of the global display precision,\n",
                "but hledger 1.50+ checks more strictly, using the entry's local precision.\n",
                "You can use --txn-tolerance=display to keep it working, or fix it (recommended);\n",
                "see 'Transaction balancing' in the hledger manual.\n",
            ));
        }
        let mismatched = lot_mismatch_commodities(&bopts.lotful_commodities, ps);
        if !mismatched.is_empty() {
            let cs: Vec<&str> = mismatched.iter().map(|c| &**c).collect();
            msg.push_str(&format!(
                "\nNote, {} is a lot-tracked commodity being transferred between accounts,\n",
                cs.join(", ")
            ));
            msg.push_str(concat!(
                "so no balancing conversion cost was inferred; its quantities are expected\n",
                "to add up. If the difference is a fee, you can either\n",
                "- record the fee expense in the lot-tracked commodity, with posting(s)\n",
                "  adding up to the missing quantity\n",
                "- or split the sending posting into a transfer part and fee part(s)\n",
                "  matching the fee expense(s).\n",
            ));
            if !bopts.lenient_lots {
                msg.push_str(
                    "(With --ignore-lots, a balancing conversion cost is inferred instead.)\n",
                );
            }
        }
        msg
    };
    [message(&rps, true), message(&bvps, false)]
        .into_iter()
        .filter(|m| !m.is_empty())
        .collect()
}

/// Postings to the imbalance account that make a balanced transaction's
/// tolerated imbalance, if any, exact: one per commodity of the real
/// postings' imbalance, then likewise for the balanced virtual postings,
/// tagged as generated imbalance postings, and as if their amount had been
/// inferred (transactionImbalancePostings).
fn transaction_imbalance_postings(
    verbose_tags: bool,
    account: &str,
    styles: &CommodityStyles,
    t: &Transaction,
) -> Vec<Posting> {
    let set_aside = set_aside_test(t);
    let styles = styles_with_rounding(Rounding::No, styles);
    let mut out = Vec::new();
    for r in [Realness::Real, Realness::BalancedVirtual] {
        let sum: MixedAmount = t
            .postings
            .iter()
            .filter(|p| p.realness == r)
            .map(|p| posting_balancing_amount(set_aside(p), p))
            .sum();
        for a in sum.amounts() {
            if a.clone().with_full_precision_up_to(None).looks_zero() {
                continue;
            }
            let mut p = Posting {
                account: AccountName::from(account),
                realness: r,
                amount: MixedAmount::from_amount(-a).styled(&styles),
                ..Posting::default()
            };
            p.add_hidden_and_maybe_visible_tag(
                false,
                verbose_tags,
                (IMBALANCE_POSTING_TAG.into(), String::new()),
            );
            p.add_hidden_and_maybe_visible_tag(
                false,
                verbose_tags,
                (GENERATED_POSTING_TAG.into(), String::new()),
            );
            p.original = Some(Box::new(Posting {
                amount: MixedAmount::missing(),
                ..p.clone()
            }));
            out.push(p);
        }
    }
    out
}

/// With `--infer-imbalance`, add the postings showing a balanced
/// transaction's tolerated imbalance; returns them too
/// (transactionMaybeAddImbalancePostings).
fn transaction_maybe_add_imbalance_postings(
    bopts: &BalancingOpts,
    mut t: Transaction,
) -> (Transaction, Vec<Posting>) {
    if !bopts.infer_imbalance {
        return (t, Vec::new());
    }
    let none = CommodityStyles::new();
    let styles = bopts.commodity_styles.as_ref().unwrap_or(&none);
    let ips = transaction_imbalance_postings(
        bopts.verbose_balancing_tags,
        &bopts.imbalance_account,
        styles,
        &t,
    );
    t.postings.extend(ips.iter().cloned());
    (t, ips)
}

/// Balance one transaction on its own, inferring a missing amount or
/// conversion costs if needed (balanceSingleTransaction).
pub fn balance_single_transaction(
    bopts: &BalancingOpts,
    t: Transaction,
) -> Result<Transaction, String> {
    balance_transaction(bopts, t).map(|(t, _)| t)
}

/// Balance a transaction, which in a journal using lots might first need
/// its lot fee outflows split off (balanceTransactionHelperMaybeSplittingLotFees).
// M6: lots (tagging gain postings, splitting lot fees); until then this is plain balancing.
fn balance_transaction_maybe_splitting_lot_fees(
    bopts: &BalancingOpts,
    t: Transaction,
) -> Result<(Transaction, Vec<(AccountName, MixedAmount)>), String> {
    balance_transaction(bopts, t)
}

/// Balance a transaction: infer conversion costs (if allowed) and a missing
/// amount, then check that it is balanced. Returns it, with the accounts and
/// amounts inferred, or the error (balanceTransactionHelper).
pub fn balance_transaction(
    bopts: &BalancingOpts,
    t: Transaction,
) -> Result<(Transaction, Vec<(AccountName, MixedAmount)>), String> {
    let t = if bopts.infer_balancing_costs {
        infer_balancing_costs(bopts.lenient_lots, &bopts.lotful_commodities, t)
    } else {
        t
    };
    let none = CommodityStyles::new();
    let styles = bopts.commodity_styles.as_ref().unwrap_or(&none);
    let (mut t, inferred) = infer_balancing_amount(styles, t)?;
    let errs = transaction_check_balanced(bopts, &t);
    if !errs.is_empty() {
        let mut all = Vec::new();
        if transaction_commodities(&t).len() > 1 && !bopts.infer_balancing_costs {
            all.push(
                "Automatic commodity conversion is disabled by strict mode or the balanced check."
                    .to_string(),
            );
        }
        all.extend(errs);
        return Err(transaction_balance_error(&t, &all));
    }
    for p in &mut t.postings {
        p.tags.retain(|(n, _)| n != ROUNDED_POSTING_TAG);
    }
    Ok((t, inferred))
}

/// The commodities of a transaction's posting amounts, with `AUTO` for a
/// missing amount (transactionCommodities).
fn transaction_commodities(t: &Transaction) -> BTreeSet<Sym> {
    t.postings
        .iter()
        .flat_map(|p| p.amount.commodities())
        .collect()
}

/// An unbalanced transaction error: the transaction as an excerpt, then
/// these explanations (transactionBalanceError).
pub fn transaction_balance_error(t: &Transaction, errs: &[String]) -> String {
    let (_, _, ex) = super::errors::transaction_error_excerpt(t, None);
    let multi = if transaction_commodities(t).len() > 1 {
        "multi-commodity "
    } else {
        ""
    };
    let errs: String = errs.iter().map(|e| format!("{e}\n")).collect();
    format!(
        "{}:\n{}\n\nThis {multi}transaction is unbalanced.\n{}",
        source_pos_pair_pretty(&t.pos),
        crate::text::chomp(&ex),
        crate::text::chomp(&errs)
    )
}

/// Infer the amount of the one real posting with no amount, and likewise of
/// the one balanced virtual posting, from the others' amounts at cost; a
/// virtual posting with no amount gets zero. Inferred amounts get these
/// styles, keeping their precision. Returns the transaction and the
/// accounts and amounts inferred, in order (transactionInferBalancingAmount).
fn infer_balancing_amount(
    styles: &CommodityStyles,
    mut t: Transaction,
) -> Result<(Transaction, Vec<(AccountName, MixedAmount)>), String> {
    if t.postings.iter().all(Posting::has_amount) {
        return Ok((t, Vec::new()));
    }
    let set_aside = set_aside_test(&t);
    let mut sums = [MixedAmount::null(), MixedAmount::null()];
    let mut amountless = [0, 0];
    for p in t.postings.iter().filter(|p| !set_aside(p)) {
        let i = match p.realness {
            Realness::Real => 0,
            Realness::BalancedVirtual => 1,
            Realness::Virtual => continue,
        };
        if p.has_amount() {
            let a = if p.has_tag(COST_POSTING_TAG) {
                p.amount.clone().strip_costs()
            } else {
                p.amount.clone().to_cost()
            };
            sums[i] = &sums[i] + &a;
        } else {
            amountless[i] += 1;
        }
    }
    for (i, kind) in ["real", "balanced virtual"].iter().enumerate() {
        if amountless[i] > 1 {
            return Err(transaction_balance_error(
                &t,
                &[
                    format!("There can't be more than one {kind} posting with no amount."),
                    "(Remember to put two or more spaces between account and amount.)".to_string(),
                ],
            ));
        }
    }
    let styles = styles_with_rounding(Rounding::No, styles);
    let flags: Vec<bool> = t.postings.iter().map(&set_aside).collect();
    let mut inferred = Vec::new();
    for (p, set_aside) in t.postings.iter_mut().zip(flags) {
        if set_aside || p.has_amount() {
            continue;
        }
        let a = match p.realness {
            Realness::Real => sums[0].clone(),
            Realness::BalancedVirtual => sums[1].clone(),
            Realness::Virtual => MixedAmount::from_amount(Amount::null()),
        };
        let a = (-a).to_cost();
        let a = if a.amounts_raw().iter().any(|x| !x.is_zero()) {
            a.filter(|x| !x.is_zero())
        } else {
            a
        };
        let a = a.styled(&styles);
        p.original = Some(Box::new(p.original().clone()));
        p.amount = a.clone();
        inferred.push((p.account.clone(), a));
    }
    Ok((t, inferred))
}

/// The amounts of some postings.
fn posting_amounts<'a>(ps: &'a [&Posting]) -> impl Iterator<Item = &'a Amount> + Clone {
    ps.iter().flat_map(|p| p.amount.amounts_raw())
}

/// The sum of some postings' amounts (sumPostings).
fn sum_postings(ps: &[&Posting]) -> MixedAmount {
    ps.iter().map(|p| &p.amount).sum()
}

/// Whether a commodity has a cost basis in one of these postings
/// (commodityHasLotPosting).
fn commodity_has_lot_posting(ps: &[&Posting], c: &Sym) -> bool {
    posting_amounts(ps).any(|a| &a.commodity == c && a.cost_basis.is_some())
}

/// Whether a commodity is lot-related (has a cost basis here, or is
/// declared lotful) and appears with both signs in these postings
/// (lotRelatedBothSigns).
fn lot_related_both_signs(lotful: &BTreeSet<Sym>, ps: &[&Posting], c: &Sym) -> bool {
    (commodity_has_lot_posting(ps, c) || lotful.contains(c))
        && posting_amounts(ps).any(|a| &a.commodity == c && a.quantity > Decimal::ZERO)
        && posting_amounts(ps).any(|a| &a.commodity == c && a.quantity < Decimal::ZERO)
}

/// The lot-related commodities whose quantities in these postings look
/// mismatched: appearing with both signs, yet leaving a residual that is
/// not one posting's amount, when the postings sum to two unpriced
/// commodities of opposite sign (lotMismatchCommodities).
fn lot_mismatch_commodities(lotful: &BTreeSet<Sym>, ps: &[&Posting]) -> Vec<Sym> {
    match sum_postings(ps).amounts().as_slice() {
        [a, b]
            if a.cost.is_none()
                && b.cost.is_none()
                && a.quantity.signum() != b.quantity.signum() =>
        {
            [a, b]
                .into_iter()
                .filter(|x| {
                    lot_related_both_signs(lotful, ps, &x.commodity)
                        && !posting_amounts(ps)
                            .any(|y| y.commodity == x.commodity && y.quantity == x.quantity)
                })
                .map(|x| x.commodity.clone())
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Infer conversion costs that make the real postings, and separately the
/// balanced virtual postings, balance, when their amounts sum to exactly
/// two commodities of opposite sign with no costs: the postings of the
/// commodity converted from get a total cost if there is one of them, or
/// else an averaged unit cost (transactionInferBalancingCosts).
fn infer_balancing_costs(lenient: bool, lotful: &BTreeSet<Sym>, mut t: Transaction) -> Transaction {
    let real = cost_inferrer_for(lenient, lotful, &t, Realness::Real);
    let bv = cost_inferrer_for(lenient, lotful, &t, Realness::BalancedVirtual);
    if real.is_none() && bv.is_none() {
        return t;
    }
    for p in &mut t.postings {
        for inferrer in [&real, &bv].into_iter().flatten() {
            inferrer.apply(p);
        }
    }
    t
}

/// How to give postings of one realness a conversion cost (costInferrerFor).
struct CostInferrer {
    realness: Realness,
    from: Amount,
    /// The only posting quantity of `from`'s commodity to price, if one
    /// posting is targeted (isTargeted).
    targeted: Option<Decimal>,
    cost: AmountCost,
}

impl CostInferrer {
    /// Give the posting the cost, if it has a single amount in the converted
    /// commodity (and is the targeted one, if one is).
    fn apply(&self, p: &mut Posting) {
        let amounts = p.amount.amounts();
        let [a] = amounts.as_slice() else { return };
        if p.realness != self.realness
            || a.commodity != self.from.commodity
            || self.targeted.as_ref().is_some_and(|q| &a.quantity != q)
        {
            return;
        }
        let priced = Amount {
            cost: Some(self.cost.clone()),
            ..a.clone()
        };
        p.original = Some(Box::new(p.original().clone()));
        p.amount = MixedAmount::from_amount(priced);
    }
}

fn cost_inferrer_for(
    lenient: bool,
    lotful: &BTreeSet<Sym>,
    t: &Transaction,
    realness: Realness,
) -> Option<CostInferrer> {
    let set_aside = set_aside_test(t);
    let ps: Vec<&Posting> = t
        .postings
        .iter()
        .filter(|p| p.realness == realness && !set_aside(p))
        .collect();
    let commodities: Vec<Sym> = ps
        .iter()
        .flat_map(|p| p.amount.amounts())
        .map(|a| a.commodity)
        .collect();
    let sum = sum_postings(&ps).amounts();
    let [a, b] = sum.as_slice() else { return None };
    if a.cost.is_some()
        || b.cost.is_some()
        || a.quantity.signum() == b.quantity.signum()
        || !(lenient || lot_mismatch_commodities(lotful, &ps).is_empty())
    {
        return None;
    }
    let prefer = |has: &dyn Fn(&Sym) -> bool| match (has(&a.commodity), has(&b.commodity)) {
        (true, false) => Some((a, b)),
        (false, true) => Some((b, a)),
        _ => None,
    };
    let (from, to) = prefer(&|c| commodity_has_lot_posting(&ps, c))
        .or_else(|| prefer(&|c| lotful.contains(c)))
        .or_else(|| {
            commodities.iter().find_map(|c| {
                if c == &a.commodity {
                    Some((a, b))
                } else if c == &b.commodity {
                    Some((b, a))
                } else {
                    None
                }
            })
        })?;
    let targeted = (lot_related_both_signs(lotful, &ps, &from.commodity)
        && ps
            .iter()
            .filter(|q| {
                matches!(q.amount.amounts().as_slice(),
                    [x] if x.commodity == from.commodity && x.quantity == from.quantity)
            })
            .count()
            == 1)
        .then(|| from.quantity.clone());
    let cost = if targeted.is_some()
        || commodities.iter().filter(|c| **c == from.commodity).count() == 1
    {
        AmountCost::Total(Box::new(-to))
    } else {
        let unit = to.clone().divide(&from.quantity);
        let precision = match (from.style.precision, to.style.precision) {
            (Precision::Fixed(x), Precision::Fixed(y)) => {
                Precision::Fixed(x.saturating_add(y).max(2))
            }
            _ => Precision::Natural,
        };
        let u = (-unit).with_precision(precision);
        let shown = u.display_precision();
        AmountCost::Unit(Box::new(
            u.with_full_precision_up_to(None).with_precision_min(shown),
        ))
    };
    Some(CostInferrer {
        realness,
        from: from.clone(),
        targeted,
        cost,
    })
}

/// Balance all the transactions, inferring missing amounts and costs
/// (journalBalanceTransactions), and with `defer`, return the first balance
/// assertion failure separately instead of failing
/// (journalBalanceTransactionsAndDeferAssertions). Transactions are
/// numbered first. The journal's commodity styles, with hard rounding, are
/// the display precisions used for checking and for styling inferred
/// amounts.
pub fn journal_balance_transactions(
    bopts: &BalancingOpts,
    mut j: Journal,
    _defer: bool,
) -> Result<(Journal, Option<String>), String> {
    for (i, t) in j.txns.iter_mut().enumerate() {
        t.index = i as u32 + 1;
    }
    let bopts = BalancingOpts {
        commodity_styles: Some(styles_with_rounding(Rounding::Hard, &j.commodity_styles())),
        ..bopts.clone()
    };
    let txns = std::mem::take(&mut j.txns);
    let mut balanced = Vec::with_capacity(txns.len());
    for t in txns {
        // M2c: balance assignments and assertions; transactions with assignments are left unbalanced until then.
        if t.postings.iter().any(Posting::has_balance_assignment) {
            balanced.push(t);
            continue;
        }
        let (t, _) = balance_transaction_maybe_splitting_lot_fees(&bopts, t)?;
        let (t, _) = transaction_maybe_add_imbalance_postings(&bopts, t);
        balanced.push(t);
    }
    j.txns = balanced;
    Ok((j, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::{
        AmountStyle, DigitGroups, Side, commodity_styles_from_amounts, eur, num, usd,
    };
    use crate::dates::Day;
    use std::sync::Arc;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn post(account: &str, a: Amount) -> Posting {
        Posting::new(account, a)
    }

    fn missing(account: &str) -> Posting {
        Posting {
            account: account.into(),
            amount: MixedAmount::missing(),
            ..Posting::default()
        }
    }

    fn txn(postings: Vec<Posting>) -> Transaction {
        Transaction {
            date: Day::from_ymd(2007, 1, 28).unwrap(),
            postings,
            ..Transaction::default()
        }
    }

    fn balanced(postings: Vec<Posting>) -> bool {
        transaction_check_balanced(&BalancingOpts::default(), &txn(postings)).is_empty()
    }

    // tests_Balancing "transactionInferBalancingAmount".
    #[test]
    fn inferring_balancing_amounts() {
        let none = CommodityStyles::new();
        let infer = |ps| infer_balancing_amount(&none, txn(ps)).unwrap().0.postings;
        assert_eq!(infer(vec![]), vec![]);
        assert_eq!(
            infer(vec![post("a", usd(d("-5"))), missing("b")]),
            vec![post("a", usd(d("-5"))), post("b", usd(d("5")))]
        );
        assert_eq!(
            infer(vec![
                post("a", usd(d("-5"))),
                post("b", eur(d("3")).at_total(usd(d("4")))),
                missing("c")
            ]),
            vec![
                post("a", usd(d("-5"))),
                post("b", eur(d("3")).at_total(usd(d("4")))),
                post("c", usd(d("1")))
            ]
        );
    }

    // tests_Balancing "balanceSingleTransaction".
    #[test]
    fn balancing_single_transactions() {
        let bopts = BalancingOpts::default();
        let balance = |ps| balance_single_transaction(&bopts, txn(ps));
        assert!(balance(vec![post("a", usd(d("1"))), post("b", usd(d("1")))]).is_err());
        assert!(balance(vec![missing("a"), missing("b")]).is_err());
        let t = balance(vec![post("a", usd(d("1"))), missing("b")]).unwrap();
        assert_eq!(t.postings[1].amount, MixedAmount::from_amount(usd(d("-1"))));
        let t = balance(vec![post("a", usd(d("1.35"))), post("b", eur(d("-1")))]).unwrap();
        assert_eq!(
            t.postings[0].amount,
            MixedAmount::from_amount(usd(d("1.35")).at_total(eur(d("1"))))
        );
        assert!(
            balance(vec![
                post("a", usd(d("1")).at(eur(d("2")))),
                post("a", usd(d("-2")).at(eur(d("1"))))
            ])
            .is_ok()
        );
        assert!(
            balance(vec![
                post("a", usd(d("1")).at_total(eur(d("1")))),
                post("a", usd(d("-2")).at_total(eur(d("-1"))))
            ])
            .is_ok()
        );
    }

    // tests_Balancing "isTransactionBalanced".
    #[test]
    fn checking_balance() {
        let virt = |a: &str, x: &str, r: Realness| Posting {
            realness: r,
            ..post(a, usd(d(x)))
        };
        assert!(balanced(vec![
            post("b", usd(d("1.00"))),
            post("c", usd(d("-1.00")))
        ]));
        assert!(!balanced(vec![
            post("b", usd(d("1.00"))),
            post("c", usd(d("-1.01")))
        ]));
        assert!(!balanced(vec![post("b", usd(d("1.00")))]));
        assert!(balanced(vec![post("b", usd(d("0")))]));
        assert!(balanced(vec![
            post("b", usd(d("1.00"))),
            post("c", usd(d("-1.00"))),
            virt("d", "100", Realness::Virtual)
        ]));
        assert!(!balanced(vec![
            post("b", usd(d("1.00"))),
            post("c", usd(d("-1.00"))),
            virt("d", "100", Realness::BalancedVirtual)
        ]));
        assert!(balanced(vec![
            post("b", usd(d("1.00"))),
            post("c", usd(d("-1.00"))),
            virt("d", "100", Realness::BalancedVirtual),
            virt("3", "-100", Realness::BalancedVirtual)
        ]));
    }

    // tests_Balancing "commodityStylesFromAmounts" 1091a, 1091b.
    #[test]
    fn commodity_styles_1091() {
        let style = |groups: Option<DigitGroups>, mark: char, precision: u8| AmountStyle {
            side: Side::L,
            spaced: false,
            digit_groups: groups,
            decimal_mark: Some(mark),
            precision: Precision::Fixed(precision),
            rounding: Rounding::No,
        };
        let comma3 = || {
            Some(DigitGroups {
                sep: ',',
                sizes: vec![3],
            })
        };
        let a1 = Amount {
            style: Arc::new(style(None, ',', 3)),
            ..num(d("1000"))
        };
        let a2 = Amount {
            style: Arc::new(style(comma3(), '.', 2)),
            ..num(d("1000"))
        };
        let expected = style(comma3(), '.', 3);
        for amounts in [[a1.clone(), a2.clone()], [a2, a1]] {
            let styles = commodity_styles_from_amounts(&amounts);
            assert_eq!(*styles[""], expected);
        }
    }

    #[test]
    fn rounding_calculated_amounts() {
        // ($100/3) beside $-100 rounds to $33, and is tagged
        let third = usd(d("100"))
            .divide(&d("3"))
            .with_precision(Precision::Fixed(0));
        let t = transaction_round_calculated_amounts(txn(vec![
            post("a", usd(d("-100")).with_precision(Precision::Fixed(0))),
            post("b", third),
        ]));
        assert_eq!(t.postings[1].amount, MixedAmount::from_amount(usd(d("33"))));
        assert!(t.postings[1].has_tag(ROUNDED_POSTING_TAG));
        assert!(!t.postings[0].has_tag(ROUNDED_POSTING_TAG));
    }
}
