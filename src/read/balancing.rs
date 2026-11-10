//! Balancing transactions (Hledger.Data.Balancing): inferring a missing
//! posting amount or a conversion cost, checking that each transaction's
//! postings sum to zero at the entry's precision, and the rounding of
//! amounts calculated in amount expressions.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use super::BalancingOpts;
use super::finalise::convert_alias_amount;
use crate::account::{AccountName, Realness, is_account_name_prefix_of, lot_base_account};
use crate::amount::render::{
    AmountFormat, show_amount_b, show_amounts_distinctly, show_mixed_amount_b,
};
use crate::amount::{
    Amount, AmountCost, CommodityStyles, MixedAmount, Precision, Rounding, Sym,
    styles_with_rounding,
};
use crate::decimal::Decimal;
use crate::model::render::{PostingLayout, show_transaction};
use crate::model::{
    COST_POSTING_TAG, CommodityAliases, GENERATED_POSTING_TAG, IMBALANCE_POSTING_TAG, Journal,
    Posting, Transaction, source_pos_pair_pretty,
};
use crate::regex::regex_escape;
use crate::text::quote_for_command_line;

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
    // nothing to round, as is usual: the styles need not be found
    if !(t.postings.iter())
        .flat_map(|p| p.amount.amounts_raw())
        .any(is_calculated)
    {
        return t;
    }
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

/// Whether an amount was calculated with `*` or `/`: it has more decimal
/// places than its precision.
fn is_calculated(a: &Amount) -> bool {
    match a.style.precision {
        Precision::Fixed(n) => a.internal_precision() > n,
        Precision::Natural => false,
    }
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
        Some(styles) => a.looks_zero_styled(styles),
        None => a.looks_zero(),
    };
    let looks_zero = |a: &MixedAmount| {
        a.is_zero()
            || if bopts.txn_balancing_display {
                looks_zero_globally(a)
            } else {
                let styles = local_styles
                    .get_or_init(|| styles_with_rounding(Rounding::Hard, &t.commodity_styles()));
                a.looks_zero_styled(styles)
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

/// Balance a transaction which could involve lots after tagging its gain
/// postings, trying first with its lot fee outflows split off, since a
/// lot transfer with a priced fee balances only in that form; if that does
/// not balance, the unsplit form is balanced as usual
/// (balanceTransactionHelperMaybeSplittingLotFees).
fn balance_transaction_maybe_splitting_lot_fees(
    bopts: &BalancingOpts,
    t: Transaction,
) -> Result<(Transaction, Vec<(AccountName, MixedAmount)>), String> {
    use crate::lots::{LotLookups, classify, transaction_has_lotful_amounts};
    let lotful = &bopts.lotful_commodities;
    if !transaction_has_lotful_amounts(lotful, &t) {
        return balance_transaction(bopts, t);
    }
    let lk = LotLookups {
        account_types: &bopts.account_types,
        lotful,
        lots_tags: &bopts.account_lots_tags,
    };
    let verbose = bopts.verbose_balancing_tags;
    let t = classify::transaction_tag_gain_postings(!bopts.lenient_lots, verbose, &lk, t);
    if lotful.is_empty() {
        return balance_transaction(bopts, t);
    }
    let t2 = classify::transaction_auto_split_fee_outflows(verbose, &lk, t.clone());
    if t2.postings.len() == t.postings.len() {
        return balance_transaction(bopts, t);
    }
    balance_transaction(bopts, t2).or_else(|_| balance_transaction(bopts, t))
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
    let (mut t, inferred) = infer_balancing_amount(&bopts.inferred_amount_styles, t)?;
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
    let (_, _, ex) = crate::model::errors::transaction_error_excerpt(t, None);
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
/// styles, which have no rounding, so they keep their precision. Returns
/// the transaction and the accounts and amounts inferred, in order
/// (transactionInferBalancingAmount).
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
            sums[i] = std::mem::take(&mut sums[i]) + a;
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
    let flags: Vec<bool> = t.postings.iter().map(&set_aside).collect();
    let mut inferred = Vec::new();
    for (p, set_aside) in t.postings.iter_mut().zip(flags) {
        if set_aside || p.has_amount() {
            continue;
        }
        let a = match p.realness {
            // at most one posting of each kind has no amount
            Realness::Real => std::mem::take(&mut sums[0]),
            Realness::BalancedVirtual => std::mem::take(&mut sums[1]),
            Realness::Virtual => MixedAmount::from_amount(Amount::null()),
        };
        let a = (-a).to_cost();
        let a = if a.amounts_raw().iter().any(|x| !x.is_zero()) {
            a.filter(|x| !x.is_zero())
        } else {
            a
        };
        let a = a.styled(styles);
        p.keep_original();
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
                .map(|x| x.commodity)
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
        p.keep_original();
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
    // a missing amount, or amounts in at most one commodity, sum to at most
    // one amount to show, or to two of one commodity, one of them with a
    // cost: no cost is inferred, and the sum need not be made to see that
    let mut amounts = posting_amounts(&ps);
    if ps.iter().any(|p| p.amount.is_missing())
        || amounts
            .next()
            .is_none_or(|x| amounts.all(|y| y.commodity == x.commodity))
    {
        return None;
    }
    let sum = sum_postings(&ps).amounts();
    let [a, b] = sum.as_slice() else { return None };
    if a.cost.is_some()
        || b.cost.is_some()
        || a.quantity.signum() == b.quantity.signum()
        || !(lenient || lot_mismatch_commodities(lotful, &ps).is_empty())
    {
        return None;
    }
    let commodities: Vec<Sym> = ps
        .iter()
        .flat_map(|p| p.amount.amounts())
        .map(|a| a.commodity)
        .collect();
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

/// Balance all the transactions, inferring missing amounts and costs and
/// doing balance assignments, and check the balance assertions
/// (journalBalanceTransactions); with `defer`, the first assertion failure
/// is returned beside the journal instead of failing
/// (journalBalanceTransactionsAndDeferAssertions). Transactions are
/// numbered first. The journal's commodity styles, with hard rounding, are
/// the display precisions used for checking and for styling inferred
/// amounts.
///
/// First the transactions without balance assignments are balanced on
/// their own. Then, if there are assignments, or assertions to check, the
/// postings of those and the transactions with assignments are processed
/// in date order (same-day items in journal order) with running account
/// balances: assignments set their posting's amount, and the transactions
/// with them are then balanced.
pub fn journal_balance_transactions(
    bopts: &BalancingOpts,
    mut j: Journal,
    defer: bool,
) -> Result<(Journal, Option<String>), String> {
    for (i, t) in j.txns.iter_mut().enumerate() {
        t.index = i as u32 + 1;
    }
    let styles = j.commodity_styles();
    let bopts = BalancingOpts {
        commodity_styles: Some(styles_with_rounding(Rounding::Hard, &styles)),
        inferred_amount_styles: styles_with_rounding(Rounding::No, &styles),
        ..bopts.clone()
    };
    // the transactions without balance assignments are balanced on their
    // own, independently; the first error, in journal order, is reported
    let balanced = crate::par::map_owned(j.txns.len(), std::mem::take(&mut j.txns), |t| {
        if t.postings.iter().any(Posting::has_balance_assignment) {
            return (true, Ok(t));
        }
        let balanced = balance_transaction_maybe_splitting_lot_fees(&bopts, t)
            .map(|(t, _)| transaction_maybe_add_imbalance_postings(&bopts, t).0);
        (false, balanced)
    });
    let mut items = Vec::new();
    for (i, (assigned, t)) in balanced.into_iter().enumerate() {
        let t = t?;
        if assigned {
            items.push(Item::Txn(i));
        } else {
            items.extend((0..t.postings.len()).map(|k| Item::Posting(i, k)));
        }
        j.txns.push(t);
    }
    let mode = if bopts.ignore_assertions {
        Assertions::DontCheck
    } else if defer {
        Assertions::Defer
    } else {
        Assertions::Check
    };
    let has_assignments = items.iter().any(|it| matches!(it, Item::Txn(_)));
    let has_assertions = items
        .iter()
        .any(|it| matches!(it, Item::Posting(i, k) if j.txns[*i].postings[*k].assertion.is_some()));
    if !(has_assignments || (mode != Assertions::DontCheck && has_assertions)) {
        return Ok((j, None));
    }
    let date = |it: &Item| match *it {
        Item::Posting(i, k) => j.txns[i].postings[k].date_in(Some(&j.txns[i])),
        Item::Txn(i) => j.txns[i].date,
    };
    items.sort_by_key(date);
    let mut state = Balancer {
        unassignable: j
            .txn_modifiers
            .iter()
            .flat_map(|tm| tm.rules.iter().map(|r| r.posting.account.clone()))
            .collect(),
        bopts,
        mode,
        aliases: j
            .commodity_aliases()
            .into_iter()
            .filter(|(_, (_, q, _))| *q != Decimal::from_i64(1))
            .collect(),
        balances: BTreeMap::new(),
        deferred: None,
    };
    for it in items {
        match it {
            Item::Posting(i, k) => {
                let t = &j.txns[i];
                let p = &t.postings[k];
                if p.has_amount() {
                    let amount = state.running_balance_amount(p, p.amount.clone().strip_costs());
                    let new = state.add(&p.account, &amount);
                    // only an assertion is checked, against a copy of the
                    // new balance and of the posting as counted
                    if p.assertion.is_some() {
                        let new = new.clone();
                        let mut p = p.clone();
                        p.amount = std::mem::take(&mut p.amount).strip_costs();
                        state.check_or_defer(t, &p, new)?;
                    }
                }
            }
            Item::Txn(i) => {
                let t = std::mem::take(&mut j.txns[i]);
                j.txns[i] = state.balance_with_assignments(t)?;
            }
        }
    }
    Ok((j, state.deferred))
}

/// Check that a transaction's balance assertions hold when it is added to
/// the journal (transactionCheckAssertions, used when adding transactions).
pub fn transaction_check_assertions(
    bopts: &BalancingOpts,
    j: &Journal,
    t: &Transaction,
) -> Result<(), String> {
    if bopts.ignore_assertions {
        return Ok(());
    }
    let mut j = j.clone();
    super::finalise::journal_style_amounts(&mut j)?;
    j.txns.push(t.clone());
    j.txns.sort_by_key(|t| t.date);
    journal_balance_transactions(bopts, j, false).map(|_| ())
}

/// A posting of a transaction balanced on its own, or a transaction with
/// balance assignments, by position in the journal's transactions.
enum Item {
    Posting(usize, usize),
    Txn(usize),
}

/// Whether balance assertions are checked, failing at the first, or checked
/// with the first failure kept for later (AssertionsMode).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Assertions {
    DontCheck,
    Check,
    Defer,
}

/// The state of the date-ordered pass (BalancingState).
struct Balancer {
    bopts: BalancingOpts,
    /// Accounts where balance assignments may not be used, because auto
    /// posting rules post to them.
    unassignable: BTreeSet<AccountName>,
    mode: Assertions,
    /// The commodity aliases with a quantity other than 1, whose amounts the
    /// running balances keep as written, converting them when they are used.
    aliases: CommodityAliases,
    /// Each account's running balance, excluding subaccounts.
    balances: BTreeMap<AccountName, MixedAmount>,
    /// The first assertion failure, when deferring.
    deferred: Option<String>,
}

impl Balancer {
    /// An amount with the amounts in commodity aliases converted, each
    /// alias's total divided only once, so that the result is exact when it
    /// can be (maConvertAliases).
    fn convert(&self, m: MixedAmount) -> MixedAmount {
        if self.aliases.is_empty() {
            return m;
        }
        m.strip_costs()
            .amounts_raw()
            .iter()
            .map(|a| MixedAmount::from_amount(convert_alias_amount(&self.aliases, a).0))
            .sum()
    }

    /// The amount a posting adds to its account's running balance, given
    /// its amount as counted: an amount converted from an alias with a
    /// quantity stays as written, without costs (postingRunningBalanceAmount).
    fn running_balance_amount(&self, p: &Posting, amount: MixedAmount) -> MixedAmount {
        if self.aliases.is_empty() {
            return amount;
        }
        let written = p.original().amount.clone().strip_costs();
        let in_alias = written
            .amounts_raw()
            .iter()
            .any(|a| self.aliases.contains_key(&a.commodity));
        let stripped = amount.clone().strip_costs();
        if in_alias && (&self.convert(written.clone()) - &stripped).is_zero() {
            written
        } else {
            amount
        }
    }

    /// An account's running balance (getRunningBalanceB).
    fn balance(&self, a: &str) -> MixedAmount {
        self.convert(self.balances.get(a).cloned().unwrap_or_default())
    }

    /// An account's running balance including its subaccounts'
    /// (getInclusiveRunningBalanceB).
    fn inclusive_balance(&self, a: &str) -> MixedAmount {
        self.convert(
            self.balances
                .iter()
                .filter(|(b, _)| &***b == a || is_account_name_prefix_of(a, b))
                .map(|(_, m)| m)
                .sum(),
        )
    }

    /// Add an amount to an account's running balance, in place, returning
    /// the new balance (addToRunningBalanceB).
    fn add(&mut self, a: &AccountName, amount: &MixedAmount) -> &MixedAmount {
        let balance = self.balances.entry(a.clone()).or_default();
        *balance = std::mem::take(balance) + amount;
        balance
    }

    /// Set an account's running balance, returning the change
    /// (setRunningBalanceB).
    fn set(&mut self, a: &AccountName, amount: MixedAmount) -> MixedAmount {
        let old = self
            .balances
            .insert(a.clone(), amount.clone())
            .unwrap_or_default();
        &amount - &self.convert(old)
    }

    /// Set an account's running balance so that, with its subaccounts', it
    /// is this amount, returning the change (setInclusiveRunningBalanceB).
    fn set_inclusive(&mut self, a: &AccountName, amount: MixedAmount) -> MixedAmount {
        let old = self.balance(a);
        let subs = self.convert(
            self.balances
                .iter()
                .filter(|(b, _)| is_account_name_prefix_of(a, b))
                .map(|(_, m)| m)
                .sum(),
        );
        let new = &amount - &subs;
        self.balances.insert(a.clone(), new.clone());
        &new - &old
    }

    /// Do a transaction's balance assignments and check its assertions,
    /// posting by posting in date order, then balance it, adding any amount
    /// inferred to its account's running balance
    /// (balanceTransactionAndCheckAssertionsB).
    fn balance_with_assignments(&mut self, t: Transaction) -> Result<Transaction, String> {
        for p in &t.postings {
            self.check_illegal_balance_assignment(&t, p)?;
        }
        let mut order: Vec<usize> = (0..t.postings.len()).collect();
        order.sort_by_key(|&k| t.postings[k].date_in(Some(&t)));
        let mut postings = t.postings.clone();
        for k in order {
            postings[k] = self.add_or_assign(&t, postings[k].clone())?;
        }
        let t = Transaction { postings, ..t };
        let (t, inferred) = balance_transaction_maybe_splitting_lot_fees(&self.bopts, t)?;
        let (t, imbalance) = transaction_maybe_add_imbalance_postings(&self.bopts, t);
        for (a, amount) in inferred
            .into_iter()
            .chain(imbalance.into_iter().map(|p| (p.account, p.amount)))
        {
            self.add(&a, &amount);
        }
        Ok(t)
    }

    /// For a posting with an amount, add it to its account's running
    /// balance; for one with a balance assignment, set the balance and infer
    /// the amount from the change; then check its assertion
    /// (addOrAssignAmountAndCheckAssertionB).
    fn add_or_assign(&mut self, t: &Transaction, mut p: Posting) -> Result<Posting, String> {
        if p.has_amount() {
            let amount = self.running_balance_amount(&p, p.amount.clone());
            let new = self.add(&p.account, &amount).clone();
            self.check_or_defer(t, &p, new)?;
        } else if let Some(ba) = p.assertion.clone() {
            let current = if ba.inclusive {
                self.inclusive_balance(&p.account)
            } else {
                self.balance(&p.account)
            };
            let new = if ba.total {
                MixedAmount::from_amount(ba.amount.clone())
            } else {
                let mut others = current.filter(|a| a.commodity != ba.amount.commodity);
                others.add_amount(ba.amount.clone());
                others
            };
            let diff = if ba.inclusive {
                self.set_inclusive(&p.account, new.clone())
            } else {
                self.set(&p.account, new.clone())
            };
            p.keep_original();
            p.amount = diff.filter(|a| !a.is_zero());
            self.check_or_defer(t, &p, new)?;
        }
        Ok(p)
    }

    /// Check a posting's balance assertion, if any, against the account's
    /// new balance, as the mode says (checkOrDeferBalanceAssertionB).
    fn check_or_defer(
        &mut self,
        t: &Transaction,
        p: &Posting,
        new: MixedAmount,
    ) -> Result<(), String> {
        let new = self.convert(new);
        match self.mode {
            Assertions::DontCheck => Ok(()),
            Assertions::Check => self.check_balance_assertion(t, p, &new),
            Assertions::Defer => {
                if let Err(e) = self.check_balance_assertion(t, p, &new) {
                    self.deferred.get_or_insert(e);
                }
                Ok(())
            }
        }
    }

    /// Check a posting's balance assertion: the asserted commodity's
    /// balance, and for a total assertion, zero in every other commodity of
    /// the balance (checkBalanceAssertionB).
    fn check_balance_assertion(
        &self,
        t: &Transaction,
        p: &Posting,
        actual: &MixedAmount,
    ) -> Result<(), String> {
        let Some(ba) = &p.assertion else {
            return Ok(());
        };
        let others: Vec<Amount> = if ba.total {
            actual
                .amounts_raw()
                .iter()
                .filter(|a| a.commodity != ba.amount.commodity)
                .map(|a| Amount {
                    quantity: Decimal::ZERO,
                    ..a.clone()
                })
                .collect()
        } else {
            Vec::new()
        };
        for asserted in std::iter::once(&ba.amount).chain(&others) {
            self.check_balance_assertion_one_commodity(t, p, asserted, actual)?;
        }
        Ok(())
    }

    /// Check that a commodity's balance in an account, ignoring costs, is
    /// exactly the asserted amount, including subaccounts if the assertion
    /// is inclusive (checkBalanceAssertionOneCommodityB).
    fn check_balance_assertion_one_commodity(
        &self,
        t: &Transaction,
        p: &Posting,
        asserted: &Amount,
        actual: &MixedAmount,
    ) -> Result<(), String> {
        let ba = p.assertion.as_deref().expect("a balance assertion");
        let actual = if ba.inclusive {
            self.inclusive_balance(&p.account)
        } else {
            actual.clone()
        };
        let commodity = &asserted.commodity;
        let asserted = asserted.clone().strip_cost();
        let actual = actual
            .filter_by_commodity(commodity)
            .amounts_raw()
            .iter()
            .fold(Amount::null(), |sum, a| &sum + a);
        if asserted.quantity == actual.quantity {
            return Ok(());
        }
        let (_, _, ex) = crate::model::errors::balance_assertion_error_excerpt(t, p);
        let fmt = AmountFormat {
            zero_commodity: true,
            ..AmountFormat::ONE_LINE
        };
        let (asserted_s, actual_s) = show_amounts_distinctly(&fmt, &asserted, &actual);
        let width = asserted_s.chars().count().max(actual_s.chars().count());
        let pad = |s: &str| format!("{}{s}", " ".repeat(width - s.chars().count()));
        let difference = show_amount_b(&fmt, &(&asserted - &actual)).s;
        let account = &*p.account;
        let which = if ba.total {
            "Across all commodities".to_string()
        } else if commodity.is_empty() {
            "In commodity \"\"".to_string()
        } else {
            format!("In commodity {commodity}")
        };
        let lot_hint = if lot_base_account(account) != account {
            format!(
                "\n\nNote '{account}' is a lot subaccount.\n\
                 Balance assertions aren't supported here unless all postings\n\
                 to this lot mention the lot subaccount name explicitly.\n\
                 So it's probably best to avoid using balance assertions on this account.\n\
                 If needed, you could assert the parent account's balance instead."
            )
        } else {
            String::new()
        };
        let msg = format!(
            "{}:\n{}\n\n\
             Balance assertion failed in {account}\n\
             {which} at this point, {}, ignoring costs,\n\
             the asserted balance is:        {}\n\
             but the calculated balance is:  {}\n\
             (difference: {difference})\n\
             To troubleshoot, check this account's running balance with assertions disabled, eg:\n\
             hledger reg -E --ignore-assertions '{}'{}{}{} -e {lot_hint}\n",
            ba.pos,
            crate::text::chomp(&ex),
            if ba.inclusive {
                "including subaccounts"
            } else {
                "excluding subaccounts"
            },
            pad(&asserted_s),
            pad(&actual_s),
            p.date_in(Some(t)).add_days(1),
            regex_escape(account),
            if ba.inclusive { "" } else { "$" },
            if ba.total {
                String::new()
            } else {
                format!(" cur:{}", quote_for_command_line(&regex_escape(commodity)))
            },
        );
        Err(crate::text::chomp(&msg).to_string())
    }

    /// Fail if a posting's balance assignment has a posting date, or is to
    /// an account auto posting rules post to (checkIllegalBalanceAssignmentB).
    fn check_illegal_balance_assignment(&self, t: &Transaction, p: &Posting) -> Result<(), String> {
        if !p.has_balance_assignment() {
            return Ok(());
        }
        let txn = show_transaction(t, PostingLayout::default(), false);
        let txn = txn.strip_suffix('\n').unwrap_or(&txn);
        if p.date.is_some() {
            return Err(format!(
                "Balance assignments and custom posting dates may not be combined.\n\n{txn}\n\
                 Balance assignments may not be used on postings with a custom posting date\n\
                 (it makes balancing the journal impossible).\n\
                 Please write the posting amount explicitly (or remove the posting date)."
            ));
        }
        if self.unassignable.contains(&p.account) {
            return Err(format!(
                "Balance assignments and auto postings may not be combined.\n\n{txn}\n\
                 Balance assignments may not be used on accounts affected by auto posting rules\n\
                 (it makes balancing the journal impossible).\n\
                 Please write the posting amount explicitly (or remove the auto posting rule(s))."
            ));
        }
        Ok(())
    }
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::{
        AmountStyle, DigitGroups, Side, commodity_styles_from_amounts, eur, num, usd,
    };
    use crate::dates::Day;

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
    #[cfg_attr(test, test)]
    pub(crate) fn inferring_balancing_amounts() {
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
    #[cfg_attr(test, test)]
    pub(crate) fn balancing_single_transactions() {
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
    #[cfg_attr(test, test)]
    pub(crate) fn checking_balance() {
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
    #[cfg_attr(test, test)]
    pub(crate) fn commodity_styles_1091() {
        let style = |groups: Option<DigitGroups>, mark: char, precision: u8| AmountStyle {
            side: Side::L,
            spaced: false,
            digit_groups: groups.map(DigitGroups::intern),
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
            style: style(None, ',', 3),
            ..num(d("1000"))
        };
        let a2 = Amount {
            style: style(comma3(), '.', 2),
            ..num(d("1000"))
        };
        let expected = style(comma3(), '.', 3);
        for amounts in [[a1.clone(), a2.clone()], [a2, a1]] {
            let styles = commodity_styles_from_amounts(&amounts);
            assert_eq!(styles[""], expected);
        }
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn on(date: Day, postings: Vec<Posting>) -> Transaction {
        Transaction {
            date,
            postings,
            ..Transaction::default()
        }
    }

    /// A posting, virtual or not, with an amount (or none) and maybe a
    /// balance assertion: partial or total, exclusive or inclusive (post',
    /// vpost', balassert, balassertParInc, balassertTotInc).
    fn posting(
        account: &str,
        virtual_: bool,
        amount: Option<Amount>,
        assertion: Option<(Amount, bool, bool)>,
    ) -> Posting {
        Posting {
            account: account.into(),
            realness: if virtual_ {
                Realness::Virtual
            } else {
                Realness::Real
            },
            amount: amount.map_or_else(MixedAmount::missing, MixedAmount::from_amount),
            assertion: assertion.map(|(amount, total, inclusive)| {
                Box::new(crate::model::BalanceAssertion {
                    amount,
                    total,
                    inclusive,
                    pos: crate::model::SourcePos::null(),
                })
            }),
            ..Posting::default()
        }
    }

    fn journal(txns: Vec<Transaction>) -> Journal {
        Journal {
            txns,
            ..Journal::default()
        }
    }

    fn balance_journal(txns: Vec<Transaction>) -> Result<Journal, String> {
        journal_balance_transactions(&BalancingOpts::default(), journal(txns), false)
            .map(|(j, _)| j)
    }

    /// samplejournalMaybeExplicit.
    fn sample_journal(explicit: bool) -> Vec<Transaction> {
        let amt = |q: &str, implicit: bool| (explicit || !implicit).then(|| usd(d(q)));
        let p = |a: &str, q: &str, implicit: bool| posting(a, false, amt(q, implicit), None);
        vec![
            on(
                day(2008, 1, 1),
                vec![
                    p("assets:bank:checking", "1", false),
                    p("income:salary", "-1", true),
                ],
            ),
            on(
                day(2008, 6, 1),
                vec![
                    p("assets:bank:checking", "1", false),
                    p("income:gifts", "-1", true),
                ],
            ),
            on(
                day(2008, 6, 2),
                vec![
                    p("assets:bank:saving", "1", false),
                    p("assets:bank:checking", "-1", true),
                ],
            ),
            on(
                day(2008, 6, 3),
                vec![
                    p("expenses:food", "1", false),
                    p("expenses:supplies", "1", false),
                    p("assets:cash", "-2", true),
                ],
            ),
            on(
                day(2008, 10, 1),
                vec![
                    p("assets:bank:checking", "1", false),
                    p("liabilities:debts", "-1", false),
                ],
            ),
            on(
                day(2008, 12, 31),
                vec![
                    p("liabilities:debts", "1", false),
                    p("assets:bank:checking", "-1", true),
                ],
            ),
        ]
    }

    // tests_Balancing "journalBalanceTransactions".
    #[cfg_attr(test, test)]
    pub(crate) fn balancing_journals() {
        let postings = |ts: &[Transaction]| -> Vec<Posting> {
            ts.iter().flat_map(|t| t.postings.clone()).collect()
        };
        let j = balance_journal(sample_journal(false)).unwrap();
        assert_eq!(postings(&j.txns), postings(&sample_journal(true)));
        // balance-assignment
        let n = |q: &str| num(d(q));
        let j = balance_journal(vec![on(
            day(2019, 1, 1),
            vec![posting("a", true, None, Some((n("1"), false, false)))],
        )])
        .unwrap();
        assert_eq!(j.txns[0].postings[0].amount.amounts_raw(), [n("1")]);
        // same-day-1
        assert!(
            balance_journal(vec![
                on(
                    day(2019, 1, 1),
                    vec![posting("a", true, None, Some((n("1"), false, false)))]
                ),
                on(
                    day(2019, 1, 1),
                    vec![posting(
                        "a",
                        true,
                        Some(n("1")),
                        Some((n("2"), false, false))
                    )]
                ),
            ])
            .is_ok()
        );
        // same-day-2
        assert!(
            balance_journal(vec![
                on(
                    day(2019, 1, 1),
                    vec![posting(
                        "a",
                        true,
                        Some(n("2")),
                        Some((n("2"), false, false))
                    )]
                ),
                on(
                    day(2019, 1, 1),
                    vec![
                        posting("b", false, Some(n("1")), None),
                        posting("a", false, None, None)
                    ]
                ),
                on(
                    day(2019, 1, 1),
                    vec![posting(
                        "a",
                        false,
                        Some(n("0")),
                        Some((n("1"), false, false))
                    )]
                ),
            ])
            .is_ok()
        );
        // out-of-order
        assert!(
            balance_journal(vec![
                on(
                    day(2019, 1, 2),
                    vec![posting(
                        "a",
                        true,
                        Some(n("1")),
                        Some((n("2"), false, false))
                    )]
                ),
                on(
                    day(2019, 1, 1),
                    vec![posting(
                        "a",
                        true,
                        Some(n("1")),
                        Some((n("1"), false, false))
                    )]
                ),
            ])
            .is_ok()
        );
    }

    // tests_Balancing "transactionCheckAssertions".
    #[cfg_attr(test, test)]
    pub(crate) fn checking_a_transaction_s_assertions() {
        let bopts = BalancingOpts::default();
        let v = |a: &str, x: Amount| posting(a, true, Some(x), None);
        let check = |ts: Vec<Transaction>, t: Transaction| {
            transaction_check_assertions(&bopts, &journal(ts), &t)
        };
        assert!(
            check(
                vec![on(day(2025, 1, 1), vec![v("a", usd(d("1")))])],
                on(
                    day(2025, 1, 1),
                    vec![posting(
                        "a",
                        true,
                        Some(usd(d("1"))),
                        Some((usd(d("2")), false, false))
                    )]
                )
            )
            .is_ok()
        );
        assert!(
            check(
                vec![
                    on(day(2025, 1, 1), vec![v("a:a", usd(d("1")))]),
                    on(day(2025, 1, 2), vec![v("a:b", usd(d("2")))]),
                    on(day(2025, 1, 2), vec![v("a:c", usd(d("5")))]),
                    on(day(2025, 1, 3), vec![v("a:d", eur(d("10")))]),
                ],
                on(
                    day(2025, 1, 4),
                    vec![posting(
                        "a",
                        true,
                        Some(usd(d("2"))),
                        Some((usd(d("10")), false, true))
                    )]
                )
            )
            .is_ok()
        );
        assert!(
            check(
                vec![
                    on(day(2025, 1, 1), vec![v("a", usd(d("1")))]),
                    on(day(2025, 1, 2), vec![v("a:b", eur(d("2")))]),
                    on(day(2025, 1, 2), vec![v("a:c", usd(d("5")))]),
                    on(day(2025, 1, 3), vec![v("a:b", eur(d("-2")))]),
                ],
                on(
                    day(2025, 1, 3),
                    vec![posting(
                        "a",
                        true,
                        Some(usd(d("2"))),
                        Some((usd(d("8")), true, true))
                    )]
                )
            )
            .is_ok()
        );
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
