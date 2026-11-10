//! Lot tracking for investment accounting (Hledger.Data.Lots, with the lot
//! helpers of Hledger.Data.Journal and Hledger.Data.AccountType).
//!
//! Finalising a journal that uses lots runs these stages, in upstream's
//! order: before balancing, cost bases are inferred from lot subaccount
//! names and transacted costs from cost bases, acquisitions' bases are
//! checked against their costs, and user-written gain postings are tagged
//! ([`classify`]); after balancing, lot postings are classified as acquire,
//! dispose, transfer-from or transfer-to ([`classify`]); then lots are
//! calculated in date order, adding lot subaccounts and splitting
//! disposals and transfers across the lots they select, and realised gain
//! postings are added or checked ([`calculate`]).

pub mod calculate;
pub mod classify;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::account::{
    AccountName, AccountType, account_name_type, lot_base_account, parent_account_names,
    split_lot_subaccount,
};
use crate::amount::render::{AmountFormat, show_amount_b, show_mixed_amount_one_line};
use crate::amount::{Amount, CostBasis, MixedAmount, Precision, Sym};
use crate::dates::Day;
use crate::decimal::Decimal;
use crate::model::errors::{posting_error_excerpt_by_index, transaction_error_excerpt};
use crate::model::{
    GENERATED_POSTING_TAG, Journal, LOT_PARENT_ASSERTION_TAG, LOTSPLIT_POSTING_TAG, Posting,
    Transaction,
};

/// Which way lots flow in a lot-tracking account: in a Long account
/// (assets) a positive posting opens a lot and a negative one closes it; in
/// a Short account (liabilities, holding short positions) it is the reverse
/// (LotDirection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LotDirection {
    Long,
    Short,
}

impl LotDirection {
    /// Long for assets (and cash), Short for liabilities, none for other
    /// types, which don't hold lots (accountTypeLotDirection).
    pub fn of_type(t: AccountType) -> Option<LotDirection> {
        if t.is_subtype_of(AccountType::Asset) {
            Some(LotDirection::Long)
        } else if t.is_subtype_of(AccountType::Liability) {
            Some(LotDirection::Short)
        } else {
            None
        }
    }

    /// The sign of a posting that opens a lot in this direction
    /// (lotDirectionSign).
    pub fn sign(self) -> Decimal {
        match self {
            LotDirection::Long => Decimal::from_i64(1),
            LotDirection::Short => Decimal::from_i64(-1),
        }
    }

    /// Does this amount open or receive a lot (isLotInflow)?
    pub fn is_inflow(self, a: &Amount) -> bool {
        (&a.quantity * &self.sign()) > Decimal::ZERO
    }

    /// Does this amount close or send a lot (isLotOutflow)?
    pub fn is_outflow(self, a: &Amount) -> bool {
        (&a.quantity * &self.sign()) < Decimal::ZERO
    }
}

/// How lots are selected for disposal or transfer: oldest first, newest
/// first, highest cost first, at their average cost (in oldest first
/// order), or by explicit selector; the `*ALL` variants select per account
/// but check that no other account's lots should go first
/// (ReductionMethod).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReductionMethod {
    Fifo,
    Lifo,
    Hifo,
    Average,
    Specid,
    FifoAll,
    LifoAll,
    HifoAll,
    AverageAll,
}

impl ReductionMethod {
    /// A method's name in a lots: tag value, ignoring case and surrounding
    /// space (parseReductionMethod).
    pub fn parse(v: &str) -> Option<ReductionMethod> {
        use ReductionMethod::*;
        Some(
            match v
                .trim_matches(crate::text::is_space)
                .to_uppercase()
                .as_str()
            {
                "FIFO" => Fifo,
                "LIFO" => Lifo,
                "HIFO" => Hifo,
                "AVERAGE" => Average,
                "SPECID" => Specid,
                "FIFOALL" => FifoAll,
                "LIFOALL" => LifoAll,
                "HIFOALL" => HifoAll,
                "AVERAGEALL" => AverageAll,
                _ => return None,
            },
        )
    }

    /// Whether disposals use the pool's weighted average cost
    /// (methodIsAverage).
    pub fn is_average(self) -> bool {
        matches!(self, ReductionMethod::Average | ReductionMethod::AverageAll)
    }

    /// Whether selections are checked across all accounts (methodIsGlobal).
    pub fn is_global(self) -> bool {
        use ReductionMethod::*;
        matches!(self, FifoAll | LifoAll | HifoAll | AverageAll)
    }

    /// A `*ALL` method's per-account ordering (methodBaseOrdering).
    pub fn base_ordering(self) -> ReductionMethod {
        use ReductionMethod::*;
        match self {
            FifoAll => Fifo,
            LifoAll => Lifo,
            HifoAll => Hifo,
            AverageAll => Average,
            m => m,
        }
    }
}

/// The method's name, as Haskell's show gives it.
impl fmt::Display for ReductionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ReductionMethod::*;
        f.write_str(match self {
            Fifo => "FIFO",
            Lifo => "LIFO",
            Hifo => "HIFO",
            Average => "AVERAGE",
            Specid => "SPECID",
            FifoAll => "FIFOALL",
            LifoAll => "LIFOALL",
            HifoAll => "HIFOALL",
            AverageAll => "AVERAGEALL",
        })
    }
}

/// Identifies a lot of a commodity: its acquisition date and optional
/// label, ordered by date, then label (no label first) (LotId).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LotId {
    pub date: Day,
    pub label: Option<Arc<str>>,
}

/// As Haskell's derived show gives it, for internal errors.
impl fmt::Display for LotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match &self.label {
            Some(l) => format!("Just {}", crate::text::show_string(l)),
            None => "Nothing".to_string(),
        };
        write!(f, "LotId {{lotDate = {}, lotLabel = {label}}}", self.date)
    }
}

/// Each commodity's lots, each lot's balance in each account holding part
/// of it (LotState). A lot with no accounts left is a fully disposed one,
/// kept so that its label is not reused.
pub type LotState = BTreeMap<Sym, BTreeMap<LotId, BTreeMap<AccountName, Amount>>>;

/// What lot classification and gain tagging need to know of a journal:
/// account types, the commodities declared lotful, and the accounts' lots:
/// tags. The balancer has these in its options, finalising in the journal.
pub struct LotLookups<'a> {
    pub account_types: &'a BTreeMap<AccountName, AccountType>,
    pub lotful: &'a BTreeSet<Sym>,
    pub lots_tags: &'a BTreeMap<AccountName, String>,
}

impl LotLookups<'_> {
    pub fn account_type(&self, a: &str) -> Option<AccountType> {
        account_name_type(self.account_types, a)
    }

    pub fn is_lotful(&self, c: &str) -> bool {
        self.lotful.contains(c)
    }

    /// Does this account, or its nearest ancestor with a lots: tag, opt out
    /// of lot tracking with lots: NONE (accountUsesNoLotsWith)?
    pub fn uses_no_lots(&self, a: &str) -> bool {
        std::iter::once(a)
            .chain(parent_account_names(a))
            .find_map(|a| self.lots_tags.get(a))
            .is_some_and(|v| is_none_lots_tag_value(v))
    }

    /// The lot direction of an account, or of a lot subaccount's base
    /// account (lotDirectionOf).
    pub fn direction(&self, a: &str) -> Option<LotDirection> {
        self.account_type(lot_base_account(a))
            .and_then(LotDirection::of_type)
    }

    fn is_asset(&self, a: &str) -> bool {
        self.account_type(a)
            .is_some_and(|t| t.is_subtype_of(AccountType::Asset))
    }

    fn is_equity(&self, a: &str) -> bool {
        self.account_type(a)
            .is_some_and(|t| t.is_subtype_of(AccountType::Equity))
    }
}

/// Is this lots: tag value NONE, ignoring case (isNoneLotsTagValue)?
pub fn is_none_lots_tag_value(v: &str) -> bool {
    v.trim_matches(crate::text::is_space).to_uppercase() == "NONE"
}

/// The first lots: tag value among these tags, ignoring the name's case.
fn lots_tag_value(tags: &[crate::model::Tag]) -> Option<&str> {
    tags.iter()
        .find(|(k, _)| k.to_lowercase() == "lots")
        .map(|(_, v)| v.as_str())
}

/// Does this commodity have a lots: tag (journalCommodityUsesLots)?
pub fn commodity_uses_lots(j: &Journal, c: &str) -> bool {
    j.declared_commodity_tags
        .get(c)
        .is_some_and(|tags| lots_tag_value(tags).is_some())
}

/// Does this account opt out of lot tracking (journalAccountUsesNoLots)?
pub fn account_uses_no_lots(j: &Journal, a: &str) -> bool {
    std::iter::once(a)
        .chain(parent_account_names(a))
        .find_map(|a| {
            j.declared_account_tags
                .get(a)
                .and_then(|t| lots_tag_value(t))
        })
        .is_some_and(is_none_lots_tag_value)
}

/// The reduction method in effect for an account and commodity, and where
/// it came from: the account's (inherited) lots: tag, else the commodity's,
/// else FIFO (resolveReductionMethodForAccount).
pub fn resolve_reduction_method(j: &Journal, acct: &str, c: &str) -> (ReductionMethod, String) {
    let account_method =
        lots_tag_value(&j.inherited_account_tags(acct)).and_then(ReductionMethod::parse);
    if let Some(m) = account_method {
        return (m, format!("from account tag on {acct}"));
    }
    let commodity_method = j
        .declared_commodity_tags
        .get(c)
        .and_then(|tags| lots_tag_value(tags))
        .and_then(ReductionMethod::parse);
    match commodity_method {
        Some(m) => (m, format!("from commodity tag on {c}")),
        None => (ReductionMethod::Fifo, "default".to_string()),
    }
}

/// The lot direction of an account in this journal (journalLotDirection).
pub fn lot_direction(j: &Journal, a: &str) -> Option<LotDirection> {
    j.account_type(lot_base_account(a))
        .and_then(LotDirection::of_type)
}

/// The lot direction of an account, Long for accounts of other types
/// (journalLotDirectionOrLong).
pub fn lot_direction_or_long(j: &Journal, a: &str) -> LotDirection {
    lot_direction(j, a).unwrap_or(LotDirection::Long)
}

/// The lot subaccount name (`{...}`) at the end of an account name, if any
/// (lotSubaccountName).
pub fn lot_subaccount_name(a: &str) -> Option<&str> {
    split_lot_subaccount(a).map(|(_, name)| name)
}

/// A lot name, as used for a lot subaccount: `{DATE, "LABEL", COST}` with
/// the parts present; the cost in its own style, a zero with its commodity
/// (showLotName).
pub fn show_lot_name(cb: &CostBasis) -> String {
    let fmt = AmountFormat {
        zero_commodity: true,
        ..AmountFormat::NO_COST
    };
    let parts: Vec<String> = [
        cb.date.map(|d| d.to_string()),
        cb.label.as_ref().map(|l| format!("\"{l}\"")),
        cb.cost.as_ref().map(|c| show_amount_b(&fmt, c).s),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!("{{{}}}", parts.join(", "))
}

/// The cost basis with its cost styled (styleLotCbCost).
pub fn style_lot_cb_cost(styles: &crate::amount::CommodityStyles, cb: &CostBasis) -> CostBasis {
    CostBasis {
        cost: cb
            .cost
            .as_ref()
            .map(|c| Box::new((**c).clone().styled(styles))),
        ..cb.clone()
    }
}

/// The cost basis with its cost showing at least its commodity style's
/// decimal places (widenLotCbCost).
fn widen_lot_cb_cost(styles: &crate::amount::CommodityStyles, cb: CostBasis) -> CostBasis {
    let cost = cb
        .cost
        .map(|c| match styles.get(&c.commodity).map(|s| s.precision) {
            Some(Precision::Fixed(p)) => Box::new(c.with_precision_min(p)),
            _ => c,
        });
    CostBasis { cost, ..cb }
}

/// A lot name, without the cost under the average methods, where it
/// changes (showLotNameForMethod).
pub fn show_lot_name_for_method(method: ReductionMethod, cb: &CostBasis) -> String {
    if method.is_average() {
        show_lot_name(&CostBasis {
            cost: None,
            ..cb.clone()
        })
    } else {
        show_lot_name(cb)
    }
}

/// Do two lot cost amounts refer to the same cost: the same commodity, and
/// equal quantities, or one equal to the other rounded to its display
/// precision or to the first's decimal places (lotCostsMatch)?
pub fn lot_costs_match(x: &Amount, y: &Amount) -> bool {
    let places = |a: &Amount| a.quantity.normalize().places();
    x.commodity == y.commodity
        && (x.quantity == y.quantity
            || x.quantity == y.rounded_quantity()
            || x.rounded_quantity() == y.quantity
            || x.quantity == y.quantity.round_to(places(x))
            || x.quantity.round_to(places(y)) == y.quantity)
}

/// Two cost bases merged: each part from whichever has it, which must
/// agree if both do (mergeCostBasis).
pub fn merge_cost_basis(a: &CostBasis, b: &CostBasis) -> Result<CostBasis, String> {
    fn field<T: Clone + PartialEq>(
        name: &str,
        show: impl Fn(&T) -> String,
        x: &Option<T>,
        y: &Option<T>,
    ) -> Result<Option<T>, String> {
        match (x, y) {
            (Some(x), Some(y)) if x != y => Err(format!(
                "conflicting cost basis {name}: account name has {} but amount has {}",
                show(x),
                show(y)
            )),
            (Some(_), _) => Ok(x.clone()),
            (None, _) => Ok(y.clone()),
        }
    }
    let date = field("date", |d: &Day| d.to_string(), &a.date, &b.date)?;
    let label = field("label", |l: &Arc<str>| l.to_string(), &a.label, &b.label)?;
    let cost = match (&a.cost, &b.cost) {
        (Some(x), Some(y)) if !lot_costs_match(x, y) => {
            let show = |a: &Amount| show_amount_b(&AmountFormat::NO_COST, a).s;
            return Err(format!(
                "conflicting cost basis cost: account name has {} but amount has {}",
                show(x),
                show(y)
            ));
        }
        (Some(x), _) => Some(x.clone()),
        (None, y) => y.clone(),
    };
    Ok(CostBasis { date, label, cost })
}

/// Like [`merge_cost_basis`], ignoring the costs under the average methods
/// (mergeCostBasisForMethod).
fn merge_cost_basis_for_method(
    method: ReductionMethod,
    a: &CostBasis,
    b: &CostBasis,
) -> Result<CostBasis, String> {
    if method.is_average() {
        let costless = |cb: &CostBasis| CostBasis {
            cost: None,
            ..cb.clone()
        };
        merge_cost_basis(&costless(a), &costless(b))
    } else {
        merge_cost_basis(a, b)
    }
}

/// A lot name (`{...}`, as [`show_lot_name`] makes) read back as a cost
/// basis, the cost read by the given parser. The parts are all optional,
/// but must be in order: date, label, cost (parseLotName).
pub fn parse_lot_name(
    parse_amount: impl Fn(&str) -> Option<Amount>,
    t: &str,
) -> Result<CostBasis, String> {
    let Some(inner) = t.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return Err(format!("lot name must be enclosed in braces: {t}"));
    };
    let inner = inner.trim_matches(crate::text::is_space);
    let mut cb = CostBasis::default();
    if inner.is_empty() {
        return Ok(cb);
    }
    for p in split_lot_name_parts(inner) {
        if is_date_part(p) {
            match parse_lot_date(p) {
                Some(d) => cb.date = Some(d),
                None => return Err(format!("invalid date in lot name: {p}")),
            }
        } else if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
            cb.label = Some(Arc::from(&p[1..p.len() - 1]));
        } else {
            match parse_amount(p) {
                Some(a) => cb.cost = Some(Box::new(a)),
                None => {
                    return Err(missing_date_comma_hint(p)
                        .or_else(|| wrong_order_hint(t, p))
                        .unwrap_or_else(|| format!("invalid lot name: {p}")));
                }
            }
        }
    }
    Ok(cb)
}

/// Ten characters, all digits or hyphens: looks like a date.
fn is_date_part(p: &str) -> bool {
    p.chars().count() == 10 && p.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// A YYYY-MM-DD date, if it is a valid one.
fn parse_lot_date(p: &str) -> Option<Day> {
    let b = p.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&p[..4])?, num(&p[5..7])?, num(&p[8..])?);
    Day::from_ymd(y, m as u32, d as u32)
}

/// The first `n` characters and the rest.
fn split_chars(s: &str, n: usize) -> (&str, &str) {
    let i = s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
    s.split_at(i)
}

/// A part that starts with a date not followed by a comma: probably a
/// forgotten comma, shown corrected (missingDateCommaHint).
fn missing_date_comma_hint(p: &str) -> Option<String> {
    if p.chars().count() <= 10 {
        return None;
    }
    let (candidate, rest) = split_chars(p, 10);
    let rest = rest.trim_start_matches(crate::text::is_space);
    (is_date_part(candidate) && !rest.is_empty() && !rest.starts_with(',')).then(|| {
        format!("Missing comma in lot name: {p}\nDid you mean:              {candidate}, {rest} ?")
    })
}

/// A cost part starting with a date, or followed by a comma and something
/// like a date or label: the parts are out of order (wrongOrderHint).
fn wrong_order_hint(t: &str, p: &str) -> Option<String> {
    let starts_another = |s: &str| s.starts_with('"') || is_date_part(split_chars(s, 10).0);
    let after_commas = p
        .char_indices()
        .filter(|(_, c)| *c == ',')
        .map(|(i, _)| p[i + 1..].trim_start_matches(crate::text::is_space));
    (is_date_part(split_chars(p, 10).0) || after_commas.into_iter().any(starts_another)).then(
        || {
            format!(
                "lot name parts must be in this order (if present): {{DATE, \"LABEL\", COST}}: {t}"
            )
        },
    )
}

/// A lot name's inside split into its date, label and cost parts, peeling
/// the date and label off the front, so that commas in the cost (a decimal
/// comma, or a quoted commodity symbol) don't split it (splitParts).
fn split_lot_name_parts(s: &str) -> Vec<&str> {
    fn strip_sep(s: &str) -> &str {
        let s = s.trim_start_matches(crate::text::is_space);
        s.strip_prefix(',')
            .map_or(s, |s| s.trim_start_matches(crate::text::is_space))
    }
    let mut parts = Vec::new();
    let mut rest = s;
    // a date, followed by the end or a comma
    if rest.chars().count() >= 10 {
        let (candidate, after) = split_chars(rest, 10);
        let after_trimmed = after.trim_start_matches(crate::text::is_space);
        if is_date_part(candidate) && (after_trimmed.is_empty() || after_trimmed.starts_with(',')) {
            parts.push(candidate);
            rest = strip_sep(after);
        }
    }
    // a quoted label, unless what follows looks like the rest of a cost
    // amount (then it is a quoted commodity symbol)
    if let Some(s1) = rest.strip_prefix('"')
        && let Some(i) = s1.find('"')
    {
        let after = &s1[i + 1..];
        let continues_cost = after
            .trim_start_matches(crate::text::is_space)
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.'));
        if !continues_cost {
            parts.push(&rest[..i + 2]);
            rest = strip_sep(after);
        }
    }
    let cost = rest.trim_matches(crate::text::is_space);
    if !cost.is_empty() {
        parts.push(cost);
    }
    parts
}

/// Does this journal use lots at all: a commodity declared lotful, an
/// account with a lots: tag, or a posting (in transactions, periodic
/// transaction rules or auto posting rules) with a cost basis or a lot
/// subaccount (journalHasLotFeatures)?
pub fn journal_has_lot_features(j: &Journal) -> bool {
    let has_lot_feature = |p: &Posting| {
        p.amount
            .amounts_raw()
            .iter()
            .any(|a| a.cost_basis.is_some())
            || lot_subaccount_name(&p.account).is_some()
    };
    !j.lotful_commodities().is_empty()
        || !j.account_lots_tags().is_empty()
        || j.txns.iter().flat_map(|t| &t.postings).any(has_lot_feature)
        || j.periodic_txns
            .iter()
            .flat_map(|pt| &pt.postings)
            .any(has_lot_feature)
        || j.txn_modifiers
            .iter()
            .flat_map(|tm| &tm.rules)
            .any(|r| has_lot_feature(&r.posting))
}

/// Could this transaction involve lots: has it an amount with a cost basis
/// or in a lotful commodity (transactionHasLotfulAmounts)?
pub fn transaction_has_lotful_amounts(lotful: &BTreeSet<Sym>, t: &Transaction) -> bool {
    t.postings
        .iter()
        .flat_map(|p| p.amount.amounts_raw())
        .any(|a| a.cost_basis.is_some() || lotful.contains(&a.commodity))
}

/// The value of a posting's `_ptype` tag, if it has one.
fn ptype(p: &Posting) -> Option<&str> {
    p.tags
        .iter()
        .find(|(n, _)| n == "_ptype")
        .map(|(_, v)| v.as_str())
}

fn has_ptype(p: &Posting, v: &str) -> bool {
    p.tags.iter().any(|(n, x)| n == "_ptype" && x == v)
}

/// Has this posting a lot-related `_ptype` tag (isLotPosting)?
pub fn is_lot_posting(p: &Posting) -> bool {
    ["acquire", "dispose", "transfer-from", "transfer-to", "gain"]
        .iter()
        .any(|v| has_ptype(p, v))
}

/// Has this posting any `_ptype` tag (isClassifiedPosting)?
pub fn is_classified_posting(p: &Posting) -> bool {
    p.tags.iter().any(|(n, _)| n == "_ptype")
}

pub fn is_acquire_posting(p: &Posting) -> bool {
    has_ptype(p, "acquire")
}

pub fn is_dispose_posting(p: &Posting) -> bool {
    has_ptype(p, "dispose")
}

pub fn is_transfer_from_posting(p: &Posting) -> bool {
    has_ptype(p, "transfer-from")
}

pub fn is_transfer_to_posting(p: &Posting) -> bool {
    has_ptype(p, "transfer-to")
}

/// Is this a disposal's gain posting, user-written or generated
/// (isGainPosting)?
pub fn is_gain_posting(p: &Posting) -> bool {
    has_ptype(p, "gain")
}

/// Was this posting generated by hledger (isGeneratedPosting)?
fn is_generated_posting(p: &Posting) -> bool {
    p.has_tag(GENERATED_POSTING_TAG)
}

/// Does this posting carry a cost basis that the balancer copied into its
/// inferred amount, rather than one the user wrote (hasBalancerCopiedBasis)?
pub fn has_balancer_copied_basis(p: &Posting) -> bool {
    !p.original().has_amount()
        && p.amount
            .amounts_raw()
            .iter()
            .any(|a| a.cost_basis.is_some())
}

/// Does this posting move a lotful commodity in a lot-tracking account
/// without having been classified (isUnclassifiedLotfulPosting)?
fn is_unclassified_lotful_posting(j: &Journal, p: &Posting) -> bool {
    p.is_real()
        && p.has_amount()
        && !is_lot_posting(p)
        && lot_direction(j, &p.account).is_some()
        && !account_uses_no_lots(j, lot_base_account(&p.account))
        && p.amount
            .amounts_raw()
            .iter()
            .any(|a| commodity_uses_lots(j, &a.commodity) && !a.quantity.is_zero())
        && !has_balancer_copied_basis(p)
}

/// The transaction approximately as the user wrote it, for error excerpts:
/// postings as originally written, generated ones dropped
/// (transactionAsWritten).
fn transaction_as_written(t: &Transaction) -> Transaction {
    Transaction {
        postings: t
            .postings
            .iter()
            .filter(|q| !is_generated_posting(q))
            .map(|q| q.original().clone())
            .collect(),
        ..t.clone()
    }
}

/// A posting index in the transaction as its index in the transaction as
/// written (asWrittenPostingIndex).
fn as_written_posting_index(t: &Transaction, idx: usize) -> usize {
    t.postings[..idx.min(t.postings.len())]
        .iter()
        .filter(|q| !is_generated_posting(q))
        .count()
}

/// How classification read a transaction's postings, as a sentence, or
/// nothing if it has not classified them (postingsReadAs).
fn postings_read_as(t: &Transaction) -> String {
    if !t.postings.iter().any(is_classified_posting) {
        return String::new();
    }
    let read_as = |p: &&Posting| match ptype(p) {
        Some(v) => v.to_string(),
        None if p.has_tag(LOT_PARENT_ASSERTION_TAG) => "balance-assertion".to_string(),
        None => "unclassified".to_string(),
    };
    let (generated, written): (Vec<&Posting>, Vec<&Posting>) =
        t.postings.iter().partition(|p| is_generated_posting(p));
    let list = |ps: &[&Posting]| ps.iter().map(read_as).collect::<Vec<_>>().join(", ");
    let generated = if generated.is_empty() {
        String::new()
    } else {
        format!("; and generated: {}", list(&generated))
    };
    format!("Postings were read as: {}{generated}.", list(&written))
}

/// A lot error with how the transaction's postings were read appended, if
/// they were classified (appendPostingsReadAs).
pub fn append_postings_read_as(t: &Transaction, msg: String) -> String {
    match postings_read_as(t) {
        r if r.is_empty() => msg,
        r => format!("{}\n\n{r}", msg.trim_end_matches('\n')),
    }
}

/// `FILE:LINE:` and the transaction as written, then a blank line
/// (txnErrPrefix).
pub fn txn_err_prefix(t: &Transaction) -> String {
    let (f, l, ex) = transaction_error_excerpt(&transaction_as_written(t), None);
    format!("{f}:{l}:\n{ex}\n")
}

/// Like [`txn_err_prefix`], numbered at the posting with this index
/// (postingAtErrPrefix).
pub fn posting_at_err_prefix(t: &Transaction, i: usize) -> String {
    let (f, l, ex) = posting_error_excerpt_by_index(
        &transaction_as_written(t),
        as_written_posting_index(t, i),
        None,
    );
    format!("{f}:{l}:\n{ex}\n")
}

/// The error prefix for a posting (postingErrPrefix): its transaction as
/// it was when classification last tied the postings to it (`tied`),
/// numbered at the first posting equal to it without costs, if there is
/// one (a posting that has gained a commodity tag since then is not).
fn posting_err_prefix(tied: &Transaction, p: &Posting) -> String {
    let costless = |q: &Posting| Posting {
        amount: q.amount.clone().strip_costs(),
        ..q.clone()
    };
    let p = costless(p);
    match tied.postings.iter().position(|q| costless(q) == p) {
        Some(k) => posting_at_err_prefix(tied, k),
        None => txn_err_prefix(tied),
    }
}

/// The error for a lotful posting that was not classified
/// (unclassifiedLotWarning).
fn unclassified_lot_warning(j: &Journal, t: &Transaction, idx: usize, p: &Posting) -> String {
    let source = match p
        .amount
        .amounts_raw()
        .iter()
        .find(|a| commodity_uses_lots(j, &a.commodity))
    {
        Some(a) => format!("{} is declared lotful (commodity lots: tag)", a.commodity),
        None => "this posting involves a lotful commodity".to_string(),
    };
    let inferred = if p.original().has_amount() {
        String::new()
    } else {
        format!(
            " (with inferred amount {})",
            show_mixed_amount_one_line(&p.amount)
        )
    };
    let (f, l, ex) = posting_error_excerpt_by_index(
        &transaction_as_written(t),
        as_written_posting_index(t, idx),
        None,
    );
    let short = if lot_direction(j, &p.account) == Some(LotDirection::Short) {
        "\n(Liability accounts hold short positions, and transferring those between\n\
         accounts is not supported yet: cover the position in one account and\n\
         open it in the other, with prices.)"
    } else {
        ""
    };
    format!(
        "{f}:{l}:\n{ex}\n{source} but this posting{inferred} was not classified as\n\
         acquire, dispose, or transfer. Lot state will not be updated.\n\
         Possible fixes: add a cost basis ({{$X}}), a price (@ $X),\n\
         or check the account type declaration.{short}"
    )
}

/// The units and total cost basis of each AVERAGE or AVERAGEALL lot pool,
/// from the lot postings dated before the given date (if any), keyed by
/// base account and commodity; pools with no units, or bases in several
/// commodities, left out (journalAveragePoolTotals).
pub fn journal_average_pool_totals(
    j: &Journal,
    end: Option<Day>,
) -> BTreeMap<(AccountName, Sym), (Decimal, Amount)> {
    let lot_amounts: Vec<(&str, &Amount)> = j
        .postings()
        .filter(|r| lot_subaccount_name(&r.p.account).is_some())
        .filter(|r| end.is_none_or(|e| r.date() < e))
        .flat_map(|r| {
            r.p.amount
                .amounts_raw()
                .iter()
                .map(move |a| (&*r.p.account, a))
        })
        .collect();
    let pools: BTreeSet<(&str, &Sym)> = lot_amounts
        .iter()
        .map(|(sub, a)| (lot_base_account(sub), &a.commodity))
        .collect();
    let mut totals = BTreeMap::new();
    for (base, c) in pools {
        let method = resolve_reduction_method(j, base, c).0;
        if !method.is_average() {
            continue;
        }
        let in_pool = |sub: &str| {
            if method.is_global() {
                lot_direction_or_long(j, lot_base_account(sub)) == lot_direction_or_long(j, base)
            } else {
                lot_base_account(sub) == base
            }
        };
        let entries: Vec<(&Amount, &Amount)> = lot_amounts
            .iter()
            .filter(|(sub, a)| &a.commodity == c && in_pool(sub))
            .filter_map(|(_, a)| Some((*a, a.cost_basis.as_ref()?.cost.as_deref()?)))
            .collect();
        let Some((_, ub1)) = entries.first() else {
            continue;
        };
        if entries.iter().any(|(_, ub)| ub.commodity != ub1.commodity) {
            continue;
        }
        let units = entries
            .iter()
            .fold(Decimal::ZERO, |s, (a, _)| &s + &a.quantity);
        if units.is_zero() {
            continue;
        }
        let cost = entries.iter().fold(Decimal::ZERO, |s, (a, ub)| {
            &s + &(&a.quantity * &ub.quantity)
        });
        let total = Amount {
            quantity: cost,
            ..(*ub1).clone()
        };
        totals.insert((AccountName::from(base), *c), (units, total));
    }
    totals
}

/// The average cost of each AVERAGE or AVERAGEALL lot pool, just before
/// the given date (journalAveragePoolCosts).
pub fn journal_average_pool_costs(
    j: &Journal,
    end: Option<Day>,
) -> BTreeMap<(AccountName, Sym), Amount> {
    journal_average_pool_totals(j, end)
        .into_iter()
        .map(|(k, (units, cost))| (k, cost.divide_and_update_precision(&units)))
        .collect()
}

/// The journal without lot detail, for reports without `--lots`: lot
/// parent assertion postings dropped, lot subaccounts removed from account
/// names, and a disposal's or transfer's per-lot fragments merged back into
/// one posting (journalCollapseLotDetail).
pub fn journal_collapse_lot_detail(j: &mut Journal) {
    use crate::model::FEESPLIT_POSTING_TAG;
    let needs_collapse = |p: &Posting| {
        p.has_tag(FEESPLIT_POSTING_TAG)
            || p.has_tag(LOTSPLIT_POSTING_TAG)
            || p.has_tag(LOT_PARENT_ASSERTION_TAG)
            || lot_subaccount_name(&p.account).is_some()
    };
    // the transactions with none of those are left as they are
    for t in &mut j.txns {
        if !t.postings.iter().any(needs_collapse) {
            continue;
        }
        let ps: Vec<Posting> = std::mem::take(&mut t.postings)
            .into_iter()
            .filter(|p| !p.has_tag(LOT_PARENT_ASSERTION_TAG))
            .map(|mut p| {
                let base = lot_base_account(&p.account);
                if base.len() != p.account.len() {
                    let new = AccountName::from(base);
                    if p.original().account != new {
                        p.assertion = None;
                    }
                    p.account = new;
                }
                p
            })
            .collect();
        t.postings = merge_lot_splits(ps);
    }
}

/// Consecutive per-lot fragments of one posting (sharing its original)
/// merged into one, with their amounts summed, and a cost basis of the
/// parts on which their lots agree (mergeLotSplits).
fn merge_lot_splits(ps: Vec<Posting>) -> Vec<Posting> {
    let mut out: Vec<Posting> = Vec::new();
    let mut it = ps.into_iter().peekable();
    while let Some(mut p) = it.next() {
        if !p.has_tag(LOTSPLIT_POSTING_TAG) {
            out.push(p);
            continue;
        }
        let mut run = Vec::new();
        while let Some(q) =
            it.next_if(|q| q.has_tag(LOTSPLIT_POSTING_TAG) && q.original == p.original)
        {
            run.push(q);
        }
        let merged = std::iter::once(&p)
            .chain(&run)
            .fold(MixedAmount::null(), |s, q| &s + &q.amount);
        let cbs: Vec<CostBasis> = std::iter::once(&p)
            .chain(&run)
            .flat_map(|q| q.amount.amounts_raw())
            .filter_map(|a| a.cost_basis.as_deref().cloned())
            .collect();
        p.tags.retain(|(n, _)| n != LOTSPLIT_POSTING_TAG);
        p.amount = if run.is_empty() || cbs.is_empty() {
            merged
        } else {
            fn agreed<T: Clone + PartialEq>(vs: impl Iterator<Item = Option<T>>) -> Option<T> {
                let vs: Vec<Option<T>> = vs.collect();
                match vs.split_first() {
                    Some((v, rest)) if rest.iter().all(|x| x == v) => v.clone(),
                    _ => None,
                }
            }
            let common = CostBasis {
                date: agreed(cbs.iter().map(|cb| cb.date)),
                label: agreed(cbs.iter().map(|cb| cb.label.clone())),
                cost: agreed(cbs.iter().map(|cb| cb.cost.clone())),
            };
            merged.map(|a| Amount {
                cost_basis: Some(Box::new(common.clone())),
                ..a.clone()
            })
        };
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amt(s: &str) -> Option<Amount> {
        crate::read::amount::parse_amount(s).ok()
    }

    #[test]
    fn lot_name_parts_keep_commas_in_labels_and_costs() {
        // the examples in splitParts' documentation
        assert_eq!(
            split_lot_name_parts("2026-01-15, \"my, label\", €1,50"),
            ["2026-01-15", "\"my, label\"", "€1,50"]
        );
        assert_eq!(
            split_lot_name_parts("2026-01-15, \"an, odd, commodity\" 1,5"),
            ["2026-01-15", "\"an, odd, commodity\" 1,5"]
        );
        assert_eq!(
            split_lot_name_parts("2026-01-15, \"a, b\", \"an, odd, commodity\" 1,5"),
            ["2026-01-15", "\"a, b\"", "\"an, odd, commodity\" 1,5"]
        );
        assert_eq!(split_lot_name_parts("$100"), ["$100"]);
    }

    #[test]
    fn lot_names_read_back_as_written() {
        for name in [
            "{2026-01-15, \"lot A\", $50}",
            "{2026-01-15, $50}",
            "{$50}",
            "{2026-01-15}",
        ] {
            let cb = parse_lot_name(amt, name).unwrap();
            assert_eq!(show_lot_name(&cb), name);
        }
        assert_eq!(parse_lot_name(amt, "{}").unwrap(), CostBasis::default());
    }

    #[test]
    fn lot_name_errors() {
        assert_eq!(
            parse_lot_name(amt, "x").unwrap_err(),
            "lot name must be enclosed in braces: x"
        );
        assert_eq!(
            parse_lot_name(amt, "{2026-02-30, $50}").unwrap_err(),
            "invalid date in lot name: 2026-02-30"
        );
        assert_eq!(
            parse_lot_name(amt, "{2026-01-15 $10}").unwrap_err(),
            "Missing comma in lot name: 2026-01-15 $10\nDid you mean:              2026-01-15, $10 ?"
        );
        assert_eq!(
            parse_lot_name(amt, "{$10, \"note\"}").unwrap_err(),
            "lot name parts must be in this order (if present): {DATE, \"LABEL\", COST}: {$10, \"note\"}"
        );
        assert_eq!(
            parse_lot_name(amt, "{not a lot}").unwrap_err(),
            "invalid lot name: not a lot"
        );
    }

    #[test]
    fn lot_costs_match_when_rounded() {
        let third = Amount {
            quantity: &Decimal::from_i64(10) / &Decimal::from_i64(3),
            ..amt("$1").unwrap()
        };
        assert!(lot_costs_match(&amt("$3.33333333").unwrap(), &third));
        assert!(lot_costs_match(&amt("$3.33").unwrap(), &third));
        assert!(!lot_costs_match(&amt("$3.34").unwrap(), &third));
        assert!(!lot_costs_match(&amt("3.33 EUR").unwrap(), &third));
    }

    #[test]
    fn cost_bases_merge_or_conflict() {
        let a = parse_lot_name(amt, "{2026-01-15, $50}").unwrap();
        let b = parse_lot_name(amt, "{\"x\"}").unwrap();
        assert_eq!(
            show_lot_name(&merge_cost_basis(&a, &b).unwrap()),
            "{2026-01-15, \"x\", $50}"
        );
        let c = parse_lot_name(amt, "{2026-01-16}").unwrap();
        assert_eq!(
            merge_cost_basis(&a, &c).unwrap_err(),
            "conflicting cost basis date: account name has 2026-01-15 but amount has 2026-01-16"
        );
    }

    #[test]
    fn reduction_methods() {
        assert_eq!(
            ReductionMethod::parse(" fifoall "),
            Some(ReductionMethod::FifoAll)
        );
        assert_eq!(ReductionMethod::parse("NONE"), None);
        assert_eq!(ReductionMethod::AverageAll.to_string(), "AVERAGEALL");
        assert_eq!(
            ReductionMethod::HifoAll.base_ordering(),
            ReductionMethod::Hifo
        );
        assert!(is_none_lots_tag_value(" none"));
    }
}
