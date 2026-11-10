//! The lot stages around transaction balancing (from Hledger.Data.Lots,
//! with journalInferPostingsTransactedCost from Hledger.Data.Journal and
//! journalInferBasisFromAccountNames from Hledger.Read.Common): before
//! balancing, cost bases are inferred from lot subaccount names and
//! transacted costs from cost bases, acquisitions' bases are checked, and
//! user-written gain postings are tagged; after it, lot postings are
//! classified as acquire, dispose, transfer-from or transfer-to, a
//! transfer's fee outflows first split off.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    LotDirection, LotLookups, has_balancer_copied_basis, is_classified_posting, is_gain_posting,
    lot_direction, transaction_has_lotful_amounts,
};
use crate::account::{AccountName, AccountType, lot_base_account};
use crate::amount::render::{AmountFormat, show_amounts_distinctly};
use crate::amount::{Amount, AmountCost, MixedAmount, Sym};
use crate::decimal::Decimal;
use crate::model::errors::posting_error_excerpt_by_index;
use crate::model::{
    COST_POSTING_TAG, FEESPLIT_POSTING_TAG, GENERATED_POSTING_TAG, Journal, Posting, Transaction,
};

fn has_cost_basis(p: &Posting) -> bool {
    p.amount
        .amounts_raw()
        .iter()
        .any(|a| a.cost_basis.is_some())
}

fn any_cost(amts: &[Amount]) -> bool {
    amts.iter().any(|a| a.cost.is_some())
}

fn any_negative(amts: &[Amount]) -> bool {
    amts.iter().any(Amount::is_negative)
}

fn hidden_tag(name: &str, value: &str) -> crate::model::Tag {
    (name.to_string(), value.to_string())
}

/// An amount's cost per unit: its unit cost, or its total cost divided by
/// its quantity, with the quotient's decimal places (amountCostToUnitCost).
pub fn amount_cost_to_unit_cost(qty: &Decimal, cost: &AmountCost) -> Amount {
    match cost {
        AmountCost::Unit(c) => (**c).clone(),
        AmountCost::Total(c) => (**c).clone().divide_and_update_precision(qty),
    }
}

/// In a lot transfer with a fee, split the sending posting into a transfer
/// part, matching what asset accounts receive, and dispose parts matching
/// the fee postings in the lotful commodity (taking their costs), so that
/// classification sees the two roles. The fee parts, first, are tagged as
/// generated fee splits (transactionAutoSplitFeeOutflows).
pub fn transaction_auto_split_fee_outflows(
    verbose: bool,
    lk: &LotLookups,
    mut t: Transaction,
) -> Transaction {
    let ps = &t.postings;
    let is_asset = |acct: &str| lk.is_asset(lot_base_account(acct));
    let opted_out =
        |p: &Posting| lk.uses_no_lots(lot_base_account(&p.account)) && !has_cost_basis(p);
    // the first amount in a non-lot account equal to the fee, or else all
    // of their positive amounts, if they add up to it
    let fee_counterparts = |comm: &Sym, qty: &Decimal| -> Option<Vec<Amount>> {
        let candidates: Vec<&Amount> = ps
            .iter()
            .filter(|q| lk.direction(&q.account).is_none())
            .flat_map(|q| q.amount.amounts_raw())
            .filter(|a| &a.commodity == comm && a.quantity > Decimal::ZERO)
            .collect();
        if let Some(fa) = candidates.iter().find(|a| &a.quantity == qty) {
            return Some(vec![(*fa).clone()]);
        }
        let total = candidates
            .iter()
            .fold(Decimal::ZERO, |s, a| &s + &a.quantity);
        (!candidates.is_empty() && &total == qty).then(|| candidates.into_iter().cloned().collect())
    };
    let try_split = |p: &Posting| -> Option<Vec<Posting>> {
        if p.has_tag(FEESPLIT_POSTING_TAG) || !is_asset(&p.account) || opted_out(p) {
            return None;
        }
        let [a] = p.amount.amounts_raw() else {
            return None;
        };
        if !(a.is_negative()
            && (lk.is_lotful(&a.commodity) || a.cost_basis.is_some())
            && a.cost.is_none())
        {
            return None;
        }
        let from_qty = -&a.quantity;
        let to_qty = ps
            .iter()
            .filter(|q| is_asset(&q.account) && !opted_out(q))
            .flat_map(|q| q.amount.amounts_raw())
            .filter(|b| {
                b.commodity == a.commodity && b.quantity > Decimal::ZERO && b.cost.is_none()
            })
            .fold(Decimal::ZERO, |s, b| &s + &b.quantity);
        let fee_qty = &from_qty - &to_qty;
        if !(to_qty > Decimal::ZERO && fee_qty > Decimal::ZERO) {
            return None;
        }
        let fees = fee_counterparts(&a.commodity, &fee_qty)?;
        let orig = Box::new(p.original().clone());
        let mut out: Vec<Posting> = fees
            .iter()
            .map(|fa| {
                let mut q = Posting {
                    amount: MixedAmount::from_amount(Amount {
                        cost: fa.cost.clone(),
                        ..a.clone().with_quantity(-&fa.quantity)
                    }),
                    original: Some(orig.clone()),
                    assertion: None,
                    ..p.clone()
                };
                q.add_hidden_and_maybe_visible_tag(
                    false,
                    verbose,
                    hidden_tag(GENERATED_POSTING_TAG, ""),
                );
                q.add_hidden_and_maybe_visible_tag(
                    false,
                    verbose,
                    hidden_tag(FEESPLIT_POSTING_TAG, ""),
                );
                q
            })
            .collect();
        out.push(Posting {
            amount: MixedAmount::from_amount(a.clone().with_quantity(-&to_qty)),
            original: Some(orig),
            ..p.clone()
        });
        Some(out)
    };
    let split: Vec<Option<Vec<Posting>>> = ps.iter().map(try_split).collect();
    if split.iter().all(Option::is_none) {
        return t;
    }
    let old = std::mem::take(&mut t.postings);
    t.postings = old
        .into_iter()
        .zip(split)
        .flat_map(|(p, s)| s.unwrap_or_else(|| vec![p]))
        .collect();
    t
}

/// Classify a transaction's lot postings, adding a `_ptype` tag (visible
/// too with `verbose`): acquire or dispose for postings with a cost basis
/// (by sign), or transfer-from or transfer-to where a counterpart in another
/// account matches; lotful asset postings without a cost basis by the same
/// rules and some heuristics; short positions in liability accounts as
/// acquire or dispose (transactionClassifyLotPostings).
pub fn transaction_classify_lot_postings(
    verbose: bool,
    lk: &LotLookups,
    mut t: Transaction,
) -> Transaction {
    let classes: Vec<Option<&'static str>> = {
        let c = Classifier::new(lk, &t.postings);
        if !t.postings.iter().any(|p| c.has_lot_relevant_amount(p)) {
            return t;
        }
        (0..t.postings.len()).map(|i| c.classify_at(i)).collect()
    };
    for (p, cls) in t.postings.iter_mut().zip(classes) {
        if let Some(cls) = cls {
            p.add_hidden_and_maybe_visible_tag(true, verbose, hidden_tag("_ptype", cls));
        }
    }
    t
}

/// A transaction's postings, and what classification works out from all of
/// them before classifying each.
struct Classifier<'a> {
    lk: &'a LotLookups<'a>,
    ps: &'a [Posting],
    /// Same-account transfer pairs: matching negative and positive
    /// unpriced amounts in one account.
    same_account_pairs: BTreeSet<usize>,
    /// The accounts with negative cost basis amounts (or bare lotful ones
    /// in asset accounts), positive cost basis amounts, and positive asset
    /// amounts without cost basis, by commodity and absolute quantity.
    neg_cb: BTreeMap<(Sym, Decimal), Vec<AccountName>>,
    pos_cb: BTreeMap<(Sym, Decimal), Vec<AccountName>>,
    pos_no_cb: BTreeMap<(Sym, Decimal), Vec<AccountName>>,
    /// The total unpriced outflows and inflows of each commodity, with
    /// their accounts.
    neg_sums: BTreeMap<Sym, (Decimal, Vec<AccountName>)>,
    pos_sums: BTreeMap<Sym, (Decimal, Vec<AccountName>)>,
    /// Has the transaction an equity posting with no cost basis of its own?
    equity_counterpart: bool,
}

/// Add an account to a key's list, once.
fn add_account<K: Ord>(m: &mut BTreeMap<K, Vec<AccountName>>, k: K, acct: &AccountName) {
    let accts = m.entry(k).or_default();
    if !accts.contains(acct) {
        accts.insert(0, acct.clone());
    }
}

/// Add a quantity, and an account once, to a commodity's sum.
fn add_to_sum(
    m: &mut BTreeMap<Sym, (Decimal, Vec<AccountName>)>,
    c: &Sym,
    q: Decimal,
    acct: &AccountName,
) {
    match m.get_mut(c) {
        Some((total, accts)) => {
            *total = &q + total;
            if !accts.contains(acct) {
                accts.insert(0, acct.clone());
            }
        }
        None => {
            m.insert(*c, (q, vec![acct.clone()]));
        }
    }
}

impl<'a> Classifier<'a> {
    fn new(lk: &'a LotLookups<'a>, ps: &'a [Posting]) -> Classifier<'a> {
        let mut c = Classifier {
            lk,
            ps,
            same_account_pairs: BTreeSet::new(),
            neg_cb: BTreeMap::new(),
            pos_cb: BTreeMap::new(),
            pos_no_cb: BTreeMap::new(),
            neg_sums: BTreeMap::new(),
            pos_sums: BTreeMap::new(),
            equity_counterpart: false,
        };
        c.equity_counterpart = ps.iter().any(|q| {
            lk.is_equity(lot_base_account(&q.account))
                && (has_balancer_copied_basis(q) || !has_cost_basis(q))
        });
        c.same_account_pairs = c.same_account_pairs();
        c.collect_counterparts();
        c.collect_sums();
        c
    }

    fn opted_out(&self, p: &Posting) -> bool {
        self.lk.uses_no_lots(lot_base_account(&p.account)) && !has_cost_basis(p)
    }

    fn is_short(&self, p: &Posting) -> bool {
        self.lk.direction(&p.account) == Some(LotDirection::Short)
    }

    fn lotful(&self, amts: &[Amount]) -> bool {
        amts.iter().any(|a| self.lk.is_lotful(&a.commodity))
    }

    fn has_lot_relevant_amount(&self, p: &Posting) -> bool {
        let amts = p.amount.amounts_raw();
        let bare = !has_cost_basis(p) && self.lotful(amts);
        p.is_real()
            && !has_balancer_copied_basis(p)
            && !self.opted_out(p)
            && (has_cost_basis(p)
                || (bare && any_negative(amts))
                || (bare && !any_negative(amts) && amts.iter().any(|a| a.quantity > Decimal::ZERO)))
    }

    /// Could this balancer-copied-basis posting be the elided destination
    /// or source of a lot transfer (isMirroredTransferCandidate)?
    fn is_mirrored_transfer_candidate(&self, p: &Posting) -> bool {
        let amts = p.amount.amounts_raw();
        let base = lot_base_account(&p.account);
        self.lk.is_asset(base)
            && !self.lk.uses_no_lots(base)
            && amts.iter().any(|a| !a.quantity.is_zero())
            && !any_cost(amts)
    }

    fn same_account_pairs(&self) -> BTreeSet<usize> {
        type Pairs = (Vec<usize>, Vec<usize>);
        let mut grouped: BTreeMap<(&str, Sym, Decimal), Pairs> = BTreeMap::new();
        for (i, p) in self.ps.iter().enumerate() {
            if !p.is_real() || !self.has_lot_relevant_amount(p) || self.is_short(p) {
                continue;
            }
            let acct = lot_base_account(&p.account);
            for a in p.amount.amounts_raw().iter().filter(|a| a.cost.is_none()) {
                let q = &a.quantity;
                if q.is_negative() {
                    let e = grouped.entry((acct, a.commodity, -q)).or_default();
                    e.0.insert(0, i);
                } else if *q > Decimal::ZERO {
                    let e = grouped.entry((acct, a.commodity, q.clone())).or_default();
                    e.1.insert(0, i);
                }
            }
        }
        grouped
            .values()
            .flat_map(|(negs, poss)| {
                let n = negs.len().min(poss.len());
                negs[..n].iter().chain(&poss[..n]).copied()
            })
            .collect()
    }

    fn collect_counterparts(&mut self) {
        let (mut neg, mut pos, mut no_cb) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        for (i, p) in self.ps.iter().enumerate() {
            if !p.is_real()
                || (has_balancer_copied_basis(p) && !self.is_mirrored_transfer_candidate(p))
                || self.same_account_pairs.contains(&i)
                || self.opted_out(p)
                || self.is_short(p)
            {
                continue;
            }
            let acct = AccountName::from(lot_base_account(&p.account));
            let is_asset = self.lk.is_asset(&acct);
            let amts = p.amount.amounts_raw();
            let is_neg = any_negative(amts);
            let has_cb = has_cost_basis(p);
            let key = |a: &Amount| (a.commodity, a.quantity.abs());
            let cb_keys = amts.iter().filter(|a| a.cost_basis.is_some()).map(key);
            if is_neg && (has_cb || (self.lotful(amts) && is_asset)) {
                let keys: Vec<_> = if has_cb {
                    cb_keys.collect()
                } else {
                    amts.iter().map(key).collect()
                };
                for k in keys {
                    add_account(&mut neg, k, &acct);
                }
            } else if !is_neg && has_cb {
                for k in cb_keys {
                    add_account(&mut pos, k, &acct);
                }
            }
            if !is_neg && is_asset && !has_cb {
                for k in amts.iter().map(key) {
                    add_account(&mut no_cb, k, &acct);
                }
            }
        }
        (self.neg_cb, self.pos_cb, self.pos_no_cb) = (neg, pos, no_cb);
    }

    fn collect_sums(&mut self) {
        let (mut neg, mut pos) = (BTreeMap::new(), BTreeMap::new());
        for (i, p) in self.ps.iter().enumerate() {
            if !p.is_real()
                || has_balancer_copied_basis(p)
                || self.same_account_pairs.contains(&i)
                || p.has_tag(FEESPLIT_POSTING_TAG)
                || self.opted_out(p)
                || self.is_short(p)
            {
                continue;
            }
            let acct = AccountName::from(lot_base_account(&p.account));
            let is_asset = self.lk.is_asset(&acct);
            let amts = p.amount.amounts_raw();
            let is_lotful = self.lotful(amts);
            for a in amts.iter().filter(|a| a.cost.is_none()) {
                let has_cb = a.cost_basis.is_some();
                if a.is_negative() && (has_cb || (is_lotful && is_asset)) {
                    add_to_sum(&mut neg, &a.commodity, -&a.quantity, &acct);
                } else if a.quantity > Decimal::ZERO && (has_cb || is_asset) {
                    add_to_sum(&mut pos, &a.commodity, a.quantity.clone(), &acct);
                }
            }
        }
        (self.neg_sums, self.pos_sums) = (neg, pos);
    }

    fn any_other_account(
        m: &BTreeMap<(Sym, Decimal), Vec<AccountName>>,
        acct: &str,
        c: &Sym,
        q: &Decimal,
    ) -> bool {
        m.get(&(*c, q.abs()))
            .is_some_and(|accts| accts.iter().any(|a| &**a != acct))
    }

    /// Is there a transfer counterpart, in another account, for an amount
    /// of this sign, commodity and quantity (hasCounterpart)?
    fn has_counterpart(&self, acct: &str, is_neg: bool, c: &Sym, q: &Decimal) -> bool {
        if is_neg {
            Self::any_other_account(&self.pos_cb, acct, c, q)
                || Self::any_other_account(&self.pos_no_cb, acct, c, q)
        } else {
            Self::any_other_account(&self.neg_cb, acct, c, q)
        }
    }

    fn has_transfer_from_counterpart(&self, acct: &str, c: &Sym, q: &Decimal) -> bool {
        Self::any_other_account(&self.neg_cb, acct, c, q)
    }

    /// Is there a transfer source of this commodity, in any quantity, in
    /// another account (hasTransferFromCommodityMatch)?
    fn has_transfer_from_commodity_match(&self, acct: &str, c: &Sym) -> bool {
        self.neg_cb
            .iter()
            .any(|((c2, _), accts)| c2 == c && accts.iter().any(|a| &**a != acct))
    }

    /// Do the commodity's unpriced outflows and inflows add up to the same,
    /// with another account on the other side (hasSumCounterpart)?
    fn has_sum_counterpart(&self, acct: &str, is_neg: bool, c: &Sym) -> bool {
        match (self.neg_sums.get(c), self.pos_sums.get(c)) {
            (Some((nq, naccts)), Some((pq, paccts))) => {
                nq == pq
                    && (if is_neg { paccts } else { naccts })
                        .iter()
                        .any(|a| &**a != acct)
            }
            _ => false,
        }
    }

    fn classify_at(&self, i: usize) -> Option<&'static str> {
        let p = &self.ps[i];
        if !p.is_real() || is_classified_posting(p) {
            return None;
        }
        let amts = p.amount.amounts_raw();
        let base = lot_base_account(&p.account);
        let is_neg = any_negative(amts);
        let direction = if is_neg {
            "transfer-from"
        } else {
            "transfer-to"
        };
        if has_balancer_copied_basis(p) {
            let mut cb_pairs = amts.iter().filter(|a| a.cost_basis.is_some());
            let has_match = if is_neg {
                cb_pairs.any(|a| self.has_counterpart(base, true, &a.commodity, &a.quantity))
            } else {
                cb_pairs
                    .any(|a| self.has_transfer_from_counterpart(base, &a.commodity, &a.quantity))
            };
            return (self.is_mirrored_transfer_candidate(p) && has_match).then_some(direction);
        }
        if self.same_account_pairs.contains(&i) {
            return Some(direction);
        }
        self.should_classify(p)
    }

    fn should_classify(&self, p: &Posting) -> Option<&'static str> {
        let amts = p.amount.amounts_raw();
        if amts.iter().all(|a| a.quantity.is_zero()) || self.opted_out(p) {
            return None;
        }
        if self.is_short(p) {
            return self.classify_short(p, amts);
        }
        if amts.iter().any(|a| a.cost_basis.is_some()) {
            return self.classify_with_cost_basis(p, amts);
        }
        if !self.lk.is_asset(lot_base_account(&p.account)) {
            return None;
        }
        self.classify_negative_lotful(p, amts)
            .or_else(|| self.classify_lotful(p, amts, true))
            .or_else(|| self.classify_lotful(p, amts, false))
            .or_else(|| self.classify_positive_lotful(p, amts))
    }

    fn classify_with_cost_basis(&self, p: &Posting, amts: &[Amount]) -> Option<&'static str> {
        let base = lot_base_account(&p.account);
        let is_neg = any_negative(amts);
        let mut cb_amts = amts.iter().filter(|a| a.cost_basis.is_some());
        let unpriced = !any_cost(amts);
        let is_transfer = cb_amts
            .clone()
            .any(|a| self.has_counterpart(base, is_neg, &a.commodity, &a.quantity))
            || (unpriced
                && !p.has_tag(FEESPLIT_POSTING_TAG)
                && cb_amts.any(|a| self.has_sum_counterpart(base, is_neg, &a.commodity)));
        if is_transfer || (unpriced && self.equity_counterpart) {
            return Some(if is_neg {
                "transfer-from"
            } else {
                "transfer-to"
            });
        }
        if self
            .lk
            .account_type(base)
            .is_some_and(AccountType::is_income_statement)
        {
            return None;
        }
        Some(if is_neg { "dispose" } else { "acquire" })
    }

    fn classify_negative_lotful(&self, p: &Posting, amts: &[Amount]) -> Option<&'static str> {
        if !self.lotful(amts) || !any_negative(amts) {
            return None;
        }
        let base = lot_base_account(&p.account);
        let has_price = any_cost(amts);
        let negs: Vec<&Amount> = amts.iter().filter(|a| a.is_negative()).collect();
        let is_transfer = amts
            .iter()
            .any(|a| self.has_counterpart(base, true, &a.commodity, &a.quantity));
        let others = || self.ps.iter().filter(|q| *q != p);
        let other_asset_receives = others().any(|q| {
            let qamts = q.amount.amounts_raw();
            let qbase = lot_base_account(&q.account);
            qbase != base
                && self.lk.is_asset(qbase)
                && !self.opted_out(q)
                && self.lotful(qamts)
                && qamts.iter().any(|a| {
                    a.quantity > Decimal::ZERO && negs.iter().any(|n| n.commodity == a.commodity)
                })
        });
        let has_fee_counterpart = others().any(|q| {
            !self.lk.is_asset(lot_base_account(&q.account))
                && q.amount.amounts_raw().iter().any(|a| {
                    a.quantity > Decimal::ZERO
                        && negs
                            .iter()
                            .any(|n| n.commodity == a.commodity && n.quantity.abs() == a.quantity)
                })
        });
        if is_transfer || (other_asset_receives && !has_fee_counterpart && !has_price) {
            Some("transfer-from")
        } else {
            (has_price || has_fee_counterpart || !other_asset_receives).then_some("dispose")
        }
    }

    /// A positive unpriced posting with a transfer source of its commodity
    /// is a transfer destination: if lotful (shouldClassifyLotful), or any
    /// (shouldClassifyBareTransferTo).
    fn classify_lotful(
        &self,
        p: &Posting,
        amts: &[Amount],
        lotful_only: bool,
    ) -> Option<&'static str> {
        if (lotful_only && !self.lotful(amts)) || any_negative(amts) || any_cost(amts) {
            return None;
        }
        let base = lot_base_account(&p.account);
        (amts
            .iter()
            .any(|a| self.has_transfer_from_counterpart(base, &a.commodity, &a.quantity))
            || amts
                .iter()
                .any(|a| self.has_transfer_from_commodity_match(base, &a.commodity)))
        .then_some("transfer-to")
    }

    fn classify_positive_lotful(&self, p: &Posting, amts: &[Amount]) -> Option<&'static str> {
        if !self.lotful(amts) || !amts.iter().any(|a| a.quantity > Decimal::ZERO) {
            return None;
        }
        let base = lot_base_account(&p.account);
        let has_transfer_from = amts
            .iter()
            .any(|a| self.has_transfer_from_counterpart(base, &a.commodity, &a.quantity));
        (any_cost(amts) || self.has_other_commodity_posting(p, amts) || has_transfer_from)
            .then_some("acquire")
    }

    fn classify_short(&self, p: &Posting, amts: &[Amount]) -> Option<&'static str> {
        let has_cb = amts.iter().any(|a| a.cost_basis.is_some());
        let has_price = any_cost(amts);
        let is_inflow = amts.iter().any(|a| LotDirection::Short.is_inflow(a));
        if !(has_cb || self.lotful(amts)) {
            return None;
        }
        if !has_price && self.equity_counterpart {
            Some(if is_inflow {
                "transfer-to"
            } else {
                "transfer-from"
            })
        } else if is_inflow {
            (has_cb || has_price || self.has_other_commodity_posting(p, amts)).then_some("acquire")
        } else {
            Some("dispose")
        }
    }

    /// Has another posting with an amount a commodity other than these
    /// (hasOtherCommodityPosting)?
    fn has_other_commodity_posting(&self, p: &Posting, amts: &[Amount]) -> bool {
        let comms: BTreeSet<&Sym> = amts.iter().map(|a| &a.commodity).collect();
        self.ps
            .iter()
            .filter(|q| *q != p && q.has_amount())
            .any(|q| {
                q.amount
                    .amounts_raw()
                    .iter()
                    .any(|a| !comms.contains(&a.commodity))
            })
    }
}

/// Does this posting look like a lot disposal or transfer source: a lot
/// outflow with a cost basis, or in a lotful commodity outside a lots: NONE
/// account (postingHasDisposeShape)?
fn posting_has_dispose_shape(lk: &LotLookups, p: &Posting) -> bool {
    let dir = lk.direction(&p.account).unwrap_or(LotDirection::Long);
    p.amount.amounts_raw().iter().any(|a| {
        dir.is_outflow(a)
            && (a.cost_basis.is_some()
                || (lk.is_lotful(&a.commodity) && !lk.uses_no_lots(lot_base_account(&p.account))))
    })
}

/// A sum of mixed amounts nested as Haskell's foldMap nests it: each on the
/// left of the sum of those after it.
pub fn fold_map(ms: impl DoubleEndedIterator<Item = MixedAmount>) -> MixedAmount {
    ms.rev().fold(MixedAmount::null(), |acc, m| &m + &acc)
}

/// In a disposal, tag the user-written realised gain postings `_ptype:gain`
/// so that the balancer sets them aside: postings to Gain accounts (also
/// amountless ones, with `tag_amountless`), or else, heuristically,
/// amountful postings outside asset, liability and equity accounts with no
/// lot amounts, if the other postings balance without them
/// (transactionTagGainPostings).
pub fn transaction_tag_gain_postings(
    tag_amountless: bool,
    verbose: bool,
    lk: &LotLookups,
    mut t: Transaction,
) -> Transaction {
    let tag: Vec<bool> = {
        let realps: Vec<&Posting> = t.postings.iter().filter(|p| p.is_real()).collect();
        if realps.iter().any(|p| is_gain_posting(p))
            || !realps.iter().any(|p| posting_has_dispose_shape(lk, p))
        {
            return t;
        }
        let is_gain_typed = |p: &Posting| {
            (tag_amountless || p.has_amount())
                && lk.account_type(&p.account) == Some(AccountType::Gain)
        };
        let is_candidate = |p: &Posting| {
            let not_ale = lk.account_type(&p.account).is_none_or(|ty| {
                !(ty.is_subtype_of(AccountType::Asset)
                    || ty.is_subtype_of(AccountType::Liability)
                    || ty.is_subtype_of(AccountType::Equity))
            });
            p.has_amount()
                && not_ale
                && !p
                    .amount
                    .amounts_raw()
                    .iter()
                    .any(|a| a.cost_basis.is_some() || lk.is_lotful(&a.commodity))
        };
        if realps.iter().any(|p| is_gain_typed(p)) {
            t.postings
                .iter()
                .map(|p| p.is_real() && is_gain_typed(p))
                .collect()
        } else if gain_heuristic_ok(lk, &t, &realps, &is_candidate) {
            t.postings
                .iter()
                .map(|p| p.is_real() && is_candidate(p))
                .collect()
        } else {
            return t;
        }
    };
    for (p, tag) in t.postings.iter_mut().zip(tag) {
        if tag {
            p.add_hidden_and_maybe_visible_tag(true, verbose, hidden_tag("_ptype", "gain"));
        }
    }
    t
}

/// Are the heuristic gain candidates a well-formed disposal's gain: all
/// postings have amounts, and the others sum to zero at transacted cost,
/// or are an unpriced sale (a lot commodity net sold and one other net
/// received)?
fn gain_heuristic_ok(
    lk: &LotLookups,
    t: &Transaction,
    realps: &[&Posting],
    is_candidate: &dyn Fn(&Posting) -> bool,
) -> bool {
    let balancing_amount = |p: &&Posting| {
        if p.has_tag(COST_POSTING_TAG) {
            p.amount.clone().strip_costs()
        } else {
            p.amount.clone().to_cost()
        }
    };
    let (candidates, others): (Vec<&Posting>, Vec<&Posting>) =
        realps.iter().partition(|p| is_candidate(p));
    if !realps.iter().all(|p| p.has_amount()) || candidates.is_empty() {
        return false;
    }
    let residual = fold_map(others.iter().map(balancing_amount));
    if t.postings.iter().any(|p| p.has_tag(COST_POSTING_TAG)) {
        return (&residual + &fold_map(candidates.iter().map(balancing_amount))).is_zero();
    }
    if residual.is_zero() {
        return true;
    }
    let dispose_shaped: Vec<&&Posting> = realps
        .iter()
        .filter(|p| posting_has_dispose_shape(lk, p))
        .collect();
    let is_lot_commodity = |c: &Sym| {
        lk.is_lotful(c)
            || dispose_shaped
                .iter()
                .flat_map(|p| p.amount.amounts_raw())
                .any(|a| &a.commodity == c)
    };
    let nonzero: Vec<&Amount> = residual
        .amounts_raw()
        .iter()
        .filter(|a| !a.quantity.is_zero())
        .collect();
    let (closed, other): (Vec<&Amount>, Vec<&Amount>) = nonzero
        .into_iter()
        .partition(|a| is_lot_commodity(&a.commodity));
    match (closed.as_slice(), other.as_slice()) {
        ([closed], [other]) => {
            dispose_shaped.iter().any(|p| {
                lk.direction(&p.account)
                    .unwrap_or(LotDirection::Long)
                    .is_outflow(closed)
            }) && other.quantity.signum() == -closed.quantity.signum()
        }
        _ => false,
    }
}

/// Tag the user-written gain postings of disposals; amountless ones only
/// when lot errors are not ignored (journalTagGainPostings).
pub fn journal_tag_gain_postings(lenient: bool, verbose: bool, mut j: Journal) -> Journal {
    let lotful = j.lotful_commodities();
    let lots_tags = j.account_lots_tags();
    let txns = std::mem::take(&mut j.txns);
    let lk = LotLookups {
        account_types: &j.account_types,
        lotful: &lotful,
        lots_tags: &lots_tags,
    };
    let txns = txns
        .into_iter()
        .map(|t| {
            if transaction_has_lotful_amounts(&lotful, &t) {
                transaction_tag_gain_postings(!lenient, verbose, &lk, t)
            } else {
                t
            }
        })
        .collect();
    j.txns = txns;
    j
}

/// Classify the lot postings of each transaction that could involve lots,
/// after splitting off lot transfer fees (journalClassifyLotPostings).
/// Returns also the classified postings of those transactions, which the
/// lot calculation's error messages find postings in.
pub fn journal_classify_lot_postings(
    verbose: bool,
    mut j: Journal,
) -> (Journal, Vec<Option<Vec<Posting>>>) {
    let lotful = j.lotful_commodities();
    let lots_tags = j.account_lots_tags();
    let txns = std::mem::take(&mut j.txns);
    let lk = LotLookups {
        account_types: &j.account_types,
        lotful: &lotful,
        lots_tags: &lots_tags,
    };
    let mut tied = Vec::with_capacity(txns.len());
    let txns = txns
        .into_iter()
        .map(|t| {
            if !transaction_has_lotful_amounts(&lotful, &t) {
                tied.push(None);
                return t;
            }
            let t = transaction_auto_split_fee_outflows(verbose, &lk, t);
            let t = transaction_classify_lot_postings(verbose, &lk, t);
            tied.push(Some(t.postings.clone()));
            t
        })
        .collect();
    j.txns = txns;
    (j, tied)
}

/// In lot-opening postings with a cost basis cost and no transacted cost,
/// which don't look like transfer destinations, infer the transacted cost
/// from the basis (journalInferPostingsTransactedCost).
pub fn journal_infer_postings_transacted_cost(mut j: Journal) -> Journal {
    let mut txns = std::mem::take(&mut j.txns);
    for t in &mut txns {
        // only an amount with a basis cost and no transacted cost gets one
        if !t
            .postings
            .iter()
            .flat_map(|p| p.amount.amounts_raw())
            .any(|a| a.cost.is_none() && has_basis_cost(a))
        {
            continue;
        }
        let is_equity = |a: &str| {
            j.account_type(a)
                .is_some_and(|ty| ty.is_subtype_of(AccountType::Equity))
        };
        let equity_counterpart = t
            .postings
            .iter()
            .any(|q| is_equity(&q.account) && !has_cost_basis(q));
        let new: Vec<Option<MixedAmount>> = t
            .postings
            .iter()
            .map(|p| {
                let dir = lot_direction(&j, &p.account).unwrap_or(LotDirection::Long);
                let needs = |a: &Amount| {
                    has_basis_cost(a)
                        && a.cost.is_none()
                        && dir.is_inflow(a)
                        && !has_transfer_from_counterpart(t, p, a)
                        && !has_transfer_group_shape(t, p, &a.commodity)
                };
                if !p.amount.amounts_raw().iter().any(needs) || equity_counterpart {
                    return None;
                }
                Some(p.amount.map(|a| match &a.cost_basis {
                    Some(cb) if needs(a) => Amount {
                        cost: cb.cost.clone().map(AmountCost::Unit),
                        ..a.clone()
                    },
                    _ => a.clone(),
                }))
            })
            .collect();
        for (p, m) in t.postings.iter_mut().zip(new) {
            if let Some(m) = m {
                p.keep_original();
                p.amount = m;
            }
        }
    }
    j.txns = txns;
    j
}

/// Has this amount a cost basis with a cost?
fn has_basis_cost(a: &Amount) -> bool {
    a.cost_basis.as_ref().is_some_and(|cb| cb.cost.is_some())
}

/// Has another posting an explicit negative amount of this commodity and
/// quantity, in another account, or unpriced in the same one?
fn has_transfer_from_counterpart(t: &Transaction, p: &Posting, a: &Amount) -> bool {
    let neg = -&a.quantity;
    t.postings.iter().any(|q| {
        q.amount.amounts_raw().iter().any(|qa| {
            qa.commodity == a.commodity
                && qa.quantity == neg
                && (q.account != p.account || qa.cost.is_none())
        })
    })
}

/// Do the transaction's unpriced amounts in this commodity look like a
/// split or consolidating transfer: equal nonzero outflow and inflow
/// totals, with an outflow in another account?
fn has_transfer_group_shape(t: &Transaction, p: &Posting, c: &Sym) -> bool {
    let unpriced = t.postings.iter().flat_map(|q| {
        q.amount
            .amounts_raw()
            .iter()
            .filter(|a| &a.commodity == c && a.cost.is_none())
            .map(move |a| (&q.account, a))
    });
    let mut neg_total = Decimal::ZERO;
    let mut pos_total = Decimal::ZERO;
    let mut other_account = false;
    for (acct, a) in unpriced {
        if a.is_negative() {
            neg_total = &neg_total + &-&a.quantity;
            other_account |= *acct != p.account;
        } else if a.quantity > Decimal::ZERO {
            pos_total = &pos_total + &a.quantity;
        }
    }
    neg_total > Decimal::ZERO && neg_total == pos_total && other_account
}

/// Check that no acquire-shaped posting has a per-unit cost basis and a
/// transacted cost which disagree; if they agree only at the written
/// basis's precision, the exact transacted cost becomes the basis
/// (journalCheckAcquireBasis).
pub fn journal_check_acquire_basis(mut j: Journal) -> Result<Journal, String> {
    let mut txns = std::mem::take(&mut j.txns);
    for t in &mut txns {
        for idx in 0..t.postings.len() {
            let p = &t.postings[idx];
            // only an amount with a basis cost and a transacted cost is checked
            if !p
                .amount
                .amounts_raw()
                .iter()
                .any(|a| a.cost.is_some() && has_basis_cost(a))
            {
                continue;
            }
            let Some(dir) = lot_direction(&j, &p.account).filter(|_| p.is_real()) else {
                continue;
            };
            let mut changed = false;
            let mut amts = p.amount.amounts_raw().to_vec();
            for a in &mut amts {
                let (Some(cb), Some(tc)) = (&a.cost_basis, &a.cost) else {
                    continue;
                };
                let Some(basis) = &cb.cost else {
                    continue;
                };
                if !dir.is_inflow(a) {
                    continue;
                }
                let transacted = amount_cost_to_unit_cost(&a.quantity, tc);
                if basis.quantity == transacted.quantity {
                    continue;
                }
                let written_places = basis.quantity.normalize().places();
                if transacted.quantity.round_to(written_places) != basis.quantity {
                    return Err(acquire_basis_err(t, idx, basis, &transacted));
                }
                let mut cb = (**cb).clone();
                cb.cost = Some(Box::new(transacted));
                a.cost_basis = Some(Box::new(cb));
                changed = true;
            }
            if changed {
                t.postings[idx].amount = MixedAmount::from_amounts(amts);
            }
        }
    }
    j.txns = txns;
    Ok(j)
}

fn acquire_basis_err(t: &Transaction, idx: usize, basis: &Amount, transacted: &Amount) -> String {
    let p = &t.postings[idx];
    let col1 = 5 + usize::from(p.is_virtual());
    let col2 = col1 + p.original().account.chars().count() - 1;
    let (f, l, ex) = posting_error_excerpt_by_index(
        &super::transaction_as_written(t),
        super::as_written_posting_index(t, idx),
        Some((col1, Some(col2))),
    );
    let fmt = AmountFormat {
        zero_commodity: true,
        ..AmountFormat::ONE_LINE_NO_COST
    };
    let (b, tr) = show_amounts_distinctly(&fmt, basis, transacted);
    format!(
        "{f}:{l}:\n{ex}\nThis acquire posting's cost basis ({b}) differs from its transacted cost ({tr}).\n\
         Options:\n  \
         - drop {{}} or {{{{}}}} so basis is inferred from the transacted cost\n  \
         - drop @ or @@ so transacted cost is inferred from the basis\n  \
         - use {{{{TotalCost}}}} or write the per-unit basis at higher precision\n  \
         - if the difference is real (gift, NSO, RSU, etc.), fund it via a separate posting"
    )
}
