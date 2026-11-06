//! The journal and what it contains: transactions, postings, declarations,
//! prices (the data types of Hledger.Data.Types, with the helpers of
//! Hledger.Data.Posting and Hledger.Data.Transaction that do not render).
//!
//! Postings do not point back to their transaction. Code that needs both
//! iterates them as pairs, or as [`PostingRef`]s; `Posting::txn` records the
//! position of a journal posting's transaction in `Journal::txns`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::account::{AccountName, AccountType, Realness};
use crate::amount::currency::to_currency_code;
use crate::amount::{Amount, AmountStyle, CommodityStyles, MixedAmount, Sym};
use crate::dates::{DateSpan, Day, Interval};
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

pub use crate::dates::null_date;

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
    /// How often it recurs, and in what span.
    pub interval: Interval,
    pub span: DateSpan,
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

/// A posting's value of a field: code, desc or description, payee, note,
/// status, acct or account, cur or comm (its one commodity), amt (its one
/// quantity), cost (its one amount's cost); or else of a tag, its own or
/// its transaction's, a `type` tag's value shortened to its one-letter
/// code; or else nothing (pivotComponent).
fn pivot_component(name: &str, t: &Transaction, p: &Posting) -> String {
    let one_amount = || match p.amount.amounts().as_slice() {
        [a] => Some(a.clone()),
        _ => None,
    };
    match name {
        "code" => t.code.clone(),
        "desc" | "description" => t.description.clone(),
        "payee" => t.payee().to_string(),
        "note" => t.note().to_string(),
        "status" => t.status.to_string(),
        "acct" | "account" => p.account.to_string(),
        "cur" | "comm" => one_amount().map_or_else(String::new, |a| a.commodity.to_string()),
        "amt" => one_amount().map_or_else(String::new, |a| a.quantity.to_string()),
        "cost" => match one_amount() {
            Some(a) if a.cost.is_some() => {
                let fmt = &crate::amount::render::AmountFormat::DEFAULT;
                let cost = crate::amount::render::show_amount_cost_b(fmt, &a).s;
                cost.trim_start().to_string()
            }
            _ => String::new(),
        },
        _ => match p.tags.iter().chain(&t.tags).find(|(n, _)| n == name) {
            Some((_, v)) if name == "type" => {
                AccountType::parse(true, v).map_or_else(|_| v.clone(), |at| at.letter().to_string())
            }
            Some((_, v)) => v.clone(),
            None => String::new(),
        },
    }
}

/// Each commodity alias, with its commodity, the number of alias units in
/// one unit of the commodity, and the commodity's style if known
/// (CommodityAliases).
pub type CommodityAliases = BTreeMap<Sym, (Sym, Decimal, Option<Arc<AmountStyle>>)>;

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
    /// The files read, with their text, the main file first, then the
    /// included files, each before the files it includes.
    pub files: Vec<(String, String)>,
    /// The default commodity and style of the D directive in effect at the
    /// end of parsing (jparsedefaultcommodity), whose style counts as a
    /// declared one.
    pub default_commodity: Option<(Sym, Arc<AmountStyle>)>,
    /// The decimal mark and the default year in effect at the end of
    /// parsing (jparsedecimalmark, jparsedefaultyear), which add's amounts
    /// are read with.
    pub parse_decimal_mark: Option<char>,
    pub parse_year: Option<i64>,
    /// When the files were read (jlastreadtime).
    pub last_read_time: Option<std::time::SystemTime>,
    /// Other files the data came from, such as CSV rules files, whose
    /// changes repl notices (jauxfiles).
    pub aux_files: Vec<String>,
}

impl Journal {
    /// The postings of all transactions, with their transactions.
    pub fn postings(&self) -> impl Iterator<Item = PostingRef<'_>> {
        self.txns
            .iter()
            .flat_map(|t| t.postings.iter().map(move |p| PostingRef::new(t, p)))
    }

    /// The display style of each commodity: given on the command line, or
    /// else declared by a commodity or D directive, or else (for a
    /// commodity alias) its commodity's with its own symbol placement, or
    /// else inferred (journalCommodityStyles).
    pub fn commodity_styles(&self) -> CommodityStyles {
        let mut explicit = self.global_commodity_styles.clone();
        for (sym, c) in &self.declared_commodities {
            if let Some(s) = &c.format {
                explicit
                    .entry(sym.clone())
                    .or_insert_with(|| Arc::new(s.clone()));
            }
        }
        if let Some((sym, s)) = &self.default_commodity {
            explicit.entry(sym.clone()).or_insert_with(|| s.clone());
        }
        let inferred = &self.inferred_commodity_styles;
        let mut styles = explicit.clone();
        for c in self.declared_commodities.values() {
            let Some(cst) = explicit.get(&c.symbol).or_else(|| inferred.get(&c.symbol)) else {
                continue;
            };
            for (a, _) in &c.aliases {
                let pst = inferred.get(&a.commodity).unwrap_or(cst);
                styles.entry(a.commodity.clone()).or_insert_with(|| {
                    Arc::new(AmountStyle {
                        side: pst.side,
                        spaced: pst.spaced,
                        ..(**cst).clone()
                    })
                });
            }
        }
        for (k, v) in inferred {
            styles.entry(k.clone()).or_insert_with(|| v.clone());
        }
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

    /// The span of the transactions' and postings' dates, primary or
    /// secondary, from the earliest to the day after the latest
    /// (journalDateSpan).
    pub fn date_span(&self, secondary: bool) -> DateSpan {
        let dates = self.txns.iter().flat_map(|t| {
            let tdate = if secondary {
                t.date2.unwrap_or(t.date)
            } else {
                t.date
            };
            let pdates = t.postings.iter().filter_map(move |p| {
                if secondary {
                    p.date2.or(p.date)
                } else {
                    p.date
                }
            });
            std::iter::once(tdate).chain(pdates)
        });
        let (min, max) = dates.fold((None, None), |(lo, hi): (Option<Day>, Option<Day>), d| {
            (
                Some(lo.map_or(d, |l| l.min(d))),
                Some(hi.map_or(d, |h| h.max(d))),
            )
        });
        DateSpan::exact(min, max.map(|d| d.add_days(1)))
    }

    /// The posting amounts and their cost amounts
    /// (journalPostingAndCostAmounts).
    pub fn posting_and_cost_amounts(&self) -> impl Iterator<Item = &Amount> {
        self.txns
            .iter()
            .flat_map(|t| &t.postings)
            .flat_map(|p| p.amount.amounts_raw())
            .flat_map(Amount::and_cost)
    }

    /// The commodities of the P directives, priced and pricing
    /// (journalCommoditiesFromPriceDirectives).
    pub fn commodities_from_price_directives(&self) -> BTreeSet<Sym> {
        self.price_directives
            .iter()
            .flat_map(|pd| [pd.commodity.clone(), pd.amount.commodity.clone()])
            .collect()
    }

    /// The commodities of the posting amounts and their costs
    /// (journalCommoditiesFromTransactions).
    pub fn commodities_from_transactions(&self) -> BTreeSet<Sym> {
        self.posting_and_cost_amounts()
            .map(|a| a.commodity.clone())
            .collect()
    }

    /// The commodities used: in P directives, the last D directive, and
    /// posting and cost amounts (journalCommoditiesUsed).
    pub fn commodities_used(&self) -> BTreeSet<Sym> {
        let mut cs = self.commodities_from_price_directives();
        cs.extend(self.default_commodity.iter().map(|(c, _)| c.clone()));
        cs.extend(self.commodities_from_transactions());
        cs
    }

    /// The commodities mentioned anywhere: declared, or used
    /// (journalCommodities).
    pub fn commodities(&self) -> BTreeSet<Sym> {
        let mut cs: BTreeSet<Sym> = self.declared_commodities.keys().cloned().collect();
        cs.extend(self.commodities_used());
        cs
    }

    /// The guessed base currency: among the P directives' price
    /// commodities, or else the posting and cost amounts' commodities, the
    /// first with the most frequent ISO 4217 code (the first seen winning
    /// ties), and that code (journalBaseCurrency).
    pub fn base_currency(&self) -> Option<(&str, &str)> {
        fn pick(syms: Vec<&str>) -> Option<(&str, &str)> {
            let mut stats: BTreeMap<&str, (isize, usize)> = BTreeMap::new();
            for (i, s) in syms.iter().enumerate() {
                stats.entry(to_currency_code(s)).or_insert((0, i)).0 -= 1;
            }
            let code = stats.into_iter().min_by_key(|(_, s)| *s)?.0;
            let sym = syms.into_iter().find(|s| to_currency_code(s) == code)?;
            Some((sym, code))
        }
        let price_targets = self.price_directives.iter().map(|pd| &*pd.amount.commodity);
        pick(price_targets.collect()).or_else(|| {
            pick(
                self.posting_and_cost_amounts()
                    .map(|a| &*a.commodity)
                    .collect(),
            )
        })
    }

    /// The ISO 4217 code of the guessed base currency, or USD
    /// (journalBaseCurrencyCode).
    pub fn base_currency_code(&self) -> &str {
        self.base_currency().map_or("USD", |(_, c)| c)
    }

    /// Replace the postings' account names with their values of a field or
    /// tag, or of several joined by `:` (`acct:kind:project`), keeping the
    /// postings as they were as their originals (journalPivot,
    /// transactionPivot, postingPivot, pivotAccount).
    pub fn pivot(&mut self, field_or_tag: &str) {
        for t in &mut self.txns {
            let accounts: Vec<String> = t
                .postings
                .iter()
                .map(|p| {
                    field_or_tag
                        .split(':')
                        .map(|name| pivot_component(name, t, p))
                        .collect::<Vec<_>>()
                        .join(":")
                })
                .collect();
            for (p, a) in t.postings.iter_mut().zip(accounts) {
                p.original = Some(Box::new(p.original().clone()));
                p.account = a.into();
            }
        }
    }

    /// The span of all the transactions' and postings' dates, primary and
    /// secondary, from the earliest to the day after the latest
    /// (journalDateSpanBothDates).
    pub fn date_span_both_dates(&self) -> DateSpan {
        crate::dates::days_span(self.txns.iter().flat_map(|t| {
            let pdates = t
                .postings
                .iter()
                .flat_map(|p| p.date.into_iter().chain(p.date2));
            std::iter::once(t.date).chain(t.date2).chain(pdates)
        }))
    }

    /// The tags declared for an account and its parents, its own first
    /// (journalInheritedAccountTags).
    pub fn inherited_account_tags(&self, a: &str) -> Vec<Tag> {
        let mut tags: Vec<Tag> = Vec::new();
        for name in std::iter::once(a).chain(crate::account::parent_account_names(a)) {
            union_tags(
                &mut tags,
                self.declared_account_tags
                    .get(name)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        tags
    }

    /// The account to use for postings hledger generates to an account of
    /// this type: the first declared with it, or else the alphabetically
    /// first having it, or else the default (journalAccountForType).
    pub fn account_for_type<'a>(&'a self, default: &'a str, atype: AccountType) -> &'a str {
        let declared = |a: &str| {
            self.declared_account_types
                .iter()
                .find(|(_, accts)| accts.iter().any(|b| &**b == a))
                .map(|(t, _)| *t)
        };
        self.declared_account_names()
            .find(|a| declared(a) == Some(atype))
            .or_else(|| {
                self.account_types
                    .iter()
                    .find(|(_, t)| **t == atype)
                    .map(|(a, _)| a)
            })
            .map_or(default, |a| &**a)
    }

    /// The accounts declared or inferred to be conversion accounts
    /// (journalConversionAccounts).
    pub fn conversion_accounts(&self) -> Vec<AccountName> {
        self.account_types
            .iter()
            .filter(|(_, t)| **t == AccountType::Conversion)
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// The commodity aliases declared with `alias:` tags on commodity
    /// directives: each alias symbol, with its commodity, the number of its
    /// units in one of the commodity, and the commodity's display style if
    /// known (journalCommodityAliases).
    pub fn commodity_aliases(&self) -> CommodityAliases {
        let styles = self.commodity_styles();
        let mut aliases = CommodityAliases::new();
        for c in self.declared_commodities.values() {
            for (a, _) in &c.aliases {
                let style = styles.get(&c.symbol).cloned();
                aliases.insert(
                    a.commodity.clone(),
                    (c.symbol.clone(), a.quantity.clone(), style),
                );
            }
        }
        aliases
    }

    /// The commodities declared with a `lots:` tag (journalLotfulCommodities).
    pub fn lotful_commodities(&self) -> std::collections::BTreeSet<Sym> {
        self.declared_commodity_tags
            .iter()
            .filter(|(_, tags)| tags.iter().any(|(n, _)| n.to_lowercase() == "lots"))
            .map(|(c, _)| c.clone())
            .collect()
    }

    /// Each account's declared `lots:` tag value, the first if there are
    /// several (journalAccountLotsTags).
    pub fn account_lots_tags(&self) -> BTreeMap<AccountName, String> {
        self.declared_account_tags
            .iter()
            .filter_map(|(a, tags)| {
                let v = tags.iter().find(|(n, _)| n.to_lowercase() == "lots")?;
                Some((a.clone(), v.1.clone()))
            })
            .collect()
    }
}

pub(crate) mod tests {
    use super::*;

    // tests_Posting "commentAddTag", "commentAddTagNextLine".
    #[cfg_attr(test, test)]
    pub(crate) fn comment_tags() {
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

    // tests_Journal "journalDateSpan".
    #[cfg_attr(test, test)]
    pub(crate) fn journal_date_span() {
        let day = |y, m, d| Day::from_ymd(y, m, d).unwrap();
        let j = Journal {
            txns: vec![
                Transaction {
                    date: day(2014, 2, 1),
                    postings: vec![Posting {
                        date: Some(day(2014, 1, 10)),
                        ..Posting::default()
                    }],
                    ..Transaction::default()
                },
                Transaction {
                    date: day(2014, 9, 1),
                    postings: vec![Posting {
                        date2: Some(day(2014, 10, 10)),
                        ..Posting::default()
                    }],
                    ..Transaction::default()
                },
            ],
            ..Journal::default()
        };
        assert_eq!(
            j.date_span(true),
            DateSpan::exact(Some(day(2014, 1, 10)), Some(day(2014, 10, 11)))
        );
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
