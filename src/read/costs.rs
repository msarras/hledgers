//! Costs and equity conversion postings (the cost and conversion parts of
//! Hledger.Data.Transaction, Hledger.Data.Posting and Hledger.Data.Journal):
//! tagging a costful posting and the pair of equity conversion postings
//! that record the same conversion, inferring costs from conversion
//! postings (`--infer-costs`), inferring conversion postings from costs
//! (`--infer-equity`), and market prices from costs.

use crate::account::AccountName;
use crate::amount::render::AmountFormat;
use crate::amount::{Amount, AmountCost, MixedAmount, Precision};
use crate::model::{
    CONVERSION_POSTING_TAG, COST_POSTING_TAG, GENERATED_POSTING_TAG, IMBALANCE_POSTING_TAG,
    Journal, LOTSPLIT_POSTING_TAG, MarketPrice, Posting, Transaction,
};
use crate::write::journal::{PostingLayout, postings_as_lines};

/// Tag equity conversion postings and the postings whose costs they
/// duplicate, and with `add_costs`, add the costs that conversion postings
/// imply, in every transaction (journalTagCostsAndEquityAndMaybeInferCosts).
pub fn journal_tag_costs_and_equity_and_maybe_infer_costs(
    verbose_tags: bool,
    add_costs: bool,
    mut j: Journal,
) -> Result<Journal, String> {
    let accts = j.conversion_accounts();
    if accts.is_empty() {
        return Ok(j);
    }
    let txns = std::mem::take(&mut j.txns);
    j.txns = txns
        .into_iter()
        .map(|t| {
            transaction_tag_costs_and_equity_and_maybe_infer_costs(
                verbose_tags,
                add_costs,
                &accts,
                t,
            )
        })
        .collect::<Result<_, _>>()?;
    Ok(j)
}

/// A posting by its position in its transaction.
type IdxPosting = (usize, Posting);

/// For each pair of adjacent conversion postings in this transaction, find
/// the posting with an equivalent cost, or failing that, a costless posting
/// matching one of the conversion amounts, and tag it and the pair; with
/// `add_costs`, give the costless posting the cost the pair implies
/// (transactionTagCostsAndEquityAndMaybeInferCosts). Pairs are taken last
/// first, and each matched posting is used once, as upstream does.
pub fn transaction_tag_costs_and_equity_and_maybe_infer_costs(
    verbose_tags: bool,
    add_costs: bool,
    conversion_accts: &[AccountName],
    t: Transaction,
) -> Result<Transaction, String> {
    if !t
        .postings
        .iter()
        .any(|p| conversion_accts.contains(&p.account))
    {
        return Ok(t);
    }
    let numbered: Vec<IdxPosting> = t.postings.iter().cloned().enumerate().collect();
    let (pairs, (mut costful, mut costless)) =
        partition_and_check_conversion_postings(false, conversion_accts, &numbered)
            .expect("unchecked partitioning succeeds");
    let mut changes: Vec<Change> = Vec::new();
    for pair in pairs {
        let change =
            tag_and_maybe_add_costs(verbose_tags, add_costs, pair, &mut costful, &mut costless)
                .map_err(|e| super::errors::annotate_error_with_transaction(&t, &e))?;
        changes.extend(change);
    }
    let postings = numbered
        .into_iter()
        .map(|(n, p)| changes.iter().rev().fold(p, |p, c| c.apply(n, p)))
        .collect();
    Ok(Transaction { postings, ..t })
}

/// How one pair of conversion postings changes the transaction: the matched
/// posting replaced by a cost-tagged version, the pair conversion-tagged
/// (postingAddCostAndOrTag).
struct Change {
    verbose_tags: bool,
    cost_index: usize,
    cost_posting: Posting,
    pair: (usize, usize),
}

impl Change {
    fn apply(&self, n: usize, p: Posting) -> Posting {
        let tag = |mut p: Posting, name: &str| {
            p.add_hidden_and_maybe_visible_tag(
                false,
                self.verbose_tags,
                (name.to_string(), String::new()),
            );
            p
        };
        if n == self.cost_index {
            tag(self.cost_posting.clone(), COST_POSTING_TAG)
        } else if n == self.pair.0 || n == self.pair.1 {
            tag(p, CONVERSION_POSTING_TAG)
        } else {
            p
        }
    }
}

/// The posting's amount, if it has exactly one (postingSingleAmount).
fn posting_single_amount(p: &Posting) -> Option<&Amount> {
    match p.amount.amounts_raw() {
        [a] => Some(a),
        _ => None,
    }
}

/// Whether two amounts look the same at the first's display precision
/// (amountsMatch).
fn amounts_match(a: &Amount, b: &Amount) -> bool {
    (a - b).with_precision(a.style.precision).looks_zero()
}

/// Match a pair of conversion postings with the remaining costful or
/// costless postings (tagAndMaybeAddCostsForEquityPostings): one costful
/// posting whose amount and cost match the pair; or if none, the first
/// costless posting matching one of the pair's amounts, provided no later
/// one has the same amount. With `add_costs`, the matched posting is used
/// up, and a costless one gets the cost.
fn tag_and_maybe_add_costs(
    verbose_tags: bool,
    add_costs: bool,
    ((n1, cp1), (n2, cp2)): (IdxPosting, IdxPosting),
    costful: &mut Vec<IdxPosting>,
    costless: &mut Vec<IdxPosting>,
) -> Result<Option<Change>, String> {
    let ca1 = conversion_posting_amount_no_cost(&cp1)?;
    let ca2 = conversion_posting_amount_no_cost(&cp2)?;
    let matching_costful: Vec<&IdxPosting> = costful
        .iter()
        .filter(|(_, p)| costful_posting_matches_both_amounts(&ca1, &ca2, p))
        .collect();
    let change = |n: usize, cost_posting: Posting| Change {
        verbose_tags,
        cost_index: n,
        cost_posting,
        pair: (n1, n2),
    };
    if let [(n, p)] = matching_costful.as_slice() {
        let c = change(*n, p.clone());
        if add_costs {
            let n = *n;
            costful.retain(|(m, _)| *m != n);
        }
        return Ok(Some(c));
    }
    if !matching_costful.is_empty() {
        return Ok(None);
    }
    // the costless postings, with the cost added if adding costs, and the amount compared
    let matching_costless: Vec<(usize, Posting, Option<Amount>)> = if add_costs {
        costless
            .iter()
            .filter_map(|(n, p)| {
                add_cost_if_matches_one_amount(&ca1, &ca2, p).map(|(q, a)| (*n, q, Some(a)))
            })
            .collect()
    } else {
        costless
            .iter()
            .map(|(n, p)| (*n, p.clone(), posting_single_amount(p).cloned()))
            .collect()
    };
    let Some(((n, p, amt), others)) = matching_costless.split_first() else {
        return Ok(None);
    };
    let unique = |amt: &Amount| {
        !others
            .iter()
            .any(|(_, _, b)| b.as_ref().is_some_and(|b| amounts_match(amt, b)))
    };
    if !amt.as_ref().is_some_and(unique) {
        return Ok(None);
    }
    let c = change(*n, p.clone());
    if add_costs {
        let n = *n;
        costless.retain(|(m, _)| *m != n);
    }
    Ok(Some(c))
}

/// A conversion posting's single costless amount, or the error
/// (conversionPostingAmountNoCost).
fn conversion_posting_amount_no_cost(p: &Posting) -> Result<Amount, String> {
    let annotated = |msg: &str| {
        let mut s = format!("{msg}\n");
        for l in postings_as_lines(
            &AmountFormat::DEFAULT,
            false,
            PostingLayout::default(),
            std::slice::from_ref(p),
        ) {
            s.push_str(&l);
            s.push('\n');
        }
        s
    };
    match posting_single_amount(p) {
        Some(a) if a.cost.is_none() => Ok(a.clone()),
        Some(_) => Err(annotated("Conversion postings must not have a cost:")),
        None => Err(annotated(
            "Conversion postings must have a single-commodity amount:",
        )),
    }
}

/// Whether a costful posting's amount balances one conversion amount and
/// its cost (or cost basis, or an unspecified cost basis in the right
/// commodity) matches the other (costfulPostingIfMatchesBothAmounts).
fn costful_posting_matches_both_amounts(a1: &Amount, a2: &Amount, p: &Posting) -> bool {
    let Some(a) = posting_single_amount(p).filter(|a| a.cost.is_some()) else {
        return false;
    };
    let cost_matches = |c: &Amount| {
        amounts_match(c, &a.to_cost())
            || amounts_match(c, &a.to_cost_basis())
            || (a.cost_basis.as_ref().is_some_and(|b| b.cost.is_none())
                && c.commodity == a.to_cost().commodity)
    };
    (amounts_match(&-a1, a) && cost_matches(a2)) || (amounts_match(&-a2, a) && cost_matches(a1))
}

/// A costless posting given the other conversion amount as its total cost,
/// if its amount balances one of them (addCostIfMatchesOneAmount).
fn add_cost_if_matches_one_amount(
    a1: &Amount,
    a2: &Amount,
    p: &Posting,
) -> Option<(Posting, Amount)> {
    let a = posting_single_amount(p)?;
    let cost = if amounts_match(&-a1, a) {
        a2
    } else if amounts_match(&-a2, a) {
        a1
    } else {
        return None;
    };
    let priced = Posting {
        amount: MixedAmount::from_amount(Amount {
            cost: Some(AmountCost::Total(Box::new(cost.clone()))),
            ..a.clone()
        }),
        ..p.clone()
    };
    Some((priced, cost.clone()))
}

/// The pairs of adjacent conversion postings (last pair first), and the
/// other postings, costful (last first) and costless (in order); a
/// conversion posting without a neighbour is dropped, and the posting after
/// it counted as costless. With `check`, that is an error instead
/// (partitionAndCheckConversionPostings).
#[allow(clippy::type_complexity)]
pub fn partition_and_check_conversion_postings(
    check: bool,
    conversion_accts: &[AccountName],
    ps: &[IdxPosting],
) -> Result<
    (
        Vec<(IdxPosting, IdxPosting)>,
        (Vec<IdxPosting>, Vec<IdxPosting>),
    ),
    String,
> {
    let (mut pairs, mut costful, mut costless) = (Vec::new(), Vec::new(), Vec::new());
    let mut pending: Option<&IdxPosting> = None;
    for np in ps {
        let p = &np.1;
        let is_conversion = conversion_accts.contains(&p.account);
        match pending.take() {
            None if is_conversion => pending = Some(np),
            None if posting_single_amount(p).is_some_and(|a| a.cost.is_some()) => {
                costful.push(np.clone())
            }
            None => costless.push(np.clone()),
            Some(first) if is_conversion => pairs.push((first.clone(), np.clone())),
            Some(_) if check => {
                return Err("Conversion postings must occur in adjacent pairs".to_string());
            }
            Some(_) => costless.push(np.clone()),
        }
    }
    pairs.reverse();
    costful.reverse();
    Ok((pairs, (costful, costless)))
}

/// Add equity conversion postings for the costs that have none, after the
/// postings with them (or after the last of a posting's consecutive lot
/// fragments), rounding the generated amounts to the transaction's
/// precisions unless it has an imbalance posting
/// (transactionInferEquityPostings).
pub fn transaction_infer_equity_postings(
    verbose_tags: bool,
    equity_acct: &str,
    mut t: Transaction,
) -> Transaction {
    let has_imbalance_posting = t.postings.iter().any(|p| p.has_tag(IMBALANCE_POSTING_TAG));
    let styles = t.commodity_styles();
    let old = std::mem::take(&mut t.postings);
    let mut i = 0;
    while i < old.len() {
        let p = &old[i];
        let mut end = i + 1;
        if p.has_tag(LOTSPLIT_POSTING_TAG) {
            while end < old.len()
                && old[end].has_tag(LOTSPLIT_POSTING_TAG)
                && old[end].original == p.original
            {
                end += 1;
            }
        }
        t.postings.extend(postings_add_inferred_equity_postings(
            verbose_tags,
            equity_acct,
            &old[i..end],
        ));
        i = end;
    }
    if !has_imbalance_posting {
        for p in &mut t.postings {
            if p.has_tag(GENERATED_POSTING_TAG) {
                p.amount = p.amount.map(|a| match styles.get(&a.commodity) {
                    Some(s) if matches!(s.precision, Precision::Fixed(_)) => {
                        let Precision::Fixed(places) = s.precision else {
                            unreachable!()
                        };
                        Amount {
                            quantity: a.quantity.round_to(places),
                            style: s.clone(),
                            ..a.clone()
                        }
                    }
                    _ => a.clone(),
                });
            }
        }
    }
    t
}

/// A posting, or one posting's consecutive lot fragments, with its costs
/// tagged and a pair of equity conversion postings added for each pair of
/// commodity and cost commodity, at cost basis where known; postings
/// already tagged as cost postings are left alone
/// (postingsAddInferredEquityPostings).
fn postings_add_inferred_equity_postings(
    verbose_tags: bool,
    equity_acct: &str,
    ps: &[Posting],
) -> Vec<Posting> {
    let costful_amounts = |q: &Posting| -> Vec<Amount> {
        if q.has_tag(COST_POSTING_TAG) {
            Vec::new()
        } else {
            q.amount
                .amounts_raw()
                .iter()
                .filter(|a| a.cost.is_some())
                .cloned()
                .collect()
        }
    };
    let costful: Vec<Amount> = ps.iter().flat_map(costful_amounts).collect();
    if costful.is_empty() {
        return ps.to_vec();
    }
    let tag = |mut q: Posting, name: &str| {
        q.add_hidden_and_maybe_visible_tag(false, verbose_tags, (name.to_string(), String::new()));
        q
    };
    let mut out: Vec<Posting> = ps
        .iter()
        .map(|q| {
            if costful_amounts(q).is_empty() {
                q.clone()
            } else {
                tag(q.clone(), COST_POSTING_TAG)
            }
        })
        .collect();
    // a commodity symbol with runs of spaces made single spaces
    let words = |s: &str| {
        s.split(crate::text::is_space)
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let pair_of = |a: &Amount| (words(&a.commodity), words(&a.to_cost().commodity));
    let mut pairs: Vec<(String, String)> = Vec::new();
    for a in &costful {
        let pair = pair_of(a);
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    let first = &ps[0];
    let conversion_posting = || {
        let q = Posting {
            date: first.date,
            date2: first.date2,
            status: first.status,
            ..Posting::default()
        };
        tag(tag(q, CONVERSION_POSTING_TAG), GENERATED_POSTING_TAG)
    };
    for (amt_comm, cost_comm) in pairs {
        let these: Vec<&Amount> = costful
            .iter()
            .filter(|a| pair_of(a) == (amt_comm.clone(), cost_comm.clone()))
            .collect();
        let mut sorted = [amt_comm.as_str(), cost_comm.as_str()];
        sorted.sort();
        let prefix = format!("{equity_acct}:{}:", sorted.join("-"));
        let amount: MixedAmount = these
            .iter()
            .map(|a| {
                MixedAmount::from_amount(Amount {
                    cost: None,
                    cost_basis: None,
                    ..(*a).clone()
                })
            })
            .sum();
        out.push(Posting {
            account: format!("{prefix}{amt_comm}").into(),
            amount: -amount,
            ..conversion_posting()
        });
        out.push(Posting {
            account: format!("{prefix}{cost_comm}").into(),
            amount: these
                .iter()
                .map(|a| MixedAmount::from_amount(a.to_cost_basis()))
                .sum(),
            ..conversion_posting()
        });
    }
    out
}

/// Add equity conversion postings for costs, to the journal's conversion
/// account (journalInferEquityFromCosts).
pub fn journal_infer_equity_from_costs(verbose_tags: bool, mut j: Journal) -> Journal {
    let acct = j
        .account_for_type(
            crate::account::DEFAULT_BASE_CONVERSION_ACCOUNT,
            crate::account::AccountType::Conversion,
        )
        .to_string();
    let txns = std::mem::take(&mut j.txns);
    j.txns = txns
        .into_iter()
        .map(|t| transaction_infer_equity_postings(verbose_tags, &acct, t))
        .collect();
    j
}

/// The market prices implied by the costs in the journal's postings
/// (journalInferMarketPricesFromTransactions).
pub fn journal_infer_market_prices_from_transactions(mut j: Journal) -> Journal {
    // each transaction's prices, found on all cores, in order
    let prices = crate::par::map(&j.txns, |t| {
        (t.postings.iter())
            .flat_map(|p| {
                let date = p.date_in(Some(t));
                p.amount.amounts_raw().iter().filter_map(move |a| {
                    crate::valuation::amount_price_directive_from_cost(date, a)
                        .map(|pd| crate::valuation::price_directive_to_market_price(&pd))
                })
            })
            .collect::<Vec<MarketPrice>>()
    });
    j.inferred_market_prices = prices.concat();
    j
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::{eur, usd};

    fn d(s: &str) -> crate::decimal::Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn partitioning_conversion_postings() {
        let conv: Vec<AccountName> = vec!["c".into()];
        let ps: Vec<IdxPosting> = [
            Posting::new("a", eur(d("1")).at(usd(d("2")))),
            Posting::new("c", eur(d("-1"))),
            Posting::new("c", usd(d("2"))),
            Posting::new("b", eur(d("1")).at(usd(d("2")))),
            Posting::new("c", eur(d("-1"))),
            Posting::new("x", usd(d("1")).at(eur(d("1")))),
            Posting::new("y", usd(d("-4"))),
        ]
        .into_iter()
        .enumerate()
        .collect();
        let (pairs, (costful, costless)) =
            partition_and_check_conversion_postings(false, &conv, &ps).unwrap();
        let idx = |v: &[IdxPosting]| v.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        assert_eq!(
            pairs.iter().map(|(a, b)| (a.0, b.0)).collect::<Vec<_>>(),
            [(1, 2)]
        );
        // costful last first; a costful posting after a lone conversion posting counts as costless
        assert_eq!(idx(&costful), [3, 0]);
        assert_eq!(idx(&costless), [5, 6]);
        assert!(partition_and_check_conversion_postings(true, &conv, &ps).is_err());
    }
}
