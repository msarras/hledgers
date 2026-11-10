//! Finalising a parsed journal (journalFinalise in Hledger.Read.Common, with
//! the stages of Hledger.Data.Journal): a pipeline of stages in upstream's
//! order that infers missing information, checks it, and adds derived data.
//!
//! The parser keeps every list in file order, so upstream's journalReverse
//! has nothing to do here. The lot stages run only for journals that use
//! lots, and the lot checks only when lots are not ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::InputOpts;
use super::journal::ParseState;
use crate::account::{AccountName, AccountType, account_name_infer_type, expand_account_names};
use crate::amount::{
    Amount, AmountCost, AmountStyle, CommodityStyles, DEFAULT_MAX_DISPLAY_PRECISION, MixedAmount,
    Precision, Rounding, Sym, add_amount_style, add_amount_style_as, styles_with_rounding,
};
use crate::dates::DateSpan;
use crate::decimal::Decimal;
use crate::lots::{calculate, classify, lot_subaccount_name, merge_cost_basis, parse_lot_name};
use crate::model::errors::posting_account_error_excerpt;
use crate::model::{Commodity, CommodityAliases, Journal, Posting, Tag, Transaction};

/// Finalise a journal parsed from this file and text (journalFinalise).
pub fn journal_finalise(
    iopts: &InputOpts,
    file: &str,
    text: &str,
    mut j: Journal,
    st: ParseState,
) -> Result<Journal, String> {
    // does the journal use lots, and are lot errors wanted?
    let has_lots = crate::lots::journal_has_lot_features(&j);
    let lenient = lenient_lots(iopts);
    let check_lots = has_lots && !lenient;
    j.global_commodity_styles = iopts.balancing.commodity_styles.clone().unwrap_or_default();
    j.default_commodity = st.default_commodity;
    j.parse_decimal_mark = st.decimal_mark;
    j.parse_year = st.year;
    warn_if_many_forecast_transactions(iopts, &j)?;
    // setup
    journal_set_last_read_time(&mut j);
    journal_add_file(&mut j, file, text);
    // account types and amount styles
    journal_add_account_types(&mut j);
    journal_style_amounts(&mut j)?;
    let mut j = journal_apply_commodity_aliases(j)?;
    // forecast and account tags
    j = journal_add_forecast(iopts, j)?;
    journal_postings_add_account_tags(&mut j);
    // pre-balancing cost and equity tagging
    j = journal_tag_costs_and_equity_and_maybe_infer_costs(iopts, false, j)?;
    // lot cost basis and transacted cost inference, gain posting tagging
    if has_lots {
        j = journal_infer_basis_from_account_names(lenient, j)?;
        j = classify::journal_infer_postings_transacted_cost(j);
    }
    if check_lots {
        j = classify::journal_check_acquire_basis(j)?;
    }
    if has_lots {
        j = classify::journal_tag_gain_postings(lenient, iopts.verbose_tags, j);
    }
    // auto postings
    if iopts.auto && !j.txn_modifiers.is_empty() {
        j = journal_add_auto_postings(iopts, j)?;
    }
    // transaction balancing
    j = journal_check_ordereddates(iopts, j)?;
    let (j, assertion_error) = journal_balance_transactions_and_defer_assertions(iopts, j)?;
    // lot classification
    let (mut j, tied) = if check_lots {
        classify::journal_classify_lot_postings(iopts.verbose_tags, j)
    } else {
        (j, Vec::new())
    };
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
    if check_lots {
        calculate::journal_check_lots_tag_values(&j)?;
        calculate::journal_check_lots_method_coherence(&j)?;
        j = calculate::journal_calculate_lots(iopts.verbose_tags, j, tied)?;
        j = calculate::journal_add_or_check_gain_postings(iopts.verbose_tags, j)?;
    }
    if has_lots {
        calculate::journal_strip_balancer_copied_bases(&mut j);
    }
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
    // each account's type and whether it was declared (inherited or not),
    // for all the names and their parents, parents first (accountNameTreeFrom)
    let mut types: BTreeMap<&str, (AccountType, bool)> = BTreeMap::new();
    for a in expand_account_names(names.iter().map(|a| &**a)) {
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

/// The account names declared, or posted to or implied as parents by the
/// postings, sorted (journalAccountNamesDeclaredOrImplied).
pub fn journal_account_names(j: &Journal) -> Vec<AccountName> {
    // the names posted to, gathered from runs of transactions on all cores
    let runs: Vec<&[Transaction]> = j.txns.chunks(1024).collect();
    let used: BTreeSet<&str> = crate::par::map(j.txns.len(), &runs, |ts| {
        (ts.iter())
            .flat_map(|t| t.postings.iter().map(|p| &*p.account))
            .collect::<BTreeSet<&str>>()
    })
    .into_iter()
    .flatten()
    .collect();
    let mut names: BTreeSet<&str> = expand_account_names(used).into_iter().collect();
    names.extend(j.declared_accounts.iter().map(|(a, _)| &**a));
    names.into_iter().map(AccountName::from).collect()
}

/// Infer the commodity styles, and apply them to the posting amounts
/// without changing their precisions (journalStyleAmounts).
pub fn journal_style_amounts(j: &mut Journal) -> Result<(), String> {
    journal_infer_commodity_styles(j);
    let styles = styles_with_rounding(Rounding::No, &j.commodity_styles());
    crate::par::for_each_mut(j.txns.len(), &mut j.txns, |t| t.style_amounts(&styles));
    Ok(())
}

/// Infer each commodity's display style from the amounts of the postings
/// and of the D and P directives, the directives' winning; with the
/// precision from the amounts that were written (journalInferCommodityStyles).
pub fn journal_infer_commodity_styles(j: &mut Journal) {
    // amounts are added last first, so the first one's style wins; the
    // posting amounts, with their symbols and styles, are gathered from
    // runs of transactions on all cores, then added in that order
    let runs: Vec<&[Transaction]> = j.txns.chunks(1024).collect();
    let amounts = crate::par::map(j.txns.len(), &runs, |ts| {
        (ts.iter().rev())
            .flat_map(|t| t.postings.iter().rev())
            .flat_map(|p| {
                let explicit = p.is_explicit_amount();
                (p.amount.amounts_raw().iter().rev())
                    .map(move |a| (a, &*a.commodity, &*a.style, explicit))
            })
            .collect::<Vec<_>>()
    });
    let (mut all, mut explicit) = (CommodityStyles::new(), CommodityStyles::new());
    for &(a, commodity, style, is_explicit) in amounts.iter().rev().flatten() {
        add_amount_style_as(a, commodity, style, &mut all);
        if is_explicit {
            add_amount_style_as(a, commodity, style, &mut explicit);
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

/// Convert the amounts written in a commodity alias to its commodity
/// (journalApplyCommodityAliases): posting amounts with their costs, cost
/// bases and balance assertions, the postings of periodic transaction and
/// auto posting rules (a multiplier's symbol is just renamed), and price
/// directives. For an alias with a quantity other than 1, quantities are
/// divided by it and unit costs, cost bases and prices multiplied by it;
/// when that is inexact, a unit cost becomes the exact total cost. A
/// converted posting keeps its original form, for print. An alias declared
/// on two commodities, or a commodity with aliases that is itself an
/// alias, is an error.
fn journal_apply_commodity_aliases(mut j: Journal) -> Result<Journal, String> {
    let excerpt = |c: &Commodity| {
        let (f, l, ex) = crate::model::errors::commodity_tag_error_excerpt(c);
        format!("{f}:{l}:\n{ex}")
    };
    let mut declaring: BTreeMap<&Sym, Vec<&Commodity>> = BTreeMap::new();
    for c in j.declared_commodities.values() {
        for (a, _) in &c.aliases {
            declaring.entry(&a.commodity).or_default().insert(0, c);
        }
    }
    if let Some((a, cs)) = declaring.iter().find(|(_, cs)| cs.len() > 1) {
        let mut msg = format!("Alias '{a}' is declared on more than one commodity:\n\n");
        for c in cs {
            msg.push_str(&excerpt(c));
            msg.push('\n');
        }
        return Err(msg);
    }
    let aliases = j.commodity_aliases();
    if let Some(c) = j
        .declared_commodities
        .values()
        .find(|c| !c.aliases.is_empty() && aliases.contains_key(&c.symbol))
    {
        return Err(format!(
            "Commodity '{}' has aliases, but is also an alias of another commodity:\n\n{}\n",
            c.symbol,
            excerpt(c)
        ));
    }
    if aliases.is_empty() {
        return Ok(j);
    }
    let conv = AliasConverter { aliases: &aliases };
    for t in &mut j.txns {
        for p in &mut t.postings {
            conv.posting(p);
        }
    }
    for pt in &mut j.periodic_txns {
        for p in &mut pt.postings {
            conv.posting(p);
        }
    }
    for tm in &mut j.txn_modifiers {
        for r in &mut tm.rules {
            if r.is_multiplier {
                r.posting.amount = r
                    .posting
                    .amount
                    .map(|a| convert_alias_amount(&aliases, a).0);
            } else {
                conv.posting(&mut r.posting);
            }
        }
    }
    for pd in &mut j.price_directives {
        match aliases.get(&pd.commodity) {
            None => pd.amount = conv.exact(&pd.amount),
            Some((c, q, _)) => {
                pd.commodity = c.clone();
                pd.amount = scale_by(q, conv.exact(&pd.amount));
            }
        }
    }
    Ok(j)
}

/// An amount written in one of these commodity aliases converted to its
/// commodity, with the alias quantity (or 1). It gets the commodity's
/// style; from a plain alias it keeps its precision, otherwise it gets the
/// commodity's precision, or more if needed to show an exact conversion,
/// or if that is unknown, all its decimals up to 8 (convertAliasAmount).
pub fn convert_alias_amount(aliases: &CommodityAliases, a: &Amount) -> (Amount, Decimal) {
    let one = Decimal::from_i64(1);
    let Some((c, q, style)) = aliases.get(&a.commodity) else {
        return (a.clone(), one);
    };
    if *q == one {
        let style = style.as_ref().map_or_else(
            || a.style.clone(),
            |s| {
                Arc::new(AmountStyle {
                    precision: a.style.precision,
                    ..(**s).clone()
                })
            },
        );
        let a = Amount {
            commodity: c.clone(),
            style,
            ..a.clone()
        };
        return (a, one);
    }
    let converted = Amount {
        commodity: c.clone(),
        quantity: &a.quantity / q,
        ..a.clone()
    };
    let digits = converted.internal_precision();
    let exact = &converted.quantity * q == a.quantity;
    let (style, precision) = match style {
        Some(s) => match s.precision {
            Precision::Fixed(n) => ((**s).clone(), if exact { n.max(digits) } else { n }),
            Precision::Natural => ((**s).clone(), digits.min(DEFAULT_MAX_DISPLAY_PRECISION)),
        },
        None => (
            (*a.style).clone(),
            digits.min(DEFAULT_MAX_DISPLAY_PRECISION),
        ),
    };
    let style = Arc::new(AmountStyle {
        precision: Precision::Fixed(precision),
        ..style
    });
    (Amount { style, ..converted }, q.clone())
}

/// A calculated amount with all its decimal digits shown, up to 8, but no
/// trailing zeros (withAllDigits).
fn with_all_digits(a: Amount) -> Amount {
    let quantity = a.quantity.normalize();
    let places = quantity.places().min(DEFAULT_MAX_DISPLAY_PRECISION);
    Amount { quantity, ..a }.with_precision(Precision::Fixed(places))
}

/// A per-unit price multiplied by an alias quantity (scaleBy).
fn scale_by(q: &Decimal, a: Amount) -> Amount {
    if *q == Decimal::from_i64(1) {
        return a;
    }
    with_all_digits(Amount {
        quantity: &a.quantity * q,
        ..a
    })
}

/// The conversions of journalApplyCommodityAliases.
struct AliasConverter<'a> {
    aliases: &'a CommodityAliases,
}

impl AliasConverter<'_> {
    fn is_alias(&self, a: &Amount) -> bool {
        self.aliases.contains_key(&a.commodity)
    }

    /// A cost, price or assertion amount converted, with all its digits
    /// shown if it was divided (convertExactAmount).
    fn exact(&self, a: &Amount) -> Amount {
        match convert_alias_amount(self.aliases, a) {
            (c, q) if q == Decimal::from_i64(1) => c,
            (c, _) => with_all_digits(c),
        }
    }

    /// Whether converting an amount from an alias rounds its quantity
    /// (isInexact).
    fn is_inexact(&self, a: &Amount) -> bool {
        let (c, q) = convert_alias_amount(self.aliases, a);
        q != Decimal::from_i64(1) && &c.quantity * &q != a.quantity
    }

    /// An amount converted, with its cost and cost basis (convertAmount).
    fn amount(&self, a: &Amount) -> Amount {
        let (mut c, q) = convert_alias_amount(self.aliases, a);
        c.cost = a.cost.as_ref().map(|cost| match cost {
            AmountCost::Unit(u) if self.is_inexact(a) || self.is_inexact(u) => {
                let total = Amount {
                    quantity: &a.quantity.abs() * &u.quantity,
                    ..(**u).clone()
                };
                AmountCost::Total(Box::new(with_all_digits(
                    convert_alias_amount(self.aliases, &total).0,
                )))
            }
            AmountCost::Unit(u) => AmountCost::Unit(Box::new(scale_by(&q, self.exact(u)))),
            AmountCost::Total(t) => AmountCost::Total(Box::new(self.exact(t))),
        });
        c.cost_basis = a.cost_basis.as_ref().map(|b| {
            Box::new(crate::amount::CostBasis {
                cost: b
                    .cost
                    .as_ref()
                    .map(|k| Box::new(scale_by(&q, self.exact(k)))),
                ..(**b).clone()
            })
        });
        c
    }

    /// A posting with its amounts and assertion converted, if any is in an
    /// alias, keeping its original form (convertPosting).
    fn posting(&self, p: &mut Posting) {
        let has_alias = |a: &Amount| {
            self.is_alias(a)
                || a.and_cost().skip(1).any(|c| self.is_alias(c))
                || a.cost_basis
                    .as_ref()
                    .and_then(|b| b.cost.as_deref())
                    .is_some_and(|c| self.is_alias(c))
        };
        if !(p.amount.amounts_raw().iter().any(has_alias)
            || p.assertion
                .as_ref()
                .is_some_and(|ba| self.is_alias(&ba.amount)))
        {
            return;
        }
        p.keep_original();
        p.amount = p.amount.map(|a| self.amount(a));
        if let Some(ba) = &mut p.assertion {
            ba.amount = self.exact(&ba.amount);
        }
    }
}

/// In postings to a lot subaccount (`ACCOUNT:{DATE, COST}`), take the cost
/// basis from the name, merged with the amount's own; with `lenient`, a
/// posting whose name is invalid or conflicting is left as it is
/// (journalInferBasisFromAccountNames).
fn journal_infer_basis_from_account_names(
    lenient: bool,
    mut j: Journal,
) -> Result<Journal, String> {
    let parse = |s: &str| super::amount::parse_amount(s).ok();
    for t in &mut j.txns {
        for i in 0..t.postings.len() {
            let p = &t.postings[i];
            let Some(name) = lot_subaccount_name(&p.account) else {
                continue;
            };
            let pos_err = |msg: String| {
                let (f, l, ex) = posting_account_error_excerpt(t, p);
                format!("{f}:{l}:\n{ex}\n{msg}")
            };
            let lot_err = |msg: String| {
                pos_err(format!(
                    "{msg}\n\nA final account name part enclosed in {{ }} must be a valid lot subaccount name.\n\
                     Please adjust the account name, or use --ignore-lots/-I."
                ))
            };
            let inferred = parse_lot_name(parse, name)
                .map_err(lot_err)
                .and_then(|cb| {
                    if cb.date.is_none() || cb.cost.is_none() {
                        return Err(lot_err(format!(
                            "lot subaccount name must contain a date and cost: {name}"
                        )));
                    }
                    p.amount
                        .amounts_raw()
                        .iter()
                        .map(|a| {
                            let merged = match &a.cost_basis {
                                None => cb.clone(),
                                Some(existing) => merge_cost_basis(&cb, existing).map_err(|e| {
                                    pos_err(format!(
                                        "{e}\n\nThe lot subaccount name and the amount's lot annotation must agree.\n\
                                         Please make them match, or remove one of them."
                                    ))
                                })?,
                            };
                            Ok(Amount {
                                cost_basis: Some(Box::new(merged)),
                                ..a.clone()
                            })
                        })
                        .collect::<Result<Vec<Amount>, String>>()
                });
            match inferred {
                Ok(amts) => t.postings[i].amount = MixedAmount::from_amounts(amts),
                Err(_) if lenient => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(j)
}

/// With `--forecast`, add the transactions the periodic transaction rules
/// generate in the forecast period, styled (journalAddForecast).
fn journal_add_forecast(iopts: &InputOpts, mut j: Journal) -> Result<Journal, String> {
    let Some(span) = forecast_period(iopts, &j) else {
        return Ok(j);
    };
    let styles = j.commodity_styles();
    let mut generated = Vec::new();
    for pt in &j.periodic_txns {
        let txns = super::modifiers::run_periodic_transaction(iopts.verbose_tags, pt, span)?;
        generated.extend(
            txns.into_iter()
                .filter(|t| span.contains(t.date))
                .map(|mut t| {
                    t.style_amounts(&styles);
                    t
                }),
        );
    }
    j.txns.extend(generated);
    Ok(j)
}

/// The span to generate forecast transactions in, with `--forecast`: its
/// argument's dates, or else from after the journal's last transaction (or
/// the report's start, if later) or today, until the report's end or 180
/// days from today (forecastPeriod).
fn forecast_period(iopts: &InputOpts, j: &Journal) -> Option<DateSpan> {
    let requested = iopts.forecast?;
    let report = iopts.report_span;
    let journal_end = j.date_span(false).end();
    let start = requested
        .start()
        .or(journal_end.max(report.start()))
        .unwrap_or(iopts.today);
    let end = requested
        .end()
        .or(report.end())
        .unwrap_or_else(|| iopts.today.add_days(180));
    Some(DateSpan::exact(Some(start), Some(end)))
}

/// More forecast transactions than this probably come from a mistyped date
/// (maxExpectedForecastTxns).
const MAX_EXPECTED_FORECAST_TXNS: usize = 100_000;

/// Warn before generating an implausible number of forecast transactions,
/// counting them without generating them (#1683).
fn warn_if_many_forecast_transactions(iopts: &InputOpts, j: &Journal) -> Result<(), String> {
    let Some(span) = forecast_period(iopts, j) else {
        return Ok(());
    };
    let mut n = 0;
    for pt in &j.periodic_txns {
        n += super::modifiers::periodic_transaction_dates(pt, span)?.count();
    }
    if n > MAX_EXPECTED_FORECAST_TXNS {
        crate::console::warn(
            &iopts.prog_args,
            &format!(
                "--forecast would generate {n} transactions (forecast period {span}),\n\
                 using a lot of memory. If not intended, press control-C now, and check for a mistyped date."
            ),
        );
    }
    Ok(())
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
fn journal_tag_costs_and_equity_and_maybe_infer_costs(
    iopts: &InputOpts,
    infer_costs: bool,
    j: Journal,
) -> Result<Journal, String> {
    super::costs::journal_tag_costs_and_equity_and_maybe_infer_costs(
        iopts.verbose_tags,
        infer_costs,
        j,
    )
}

/// Balance the transactions without checking assertions, then add auto
/// postings (journalAddAutoPostings).
fn journal_add_auto_postings(iopts: &InputOpts, j: Journal) -> Result<Journal, String> {
    let bopts = super::BalancingOpts {
        ignore_assertions: true,
        lotful_commodities: j.lotful_commodities(),
        account_lots_tags: j.account_lots_tags(),
        lenient_lots: lenient_lots(iopts),
        verbose_balancing_tags: iopts.verbose_tags,
        ..iopts.balancing.clone()
    };
    let (j, _) = super::balancing::journal_balance_transactions(&bopts, j, false)?;
    super::modifiers::journal_modify_transactions(iopts.verbose_tags, iopts.today, j)
}

/// With `hledger check ordereddates`, check that transactions are in date
/// order, before balancing (journalCheckOrdereddates).
fn journal_check_ordereddates(iopts: &InputOpts, j: Journal) -> Result<Journal, String> {
    if iopts.checking("ordereddates") {
        super::checks::journal_check_ordereddates(&j)?;
    }
    Ok(j)
}

/// Balance the transactions, inferring missing amounts and balance
/// assignments, and check balance assertions, returning the first
/// assertion failure separately so that later errors are reported first
/// (journalBalanceTransactionsAndDeferAssertions). Assertions are checked
/// unless `-I` is given without `--strict` or `check assertions`.
fn journal_balance_transactions_and_defer_assertions(
    iopts: &InputOpts,
    j: Journal,
) -> Result<(Journal, Option<String>), String> {
    let check_assertions =
        !iopts.balancing.ignore_assertions || iopts.strict || iopts.checking("assertions");
    let bopts = super::BalancingOpts {
        ignore_assertions: !check_assertions,
        account_types: j.account_types.clone(),
        lotful_commodities: j.lotful_commodities(),
        account_lots_tags: j.account_lots_tags(),
        lenient_lots: lenient_lots(iopts),
        verbose_balancing_tags: iopts.verbose_tags,
        imbalance_account: j
            .account_for_type(
                crate::account::DEFAULT_IMBALANCE_ACCOUNT,
                AccountType::Imbalance,
            )
            .to_string(),
        ..iopts.balancing.clone()
    };
    super::balancing::journal_balance_transactions(&bopts, j, true)
}

/// Whether lot errors are skipped: with `--ignore-lots`, unless `--strict`
/// or `check lots` asks for them (lenientlots).
fn lenient_lots(iopts: &InputOpts) -> bool {
    iopts.ignore_lots && !iopts.strict && !iopts.checking("lots")
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
fn journal_infer_market_prices_from_transactions(j: Journal) -> Journal {
    super::costs::journal_infer_market_prices_from_transactions(j)
}

/// Number the account declarations in order across all files
/// (journalRenumberAccountDeclarations).
pub fn journal_renumber_account_declarations(j: &mut Journal) {
    for (i, (_, info)) in j.declared_accounts.iter_mut().enumerate() {
        info.order = i + 1;
    }
}

/// Infer equity postings from costs (journalInferEquityFromCosts).
fn journal_infer_equity_from_costs(iopts: &InputOpts, j: Journal) -> Journal {
    super::costs::journal_infer_equity_from_costs(iopts.verbose_tags, j)
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
    j1.parse_decimal_mark = j2.parse_decimal_mark;
    j1.parse_year = j2.parse_year;
    j1.last_read_time = j1.last_read_time.max(j2.last_read_time);
    j1.aux_files.extend(j2.aux_files);
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
            "2000-01-01\n  myassets:bank  1\n  revenues:gain  -1\n  expenses:food  0\n  other\n",
        ));
        let t = |a: &str| j.account_types.get(a).copied();
        assert_eq!(t("myassets:bank"), Some(AccountType::Asset));
        assert_eq!(t("revenues:gain"), Some(AccountType::Gain));
        assert_eq!(t("expenses"), Some(AccountType::Expense));
        assert_eq!(t("other"), None);
        // a type declared for one account is not inferred for others
        let j = read("account a  ; type:X\n2000-01-01\n  expenses:x  1\n  a  -1\n");
        assert_eq!(j.account_types.get("expenses:x"), None);
        // there, a declared account inherits its undeclared parent's inferred type
        let j = read("account trade  ; type:V\naccount equity:conversion\n");
        let t = j.account_types.get("equity:conversion").copied();
        assert_eq!(t, Some(AccountType::Equity));
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
