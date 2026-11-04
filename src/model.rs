//! The journal and what it contains: transactions, postings, declarations,
//! prices (the data types of Hledger.Data.Types, with the helpers of
//! Hledger.Data.Posting and Hledger.Data.Transaction that do not render).
//!
//! Postings do not point back to their transaction. Code that needs both
//! iterates them as pairs, or as [`PostingRef`]s; `Posting::txn` records the
//! position of a journal posting's transaction in `Journal::txns`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::account::{AccountName, AccountType, Realness};
use crate::amount::{Amount, AmountStyle, CommodityStyles, MixedAmount, Sym};
use crate::dates::Day;
use crate::decimal::Decimal;

/// A position in a source file: its name, and a 1-based line and column.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePos {
    pub file: Arc<str>,
    pub line: usize,
    pub col: usize,
}

impl SourcePos {
    pub fn new(file: &Arc<str>, line: usize, col: usize) -> SourcePos {
        SourcePos {
            file: file.clone(),
            line,
            col,
        }
    }

    /// The start of an unnamed source (initialPos "").
    pub fn null() -> SourcePos {
        SourcePos {
            file: Arc::from(""),
            line: 1,
            col: 1,
        }
    }
}

/// `FILE:LINE:COL` (sourcePosPretty).
impl fmt::Display for SourcePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.file.is_empty() {
            write!(f, "{}:{}", self.line, self.col)
        } else {
            write!(f, "{}:{}:{}", self.file, self.line, self.col)
        }
    }
}

/// `FILE:LINE` or `FILE:LINE1-LINE2` for a span of lines, the end position
/// being just after the last line (sourcePosPairPretty).
pub fn source_pos_pair_pretty((start, end): &(SourcePos, SourcePos)) -> String {
    let l1 = start.line;
    let l2 = if end.col == 1 {
        end.line.saturating_sub(1)
    } else {
        end.line
    };
    if l2 > l1 {
        format!("{}:{l1}-{l2}", start.file)
    } else {
        format!("{}:{l1}", start.file)
    }
}

/// A transaction's or posting's clearing status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    #[default]
    Unmarked,
    Pending,
    Cleared,
}

/// Nothing, `!` or `*`.
impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Status::Unmarked => "",
            Status::Pending => "!",
            Status::Cleared => "*",
        })
    }
}

/// The date used where there is none: 0000-01-01 (nulldate).
pub fn null_date() -> Day {
    Day::from_ymd(0, 1, 1).expect("a valid date")
}

/// A tag's name and (possibly empty) value. Names beginning with `_` are
/// hidden tags, which hledger adds itself.
pub type Tag = (String, String);

pub const GENERATED_TRANSACTION_TAG: &str = "_generated-transaction";
pub const MODIFIED_TRANSACTION_TAG: &str = "_modified-transaction";
pub const GENERATED_POSTING_TAG: &str = "_generated-posting";
pub const COST_POSTING_TAG: &str = "_cost-posting";
pub const CONVERSION_POSTING_TAG: &str = "_conversion-posting";
pub const IMBALANCE_POSTING_TAG: &str = "_imbalance-posting";
pub const FEESPLIT_POSTING_TAG: &str = "_feesplit-posting";
pub const LOTSPLIT_POSTING_TAG: &str = "_lotsplit-posting";
pub const LOT_PARENT_ASSERTION_TAG: &str = "_lot-parent-assertion";

pub fn is_hidden_tag_name(name: &str) -> bool {
    name.starts_with('_')
}

/// The visible form of a hidden tag: without its leading `_`.
fn to_visible_tag((name, value): &Tag) -> Tag {
    (name.chars().skip(1).collect(), value.clone())
}

/// These tags added to a list of tags, those already present skipped
/// (Data.List's union).
fn union_tags(tags: &mut Vec<Tag>, new: impl IntoIterator<Item = Tag>) {
    for t in new {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
}

/// An expected balance in an account after a posting: of the amount's
/// commodity only, or with `total`, of all commodities; excluding
/// subaccounts, or with `inclusive`, including them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceAssertion {
    pub amount: Amount,
    pub total: bool,
    pub inclusive: bool,
    pub pos: SourcePos,
}

/// A posting: an amount moved to or from an account within a transaction.
/// Equality ignores `txn` and `original`, as upstream's does.
#[derive(Clone, Debug, Default)]
pub struct Posting {
    /// The position of this posting's transaction in its journal's
    /// transactions, once it is in a journal.
    pub txn: Option<u32>,
    /// This posting's date, if it differs from the transaction's.
    pub date: Option<Day>,
    pub date2: Option<Day>,
    pub status: Status,
    pub account: AccountName,
    pub amount: MixedAmount,
    /// The comment lines, without their `;`, joined by newlines.
    pub comment: String,
    pub realness: Realness,
    pub tags: Vec<Tag>,
    pub assertion: Option<Box<BalanceAssertion>>,
    /// The posting as it was before hledger changed it (inferring its amount,
    /// applying a rule), if it was.
    pub original: Option<Box<Posting>>,
}

impl PartialEq for Posting {
    fn eq(&self, o: &Posting) -> bool {
        self.date == o.date
            && self.date2 == o.date2
            && self.status == o.status
            && self.account == o.account
            && self.amount == o.amount
            && self.comment == o.comment
            && self.realness == o.realness
            && self.tags == o.tags
            && self.assertion == o.assertion
    }
}

impl Posting {
    /// A posting of this amount to this account (post).
    pub fn new(account: &str, amount: Amount) -> Posting {
        Posting {
            account: AccountName::from(account),
            amount: MixedAmount::from_amount(amount),
            ..Posting::default()
        }
    }

    /// The posting before hledger changed it (originalPosting).
    pub fn original(&self) -> &Posting {
        self.original.as_deref().unwrap_or(self)
    }

    pub fn is_real(&self) -> bool {
        self.realness == Realness::Real
    }

    pub fn is_virtual(&self) -> bool {
        self.realness == Realness::Virtual
    }

    pub fn is_balanced_virtual(&self) -> bool {
        self.realness == Realness::BalancedVirtual
    }

    /// Whether the amount was written or has been inferred, rather than
    /// being missing (hasAmount).
    pub fn has_amount(&self) -> bool {
        !self.amount.is_missing()
    }

    /// Whether the original posting had an amount or a balance assignment
    /// (isExplicitAmount).
    pub fn is_explicit_amount(&self) -> bool {
        let o = self.original();
        !o.amount.is_missing() || o.assertion.is_some()
    }

    /// No amount, but a balance assertion: a balance assignment
    /// (hasBalanceAssignment).
    pub fn has_balance_assignment(&self) -> bool {
        !self.has_amount() && self.assertion.is_some()
    }

    pub fn has_tag(&self, name: &str) -> bool {
        self.tags.iter().any(|(n, _)| n == name)
    }

    /// The posting's own date, or its transaction's (postingDate).
    pub fn date_in(&self, t: Option<&Transaction>) -> Day {
        self.date.or(t.map(|t| t.date)).unwrap_or_else(null_date)
    }

    /// The posting's secondary date, or its transaction's, or its primary
    /// date (postingDate2).
    pub fn date2_in(&self, t: Option<&Transaction>) -> Day {
        self.date2
            .or(t.and_then(|t| t.date2))
            .or(self.date)
            .or(t.map(|t| t.date))
            .unwrap_or_else(null_date)
    }

    /// The posting's status, or if unmarked, its transaction's
    /// (postingStatus).
    pub fn status_in(&self, t: Option<&Transaction>) -> Status {
        match self.status {
            Status::Unmarked => t.map_or(Status::Unmarked, |t| t.status),
            s => s,
        }
    }

    /// With these tags added, those already present skipped (postingAddTags).
    pub fn add_tags(&mut self, tags: impl IntoIterator<Item = Tag>) {
        union_tags(&mut self.tags, tags);
    }

    /// With a hidden tag added, and with `verbose_tags`, its visible form
    /// too, which is also written in the comment (at its start if `prepend`)
    /// unless a tag of that name is there already
    /// (postingAddHiddenAndMaybeVisibleTag).
    pub fn add_hidden_and_maybe_visible_tag(&mut self, prepend: bool, verbose_tags: bool, ht: Tag) {
        let vt = to_visible_tag(&ht);
        let had = self
            .tags
            .iter()
            .any(|(n, _)| n.to_lowercase() == vt.0.to_lowercase());
        let mut new = vec![ht];
        if verbose_tags {
            new.push(vt.clone());
        }
        self.add_tags(new);
        if verbose_tags && !had {
            self.comment = if prepend {
                comment_prepend_tag(&self.comment, &vt)
            } else {
                comment_add_tag(&self.comment, &vt)
            };
        }
    }

    /// With amounts and balance assertion amounts negated (postingNegate).
    pub fn negate(&mut self) {
        self.amount = -std::mem::take(&mut self.amount);
        if let Some(a) = &mut self.assertion {
            a.amount = -&a.amount;
        }
    }

    /// The amount and any balance assertion amount styled with these
    /// commodity styles (styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        self.amount = std::mem::take(&mut self.amount).styled(styles);
        if let Some(a) = &mut self.assertion {
            a.amount = a.amount.clone().styled(styles);
        }
    }

    /// The commodities of the amounts, without the missing amount marker
    /// (postingCommodities).
    pub fn commodities(&self) -> Vec<Sym> {
        self.amount
            .amounts_raw()
            .iter()
            .filter(|a| !a.is_missing())
            .map(|a| a.commodity.clone())
            .collect()
    }
}

/// Two comments joined with a comma (commentJoin).
pub fn comment_join(c1: &str, c2: &str) -> String {
    match (c1.is_empty(), c2.is_empty()) {
        (true, _) => c2.to_string(),
        (_, true) => c1.to_string(),
        _ => format!("{c1}, {c2}"),
    }
}

/// A comment with a `NAME: VALUE` tag added at the end of its first line
/// (commentAddTag).
pub fn comment_add_tag(c: &str, (name, value): &Tag) -> String {
    comment_join(
        c.trim_end_matches(crate::text::is_space),
        &format!("{name}: {value}"),
    )
}

/// A comment with a tag added at its start (commentPrependTag).
pub fn comment_prepend_tag(c: &str, (name, value): &Tag) -> String {
    comment_join(
        &format!("{name}: {value}"),
        c.trim_end_matches(crate::text::is_space),
    )
}

/// A comment with a `NAME:VALUE` tag added at the end (commentAddTagUnspaced).
pub fn comment_add_tag_unspaced(c: &str, (name, value): &Tag) -> String {
    comment_join(
        c.trim_end_matches(crate::text::is_space),
        &format!("{name}:{value}"),
    )
}

/// A comment with a tag added on a new line (commentAddTagNextLine).
pub fn comment_add_tag_next_line(c: &str, (name, value): &Tag) -> String {
    let nl = if c.ends_with('\n') { "" } else { "\n" };
    format!("{c}{nl}{name}: {value}")
}

/// A transaction: a dated movement of amounts between accounts.
#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    /// The 1-based position in the journal's transactions, or 0.
    pub index: u32,
    /// Comment lines immediately before it.
    pub preceding_comment: String,
    /// Where the date starts, and where the last posting ends.
    pub pos: (SourcePos, SourcePos),
    pub date: Day,
    pub date2: Option<Day>,
    pub status: Status,
    pub code: String,
    pub description: String,
    /// The comment lines, without their `;`, joined by newlines.
    pub comment: String,
    pub tags: Vec<Tag>,
    pub postings: Vec<Posting>,
}

impl Default for Transaction {
    /// nulltransaction.
    fn default() -> Transaction {
        Transaction {
            index: 0,
            preceding_comment: String::new(),
            pos: (
                SourcePos::null(),
                SourcePos {
                    line: 2,
                    ..SourcePos::null()
                },
            ),
            date: null_date(),
            date2: None,
            status: Status::Unmarked,
            code: String::new(),
            description: String::new(),
            comment: String::new(),
            tags: Vec::new(),
            postings: Vec::new(),
        }
    }
}

impl Transaction {
    /// The payee: the description up to a `|`, or all of it (transactionPayee).
    pub fn payee(&self) -> &str {
        payee_and_note_from_description(&self.description).0
    }

    /// The note: the description after a `|`, or all of it (transactionNote).
    pub fn note(&self) -> &str {
        payee_and_note_from_description(&self.description).1
    }

    /// The secondary date, or the primary (transactionDate2).
    pub fn date2_or_date(&self) -> Day {
        self.date2.unwrap_or(self.date)
    }

    /// The primary or the secondary date (transactionDateOrDate2).
    pub fn date_of(&self, secondary: bool) -> Day {
        if secondary {
            self.date2_or_date()
        } else {
            self.date
        }
    }

    pub fn real_postings(&self) -> impl Iterator<Item = &Posting> {
        self.postings.iter().filter(|p| p.is_real())
    }

    /// The transaction's tags and its postings' (transactionAllTags).
    pub fn all_tags(&self) -> impl Iterator<Item = &Tag> {
        self.tags
            .iter()
            .chain(self.postings.iter().flat_map(|p| &p.tags))
    }

    pub fn has_tag(&self, name: &str) -> bool {
        self.tags.iter().any(|(n, _)| n == name)
    }

    /// With every posting's amounts negated (transactionNegate).
    pub fn negate(&mut self) {
        for p in &mut self.postings {
            p.negate();
        }
    }

    /// With these tags added, those already present skipped
    /// (transactionAddTags).
    pub fn add_tags(&mut self, tags: impl IntoIterator<Item = Tag>) {
        union_tags(&mut self.tags, tags);
    }

    /// With a hidden tag added, and with `verbose_tags`, its visible form
    /// too, which is also written on a new comment line unless a tag of that
    /// name is there already (transactionAddHiddenAndMaybeVisibleTag).
    pub fn add_hidden_and_maybe_visible_tag(&mut self, verbose_tags: bool, ht: Tag) {
        let vt = to_visible_tag(&ht);
        let had = self
            .tags
            .iter()
            .any(|(n, _)| n.to_lowercase() == vt.0.to_lowercase());
        let mut new = vec![ht];
        if verbose_tags {
            new.push(vt.clone());
        }
        self.add_tags(new);
        if verbose_tags && !had {
            self.comment = comment_add_tag_next_line(&self.comment, &vt);
        }
    }

    /// The posting amounts styled with these commodity styles (styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        for p in &mut self.postings {
            p.style_amounts(styles);
        }
    }

    /// The commodity styles inferred from the amounts of the postings whose
    /// amounts were written (transactionCommodityStyles).
    pub fn commodity_styles(&self) -> CommodityStyles {
        let amounts: Vec<Amount> = self
            .postings
            .iter()
            .filter(|p| p.is_explicit_amount())
            .flat_map(|p| p.amount.amounts_raw().iter().cloned())
            .collect();
        crate::amount::commodity_styles_from_amounts(&amounts)
    }

    /// The file this transaction was read from (transactionFile).
    pub fn file(&self) -> &str {
        &self.pos.0.file
    }
}

/// A description split into payee and note at the first `|`, both trimmed;
/// without one, both are the whole description
/// (payeeAndNoteFromDescription).
pub fn payee_and_note_from_description(d: &str) -> (&str, &str) {
    match d.split_once('|') {
        None => (d, d),
        Some((p, n)) => (crate::text::strip(p), crate::text::strip(n)),
    }
}

/// A posting together with its transaction, if it has one: what reports and
/// queries look at.
#[derive(Clone, Copy, Debug)]
pub struct PostingRef<'a> {
    pub t: Option<&'a Transaction>,
    pub p: &'a Posting,
}

impl<'a> PostingRef<'a> {
    pub fn new(t: &'a Transaction, p: &'a Posting) -> PostingRef<'a> {
        PostingRef { t: Some(t), p }
    }

    pub fn date(&self) -> Day {
        self.p.date_in(self.t)
    }

    pub fn date2(&self) -> Day {
        self.p.date2_in(self.t)
    }

    pub fn status(&self) -> Status {
        self.p.status_in(self.t)
    }

    /// The posting's tags and its transaction's (postingAllTags).
    pub fn all_tags(&self) -> impl Iterator<Item = &'a Tag> {
        let t_tags: &'a [Tag] = self.t.map_or(&[], |t| &t.tags);
        self.p.tags.iter().chain(t_tags)
    }
}

/// An auto posting rule's posting: added as written, or with
/// `is_multiplier`, its amount multiplying the matched posting's
/// (TMPostingRule).
#[derive(Clone, Debug, PartialEq)]
pub struct TmPostingRule {
    pub posting: Posting,
    pub is_multiplier: bool,
}

/// An auto posting rule (`= QUERY` and postings) (TransactionModifier).
#[derive(Clone, Debug, PartialEq)]
pub struct TransactionModifier {
    pub query: String,
    pub rules: Vec<TmPostingRule>,
    pub pos: SourcePos,
}

/// A periodic transaction rule (`~ PERIOD` and postings), which generates
/// transactions when forecasting (PeriodicTransaction).
#[derive(Clone, Debug, PartialEq)]
pub struct PeriodicTransaction {
    /// The period expression as written.
    pub period_expr: String,
    pub pos: (SourcePos, SourcePos),
    pub status: Status,
    pub code: String,
    pub description: String,
    pub comment: String,
    pub tags: Vec<Tag>,
    pub postings: Vec<Posting>,
}

/// A market price declared with a P directive (PriceDirective).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PriceDirective {
    pub pos: SourcePos,
    pub date: Day,
    pub commodity: Sym,
    pub amount: Amount,
}

/// One unit of `from` is worth `rate` of `to` from this date (MarketPrice).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MarketPrice {
    pub date: Day,
    pub from: Sym,
    pub to: Sym,
    pub rate: Decimal,
}

/// A commodity declared with a commodity directive.
#[derive(Clone, Debug, PartialEq)]
pub struct Commodity {
    pub symbol: Sym,
    pub format: Option<AmountStyle>,
    pub comment: String,
    pub tags: Vec<Tag>,
    /// Other symbols for it, from `alias:` tags, with the number of their
    /// units that make one of this commodity, and where each was declared.
    pub aliases: Vec<(Amount, SourcePos)>,
    pub pos: SourcePos,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PayeeDeclarationInfo {
    pub comment: String,
    pub tags: Vec<Tag>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagDeclarationInfo {
    pub comment: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountDeclarationInfo {
    pub comment: String,
    pub tags: Vec<Tag>,
    /// The 1-based order of declaration, across all files.
    pub order: usize,
    pub pos: SourcePos,
}

/// A top-level item of a journal file, in order, for `print --export`
/// (JournalItem).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalItem {
    /// The transaction in `Journal::txns` starting here.
    Transaction(SourcePos),
    /// A comment line (starting with `;`, `#` or `*`).
    Comment(String),
    /// A `comment` ... `end comment` block.
    CommentBlock(String),
    /// A directive, reproduced when exporting.
    Directive(String),
    /// A directive not reproduced when exporting.
    NonExportedDirective(String),
    /// An include directive line; the included file's items follow it.
    Include(String),
    /// One or more blank lines.
    Blank,
}

/// A journal: what was read from one or more files, finalised.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    pub declared_payees: Vec<(String, PayeeDeclarationInfo)>,
    pub declared_tags: Vec<(String, TagDeclarationInfo)>,
    pub declared_accounts: Vec<(AccountName, AccountDeclarationInfo)>,
    pub declared_account_tags: BTreeMap<AccountName, Vec<Tag>>,
    pub declared_account_types: BTreeMap<AccountType, Vec<AccountName>>,
    /// The type of every account known: declared, inherited or inferred.
    pub account_types: BTreeMap<AccountName, AccountType>,
    pub declared_commodities: BTreeMap<Sym, Commodity>,
    pub declared_commodity_tags: BTreeMap<Sym, Vec<Tag>>,
    pub inferred_commodity_styles: CommodityStyles,
    /// Commodity styles given on the command line (`-c`).
    pub global_commodity_styles: CommodityStyles,
    pub price_directives: Vec<PriceDirective>,
    pub inferred_market_prices: Vec<MarketPrice>,
    pub txn_modifiers: Vec<TransactionModifier>,
    pub periodic_txns: Vec<PeriodicTransaction>,
    pub txns: Vec<Transaction>,
    pub items: Vec<JournalItem>,
    /// The files read, with their text, the main file first.
    pub files: Vec<(String, String)>,
}

impl Journal {
    /// The postings of all transactions, with their transactions.
    pub fn postings(&self) -> impl Iterator<Item = PostingRef<'_>> {
        self.txns
            .iter()
            .flat_map(|t| t.postings.iter().map(move |p| PostingRef::new(t, p)))
    }

    /// The display style of each commodity: declared, inferred, or given on
    /// the command line, the later overriding the earlier
    /// (journalCommodityStyles).
    pub fn commodity_styles(&self) -> CommodityStyles {
        let mut styles = self.inferred_commodity_styles.clone();
        for (sym, c) in &self.declared_commodities {
            if let Some(s) = &c.format {
                styles.insert(sym.clone(), Arc::new(s.clone()));
            }
        }
        styles.extend(
            self.global_commodity_styles
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        styles
    }

    /// The type of an account, if known (journalAccountType).
    pub fn account_type(&self, a: &str) -> Option<AccountType> {
        crate::account::account_name_type(&self.account_types, a)
    }

    /// The declared accounts' names, in declaration order.
    pub fn declared_account_names(&self) -> impl Iterator<Item = &AccountName> {
        self.declared_accounts.iter().map(|(a, _)| a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests_Posting "commentAddTag", "commentAddTagNextLine".
    #[test]
    fn comment_tags() {
        let a = ("a".to_string(), String::new());
        assert_eq!(comment_add_tag("", &a), "a: ");
        assert_eq!(comment_add_tag("[1/2]", &a), "[1/2], a: ");
        assert_eq!(comment_add_tag_next_line("", &a), "\na: ");
        assert_eq!(comment_add_tag_next_line("[1/2]", &a), "[1/2]\na: ");
        assert_eq!(comment_prepend_tag("x ", &a), "a: , x");
        assert_eq!(comment_add_tag_unspaced("x", &a), "x, a:");
    }

    #[test]
    fn payee_and_note() {
        assert_eq!(payee_and_note_from_description("x"), ("x", "x"));
        assert_eq!(
            payee_and_note_from_description(" p | n | m"),
            ("p", "n | m")
        );
    }

    #[test]
    fn source_positions() {
        let f: Arc<str> = Arc::from("f.j");
        let pair = (SourcePos::new(&f, 3, 1), SourcePos::new(&f, 6, 1));
        assert_eq!(source_pos_pair_pretty(&pair), "f.j:3-5");
        let pair = (SourcePos::new(&f, 3, 1), SourcePos::new(&f, 4, 1));
        assert_eq!(source_pos_pair_pretty(&pair), "f.j:3");
        let pair = (SourcePos::new(&f, 3, 1), SourcePos::new(&f, 4, 7));
        assert_eq!(source_pos_pair_pretty(&pair), "f.j:3-4");
        assert_eq!(SourcePos::new(&f, 3, 2).to_string(), "f.j:3:2");
    }

    #[test]
    fn hidden_tags() {
        let mut p = Posting::default();
        p.add_hidden_and_maybe_visible_tag(false, true, ("_x".into(), "".into()));
        assert_eq!(p.tags, [("_x".into(), "".into()), ("x".into(), "".into())]);
        assert_eq!(p.comment, "x: ");
        p.add_hidden_and_maybe_visible_tag(false, true, ("_x".into(), "".into()));
        assert_eq!(p.comment, "x: ");
        let mut t = Transaction::default();
        t.add_hidden_and_maybe_visible_tag(false, ("_g".into(), "v".into()));
        assert_eq!((t.tags.len(), t.comment.as_str()), (1, ""));
        assert_eq!(Status::Cleared.to_string(), "*");
    }
}
