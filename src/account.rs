//! Account names and account types (Hledger.Data.AccountName,
//! Hledger.Data.AccountType, and their types in Hledger.Data.Types); account
//! trees are in [`crate::reports::account_tree`].
//!
//! An account name is a string like `assets:cash:petty`, its components
//! separated by colons. A posting's account name may be wrapped in
//! parentheses or brackets to mark the posting virtual or balanced virtual.

use std::fmt;
use std::sync::Arc;

use crate::regex::{Regexp, to_regex_ci};
use crate::text::{display_width, fit_text, take_width};

/// An account name, shared by the postings and declarations that use it.
pub type AccountName = Arc<str>;

pub const ACCT_SEP: char = ':';

/// Whether a posting is real, virtual (`(a)`: not balanced), or balanced
/// virtual (`[a]`: balanced with the other balanced virtual postings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Realness {
    #[default]
    Real,
    Virtual,
    BalancedVirtual,
}

/// The type of an account, which reports use to classify it. The last five
/// are subtypes: Cash of Asset, Conversion, UnrealisedGain and Imbalance of
/// Equity, Gain of Revenue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
    Cash,
    Conversion,
    Gain,
    UnrealisedGain,
    Imbalance,
}

impl AccountType {
    pub const ALL: [AccountType; 10] = [
        AccountType::Asset,
        AccountType::Liability,
        AccountType::Equity,
        AccountType::Revenue,
        AccountType::Expense,
        AccountType::Cash,
        AccountType::Conversion,
        AccountType::Gain,
        AccountType::UnrealisedGain,
        AccountType::Imbalance,
    ];

    /// The one-letter code (Show AccountType).
    pub fn letter(self) -> &'static str {
        match self {
            AccountType::Asset => "A",
            AccountType::Liability => "L",
            AccountType::Equity => "E",
            AccountType::Revenue => "R",
            AccountType::Expense => "X",
            AccountType::Cash => "C",
            AccountType::Conversion => "V",
            AccountType::Gain => "G",
            AccountType::UnrealisedGain => "U",
            AccountType::Imbalance => "M",
        }
    }

    /// The full name (accountTypeName).
    pub fn name(self) -> &'static str {
        match self {
            AccountType::Asset => "Asset",
            AccountType::Liability => "Liability",
            AccountType::Equity => "Equity",
            AccountType::Revenue => "Revenue",
            AccountType::Expense => "Expense",
            AccountType::Cash => "Cash",
            AccountType::Conversion => "Conversion",
            AccountType::Gain => "Gain",
            AccountType::UnrealisedGain => "UnrealisedGain",
            AccountType::Imbalance => "Imbalance",
        }
    }

    /// Whether this type is the other type or one of its subtypes
    /// (isAccountSubtypeOf).
    pub fn is_subtype_of(self, other: AccountType) -> bool {
        use AccountType::*;
        self == other
            || matches!(
                (self, other),
                (Cash, Asset)
                    | (Conversion, Equity)
                    | (Gain, Revenue)
                    | (UnrealisedGain, Equity)
                    | (Imbalance, Equity)
            )
    }

    /// Whether this is an income statement type
    /// (isIncomeStatementAccountType).
    pub fn is_income_statement(self) -> bool {
        matches!(
            self,
            AccountType::Revenue | AccountType::Expense | AccountType::Gain
        )
    }

    /// An account type from its one-letter code, ignoring case, or with
    /// `long_forms`, also its full name and some other spellings
    /// (parseAccountType). The error is the text that was not recognised.
    pub fn parse(long_forms: bool, s: &str) -> Result<AccountType, String> {
        let l = s.to_lowercase();
        let found = AccountType::ALL.into_iter().find(|t| {
            t.letter().eq_ignore_ascii_case(&l) || (long_forms && t.name().to_lowercase() == l)
        });
        let other = || match l.as_str() {
            "gains" => Some(AccountType::Gain),
            "unrealised" | "unrealised-gain" | "unrealised-gains" | "unrealized"
            | "unrealizedgain" | "unrealized-gain" | "unrealized-gains" => {
                Some(AccountType::UnrealisedGain)
            }
            _ => None,
        };
        found
            .or_else(|| if long_forms { other() } else { None })
            .ok_or_else(|| s.to_string())
    }

    /// The accepted spellings, for error messages (accountTypeChoices).
    pub fn choices(long_forms: bool) -> String {
        let mut names: Vec<&str> = AccountType::ALL.iter().map(|t| t.letter()).collect();
        if long_forms {
            names.extend(AccountType::ALL.iter().map(|t| t.name()));
        }
        names.join(", ")
    }
}

impl fmt::Display for AccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.letter())
    }
}

/// An account name alias: a plain one replaces an account name or its
/// prefix, a regex one rewrites any part of a name (AccountAlias).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccountAlias {
    Basic(AccountName, AccountName),
    Regex(Regexp, String),
}

pub const DEFAULT_BASE_CONVERSION_ACCOUNT: &str = "equity:conversion";
pub const DEFAULT_GAIN_ACCOUNT: &str = "revenues:gain";
pub const DEFAULT_IMBALANCE_ACCOUNT: &str = "equity:imbalance";

fn re(src: &str) -> Regexp {
    to_regex_ci(src).expect("a valid regular expression")
}

/// The type suggested by an account's name, from its top-level component
/// (and, for some subtypes, the rest), ignoring case
/// (accountNameInferType). The subtypes are those of upstream's
/// case-insensitive, multi-line regular expressions, matched here by hand,
/// as compiling those costs milliseconds:
///
/// - Cash: `^assets?(:.+)?:(cash|bank|che(ck|que?)(ing)?|savings?|current)(:|$)`
/// - Conversion: `^equity:(trade|trades|trading|conversion)(:|$)`
/// - Gain: `^(income|revenue)s?:(capital[- ]?)?(gains?|loss(es)?)(:|$)`
/// - UnrealisedGain: `^equity:unreali[sz]ed([- ](capital[- ])?gains?)?(:|$)`
/// - Imbalance: `^equity:imbalances?(:|$)`
pub fn account_name_infer_type(a: &str) -> Option<AccountType> {
    let top = a.split(ACCT_SEP).next().unwrap_or("").to_lowercase();
    // a pattern matches some line of the name, from its start
    let any_line = |f: fn(&[&str]) -> bool| {
        a.split('\n')
            .any(|l| f(&l.split(ACCT_SEP).collect::<Vec<_>>()))
    };
    match top.as_str() {
        "asset" | "assets" if any_line(is_cash) => Some(AccountType::Cash),
        "asset" | "assets" => Some(AccountType::Asset),
        "debt" | "debts" | "liability" | "liabilities" => Some(AccountType::Liability),
        "equity" if any_line(is_conversion) => Some(AccountType::Conversion),
        "equity" if any_line(is_unrealised_gain) => Some(AccountType::UnrealisedGain),
        "equity" if any_line(is_imbalance) => Some(AccountType::Imbalance),
        "equity" => Some(AccountType::Equity),
        "income" | "incomes" | "revenue" | "revenues" if any_line(is_gain) => {
            Some(AccountType::Gain)
        }
        "income" | "incomes" | "revenue" | "revenues" => Some(AccountType::Revenue),
        "expense" | "expenses" => Some(AccountType::Expense),
        _ => None,
    }
}

/// Whether these components name a cash account: the first is asset(s)
/// and a later one a cash word, though not the third after an empty
/// second (`(:.+)?:` needs something between its colons).
fn is_cash(cs: &[&str]) -> bool {
    const WORDS: &[&str] = &[
        "cash",
        "bank",
        "check",
        "chequ",
        "cheque",
        "checking",
        "chequing",
        "chequeing",
        "saving",
        "savings",
        "current",
    ];
    is_one_of(cs[0], &["asset", "assets"])
        && (cs.iter().enumerate().skip(1))
            .any(|(i, c)| (i != 2 || !cs[1].is_empty()) && is_one_of(c, WORDS))
}

fn is_conversion(cs: &[&str]) -> bool {
    let words = ["trade", "trades", "trading", "conversion"];
    is_one_of(cs[0], &["equity"]) && cs.get(1).is_some_and(|c| is_one_of(c, &words))
}

fn is_gain(cs: &[&str]) -> bool {
    let tops = ["income", "incomes", "revenue", "revenues"];
    let gain = |c: &str| {
        let c = strip_ci(c, "capital").map_or(c, |r| r.strip_prefix(['-', ' ']).unwrap_or(r));
        is_one_of(c, &["gain", "gains", "loss", "losses"])
    };
    is_one_of(cs[0], &tops) && cs.get(1).is_some_and(|c| gain(c))
}

fn is_unrealised_gain(cs: &[&str]) -> bool {
    let unrealised = |c: &str| {
        let Some(rest) = ["unrealised", "unrealized"]
            .iter()
            .find_map(|u| strip_ci(c, u))
        else {
            return false;
        };
        let Some(rest) = rest.strip_prefix(['-', ' ']) else {
            return rest.is_empty();
        };
        let rest = strip_ci(rest, "capital")
            .and_then(|r| r.strip_prefix(['-', ' ']))
            .unwrap_or(rest);
        is_one_of(rest, &["gain", "gains"])
    };
    is_one_of(cs[0], &["equity"]) && cs.get(1).is_some_and(|c| unrealised(c))
}

fn is_imbalance(cs: &[&str]) -> bool {
    let words = ["imbalance", "imbalances"];
    is_one_of(cs[0], &["equity"]) && cs.get(1).is_some_and(|c| is_one_of(c, &words))
}

/// Whether the text is one of these lower-case ASCII words, ignoring case.
fn is_one_of(s: &str, words: &[&str]) -> bool {
    words.iter().any(|w| strip_ci(s, w) == Some(""))
}

/// The text after this lower-case ASCII prefix, ignoring case as a
/// case-insensitive regular expression does: ASCII letters, and the long s
/// and the Kelvin sign, whose case folds are s and k.
fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut chars = s.char_indices();
    for b in prefix.bytes() {
        let (_, c) = chars.next()?;
        let folded = match c {
            '\u{17F}' => 's',
            '\u{212A}' => 'k',
            c => c.to_ascii_lowercase(),
        };
        if folded != b as char {
            return None;
        }
    }
    Some(chars.next().map_or("", |(i, _)| &s[i..]))
}

/// The type of an account: its own or its nearest parent's in this lookup,
/// otherwise the one its name suggests (accountNameType).
pub fn account_name_type(
    types: &std::collections::BTreeMap<AccountName, AccountType>,
    a: &str,
) -> Option<AccountType> {
    std::iter::once(a)
        .chain(ParentNames(a))
        .find_map(|n| types.get(n).copied())
        .or_else(|| account_name_infer_type(a))
}

/// The components of an account name (accountNameComponents).
pub fn account_name_components(a: &str) -> std::str::Split<'_, char> {
    a.split(ACCT_SEP)
}

/// The last component (accountLeafName).
pub fn account_leaf_name(a: &str) -> &str {
    a.rsplit(ACCT_SEP).next().unwrap_or(a)
}

/// The name with each of its parents' components shortened to two
/// characters: `ex:food` for `expenses:food` (accountSummarisedName).
pub fn account_summarised_name(a: &str) -> String {
    let mut cs: Vec<&str> = account_name_components(a).collect();
    let leaf = cs.pop().unwrap_or("");
    if cs.is_empty() {
        return account_leaf_name(a).to_string();
    }
    let mut out: Vec<String> = cs.iter().map(|c| c.chars().take(2).collect()).collect();
    out.push(leaf.to_string());
    out.join(":")
}

/// The number of components; none for the empty name (accountNameLevel).
pub fn account_name_level(a: &str) -> usize {
    if a.is_empty() {
        0
    } else {
        a.matches(ACCT_SEP).count() + 1
    }
}

/// The name without its last component; empty for a top-level name
/// (parentAccountName).
pub fn parent_account_name(a: &str) -> &str {
    match a.rfind(ACCT_SEP) {
        Some(i) => &a[..i],
        None => "",
    }
}

/// The parents of an account, nearest first (parentAccountNames).
pub fn parent_account_names(a: &str) -> impl Iterator<Item = &str> {
    ParentNames(a)
}

struct ParentNames<'a>(&'a str);

impl<'a> Iterator for ParentNames<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let p = parent_account_name(self.0);
        self.0 = p;
        (!p.is_empty()).then_some(p)
    }
}

/// The name and all its parents' names, shortest first: `a:b:c` gives `a`,
/// `a:b`, `a:b:c` (expandAccountName).
pub fn expand_account_name(a: &str) -> impl Iterator<Item = &str> {
    a.match_indices(ACCT_SEP)
        .map(move |(i, _)| &a[..i])
        .chain(std::iter::once(a))
}

/// All these names and their parents, sorted, without duplicates
/// (expandAccountNames).
pub fn expand_account_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let set: std::collections::BTreeSet<&str> =
        names.into_iter().flat_map(expand_account_name).collect();
    set.into_iter().collect()
}

/// The budget report's top-level account for unbudgeted amounts
/// (unbudgetedAccountName).
pub const UNBUDGETED_ACCOUNT_NAME: &str = "<unbudgeted>";

/// The account name without its first `n` components, or `...` if that
/// leaves none; under the `<unbudgeted>` top-level account, the components
/// after it are dropped instead (accountNameDrop).
pub fn account_name_drop(n: usize, a: &str) -> String {
    if a == UNBUDGETED_ACCOUNT_NAME {
        return a.to_string();
    }
    if let Some(rest) = a
        .strip_prefix(UNBUDGETED_ACCOUNT_NAME)
        .and_then(|r| r.strip_prefix(ACCT_SEP))
    {
        return match account_name_drop(n, rest) {
            d if d.is_empty() => UNBUDGETED_ACCOUNT_NAME.to_string(),
            d => format!("{UNBUDGETED_ACCOUNT_NAME}{ACCT_SEP}{d}"),
        };
    }
    let rest: Vec<&str> = a.split(ACCT_SEP).skip(n).collect();
    if rest.is_empty() {
        "...".to_string()
    } else {
        rest.join(":")
    }
}

/// These account names without duplicates, in tree order: each after its
/// parent, siblings in the `declared` order of their account directives, then the
/// undeclared ones by name; with `keep_parents`, their missing parents are
/// added (sortAccountNamesByDeclaration).
pub fn sort_account_names_by_declaration<'a, 'd>(
    declared: impl IntoIterator<Item = (&'d str, usize)>,
    keep_parents: bool,
    names: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut order: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (a, n) in declared {
        order.entry(a).or_insert(n);
    }
    let names: std::collections::BTreeSet<&str> = if keep_parents {
        names.into_iter().flat_map(expand_account_name).collect()
    } else {
        names.into_iter().collect()
    };
    // the path from the top: each level's declaration order and name
    let key = |a: &'a str| -> Vec<(usize, &'a str)> {
        expand_account_name(a)
            .map(|p| {
                (
                    order.get(p).copied().unwrap_or(usize::MAX),
                    account_leaf_name(p),
                )
            })
            .collect()
    };
    let mut sorted: Vec<&str> = names.into_iter().collect();
    sorted.sort_by_cached_key(|a| key(a));
    sorted
}

/// Whether the first name is a proper prefix of the second, ending at a
/// component boundary (isAccountNamePrefixOf).
pub fn is_account_name_prefix_of(prefix: &str, a: &str) -> bool {
    a.len() > prefix.len() && a.starts_with(prefix) && a.as_bytes()[prefix.len()] == b':'
}

/// A lot subaccount name (`ACCOUNT:{...}`) split into its base account and
/// its lot name, if it is one (splitLotSubaccount, from Hledger.Data.Lots).
pub fn split_lot_subaccount(a: &str) -> Option<(&str, &str)> {
    if !a.ends_with('}') {
        return None;
    }
    let i = a.rfind(':')?;
    let leaf = &a[i + 1..];
    leaf.starts_with('{').then_some((&a[..i], leaf))
}

/// An account name without any lot subaccount at its end (lotBaseAccount).
pub fn lot_base_account(a: &str) -> &str {
    split_lot_subaccount(a).map_or(a, |(base, _)| base)
}

/// Whether the first is a direct subaccount of the second
/// (isSubAccountNameOf).
pub fn is_sub_account_name_of(s: &str, p: &str) -> bool {
    is_account_name_prefix_of(p, s) && account_name_level(s) == account_name_level(p) + 1
}

/// Whether the posting this account name is written in is real, virtual
/// (`(a)`) or balanced virtual (`[a]`) (accountNamePostingType).
pub fn account_name_posting_type(a: &str) -> Realness {
    let (first, last) = (a.chars().next(), a.chars().next_back());
    match (first, last) {
        (Some('['), Some(']')) => Realness::BalancedVirtual,
        (Some('('), Some(')')) => Realness::Virtual,
        _ => Realness::Real,
    }
}

/// The name without the brackets or parentheses marking a virtual posting
/// (accountNameWithoutPostingType).
pub fn account_name_without_posting_type(a: &str) -> &str {
    match account_name_posting_type(a) {
        Realness::Real => a,
        Realness::Virtual | Realness::BalancedVirtual => text_unbracket(a),
    }
}

/// The name, without any brackets, wrapped as this kind of posting writes it
/// (accountNameWithPostingType).
pub fn account_name_with_posting_type(r: Realness, a: &str) -> String {
    let bare = account_name_without_posting_type(a);
    match r {
        Realness::Real => bare.to_string(),
        Realness::Virtual => format!("({bare})"),
        Realness::BalancedVirtual => format!("[{bare}]"),
    }
}

/// The text without the matching brackets or parentheses enclosing it,
/// however deeply nested (textUnbracket).
fn text_unbracket(s: &str) -> &str {
    let closing = s.chars().map_while(|c| match c {
        '(' => Some(')'),
        '[' => Some(']'),
        _ => None,
    });
    let n = closing
        .zip(s.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    // Brackets are one byte each.
    if 2 * n > s.len() {
        ""
    } else {
        &s[n..s.len() - n]
    }
}

/// Two account names joined with a colon, ignoring empty ones, wrapped as
/// virtual if either is (joinAccountNames).
pub fn join_account_names(a: &str, b: &str) -> String {
    let names: Vec<&str> = [a, b].into_iter().filter(|s| !s.is_empty()).collect();
    concat_account_names(&names)
}

/// Account names joined with colons, wrapped as the first virtual one is
/// (concatAccountNames).
pub fn concat_account_names(names: &[&str]) -> String {
    let realness = names
        .iter()
        .map(|a| account_name_posting_type(a))
        .find(|&r| r != Realness::Real)
        .unwrap_or(Realness::Real);
    let joined = names
        .iter()
        .map(|a| account_name_without_posting_type(a))
        .collect::<Vec<_>>()
        .join(":");
    account_name_with_posting_type(realness, &joined)
}

/// The account name with these aliases applied in turn, keeping any virtual
/// posting brackets (accountNameApplyAliases). An error comes from a regex
/// replacement.
pub fn account_name_apply_aliases(aliases: &[AccountAlias], a: &str) -> Result<String, String> {
    let realness = account_name_posting_type(a);
    let mut name = account_name_without_posting_type(a).to_string();
    for alias in aliases {
        name = alias_replace(alias, &name)?;
    }
    Ok(account_name_with_posting_type(realness, &name))
}

/// One alias applied to an account name (aliasReplace).
fn alias_replace(alias: &AccountAlias, a: &str) -> Result<String, String> {
    match alias {
        AccountAlias::Basic(old, new) => {
            if is_account_name_prefix_of(old, a) || **old == *a {
                Ok(format!("{new}{}", &a[old.len()..]))
            } else {
                Ok(a.to_string())
            }
        }
        AccountAlias::Regex(re, repl) => re.replace(repl, a),
    }
}

/// Aliases as upstream's error messages show them (pshow): pretty-simple's
/// layout of their derived Show text, its strings with characters that are
/// not printable as hex codes, coloured as there if `color`. Alias patterns
/// always ignore case. Only a list with a regular expression alias can fail
/// to apply, so the list never takes pretty-simple's one-line form.
pub fn show_aliases(aliases: &[AccountAlias], color: bool) -> String {
    let paint = |sgr: &str, s: &str| {
        if color {
            format!("\x1b[0;{sgr}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    let quoted = |s: &str| {
        let text: String = (s.chars())
            .map(|c| match c {
                '\n' => "\n  ".to_string(),
                c if crate::text::is_print(c) => c.to_string(),
                c => format!("\\x{:x}", u32::from(c)),
            })
            .collect();
        let q = paint("97;1", "\"");
        format!("{q}{}{q}", paint("94;1", &text))
    };
    let mut out = paint("95;1", "[");
    for (i, alias) in aliases.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&paint("95;1", ","));
        }
        out.push_str(&match alias {
            AccountAlias::Basic(old, new) => format!(" BasicAlias {} {}", quoted(old), quoted(new)),
            AccountAlias::Regex(re, repl) => format!(
                " RegexAlias\n  {} RegexpCI {} {} {}",
                paint("96;1", "("),
                quoted(re.src()),
                paint("96;1", ")"),
                quoted(repl)
            ),
        });
    }
    out.push('\n');
    out + &paint("95;1", "]")
}

/// The name cut to its first `n` components.
fn clip_account_name_to(n: usize, a: &str) -> &str {
    match a.match_indices(ACCT_SEP).nth(n.saturating_sub(1)) {
        _ if n == 0 => "",
        Some((i, _)) => &a[..i],
        None => a,
    }
}

/// A depth limit: an overall one, and ones for accounts matching regular
/// expressions (DepthSpec).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DepthSpec {
    pub flat: Option<usize>,
    pub regexps: Vec<(Regexp, usize)>,
}

impl DepthSpec {
    /// Two depth specs combined: the smaller overall limit, and both lists
    /// of regex limits (DepthSpec's Semigroup).
    pub fn combine(mut self, other: DepthSpec) -> DepthSpec {
        self.flat = match (self.flat, other.flat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.regexps.extend(other.regexps);
        self
    }
}

/// The depth an account is limited to by a depth spec: that of the regex
/// matching it most specifically (whose shallowest matching parent of the
/// account is deepest, or which matches no parent; the later one if two are
/// equally specific), otherwise the overall limit
/// (getAccountNameClippedDepth).
pub fn account_name_clipped_depth(ds: &DepthSpec, a: &str) -> Option<usize> {
    if ds.regexps.is_empty() {
        return ds.flat;
    }
    let parents: Vec<&str> = {
        let mut v: Vec<&str> = expand_account_name(a).collect();
        v.pop();
        v
    };
    let specificity = |r: &Regexp| {
        parents
            .iter()
            .position(|p| r.is_match(p))
            .map_or(usize::MAX, |i| i + 1)
    };
    let mut best: Option<(usize, usize)> = None;
    for (r, d) in ds.regexps.iter().rev() {
        if r.is_match(a) {
            let s = specificity(r);
            best = match best {
                Some((s0, d0)) if s0 >= s => Some((s0, d0)),
                _ => Some((s, *d)),
            };
        }
    }
    best.map(|(_, d)| d).or(ds.flat)
}

/// The account name clipped to the depth its depth spec gives it
/// (clipAccountName).
pub fn clip_account_name<'a>(ds: &DepthSpec, a: &'a str) -> &'a str {
    match account_name_clipped_depth(ds, a) {
        Some(n) => clip_account_name_to(n, a),
        None => a,
    }
}

/// Like [`clip_account_name`], but depth 0 gives `...`
/// (clipOrEllipsifyAccountName).
pub fn clip_or_ellipsify_account_name<'a>(ds: &DepthSpec, a: &'a str) -> &'a str {
    match account_name_clipped_depth(ds, a) {
        None => a,
        Some(0) => "...",
        Some(n) => clip_account_name_to(n, a),
    }
}

/// The name with the characters special in a regular expression escaped
/// (escapeName).
pub fn escape_name(a: &str) -> String {
    let mut out = String::with_capacity(a.len());
    for c in a.chars() {
        if "[]?+|()*$^\\{}.".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A regex matching this account and its subaccounts
/// (accountNameToAccountRegex).
pub fn account_name_to_account_regex(a: &str) -> Regexp {
    crate::regex::to_regex(&format!("^{}(:|$)", escape_name(a)))
        .expect("a valid regular expression")
}

/// A case-insensitive regex matching this account and its subaccounts
/// (accountNameToAccountRegexCI).
pub fn account_name_to_account_regex_ci(a: &str) -> Regexp {
    re(&format!("^{}(:|$)", escape_name(a)))
}

/// A regex matching just this account (accountNameToAccountOnlyRegex).
pub fn account_name_to_account_only_regex(a: &str) -> Regexp {
    crate::regex::to_regex(&format!("^{}$", escape_name(a))).expect("a valid regular expression")
}

/// A case-insensitive regex matching just this account
/// (accountNameToAccountOnlyRegexCI).
pub fn account_name_to_account_only_regex_ci(a: &str) -> Regexp {
    re(&format!("^{}$", escape_name(a)))
}

/// The account name shortened to fit a width, by abbreviating its leading
/// components to two characters each as needed, then eliding the end with
/// `..`; register's ` (split)` names are shortened in parts
/// (elideAccountName).
pub fn elide_account_name(width: isize, s: &str) -> String {
    fn elide_parts(w: isize, parts: &[&str]) -> String {
        let mut done: Vec<&str> = Vec::new();
        let mut rest = parts;
        loop {
            let joined = done
                .iter()
                .chain(rest)
                .copied()
                .collect::<Vec<_>>()
                .join(":");
            if display_width(&joined) as isize <= w || rest.len() <= 1 {
                return joined;
            }
            done.push(take_width(2, rest[0]));
            rest = &rest[1..];
        }
    }
    let fit = |t: &str| fit_text(None, Some(width), true, false, t);
    match s.strip_suffix(" (split)") {
        Some(names) => {
            let names: Vec<&str> = names.split(", ").collect();
            let n = names.len().max(1);
            let per = (width - 8 - 2 * (n as isize - 1)).max(0) / names.len() as isize;
            let parts: Vec<String> = names
                .iter()
                .map(|s| elide_parts(per, &s.split(ACCT_SEP).collect::<Vec<_>>()))
                .collect();
            fit(&format!("{} (split)", parts.join(", ")))
        }
        None => fit(&elide_parts(width, &s.split(ACCT_SEP).collect::<Vec<_>>())),
    }
}

pub(crate) mod tests {
    use super::*;

    #[test]
    fn drop_components() {
        assert_eq!(account_name_drop(1, "a:b:c"), "b:c");
        assert_eq!(account_name_drop(0, "a:b"), "a:b");
        assert_eq!(account_name_drop(2, "a:b"), "...");
        assert_eq!(account_name_drop(1, "<unbudgeted>"), "<unbudgeted>");
        assert_eq!(account_name_drop(1, "<unbudgeted>:a:b"), "<unbudgeted>:b");
        assert_eq!(account_name_drop(2, "<unbudgeted>:a"), "<unbudgeted>:...");
    }

    // tests_AccountName "expandAccountNames".
    #[cfg_attr(test, test)]
    pub(crate) fn expand_names() {
        assert_eq!(
            expand_account_names(["assets:cash", "assets:checking", "expenses:vacation"]),
            [
                "assets",
                "assets:cash",
                "assets:checking",
                "expenses",
                "expenses:vacation"
            ]
        );
        assert_eq!(
            expand_account_name("a:b:c").collect::<Vec<_>>(),
            ["a", "a:b", "a:b:c"]
        );
    }

    // tests_AccountName "isAccountNamePrefixOf".
    #[cfg_attr(test, test)]
    pub(crate) fn prefix_of() {
        assert!(!is_account_name_prefix_of("assets", "assets"));
        assert!(is_account_name_prefix_of("assets", "assets:bank"));
        assert!(is_account_name_prefix_of("assets", "assets:bank:checking"));
        assert!(!is_account_name_prefix_of("my assets", "assets:bank"));
    }

    // tests_AccountName "isSubAccountNameOf".
    #[cfg_attr(test, test)]
    pub(crate) fn sub_account_of() {
        assert!(!is_sub_account_name_of("assets", "assets"));
        assert!(is_sub_account_name_of("assets:bank", "assets"));
        assert!(!is_sub_account_name_of("assets:bank:checking", "assets"));
        assert!(!is_sub_account_name_of("assets:bank", "my assets"));
    }

    // tests_AccountName "accountNameInferType".
    #[cfg_attr(test, test)]
    pub(crate) fn infer_type() {
        use AccountType::*;
        for (name, t) in [
            ("assets", Some(Asset)),
            ("assets:cash", Some(Cash)),
            ("assets:A/R", Some(Asset)),
            ("liabilities", Some(Liability)),
            ("equity", Some(Equity)),
            ("equity:conversion", Some(Conversion)),
            ("expenses", Some(Expense)),
            ("revenues", Some(Revenue)),
            ("revenue", Some(Revenue)),
            ("income", Some(Revenue)),
            ("income:gains", Some(Gain)),
            ("revenue:gain", Some(Gain)),
            ("revenues:capital-gains", Some(Gain)),
            ("income:capitalgain", Some(Gain)),
            ("income:losses", Some(Gain)),
            ("revenue:capital-loss", Some(Gain)),
            ("income:gains:realized", Some(Gain)),
            ("equity:unrealised-gain", Some(UnrealisedGain)),
            ("equity:unrealized gains", Some(UnrealisedGain)),
            ("equity:unrealisedfoo", Some(Equity)),
            ("equity:imbalance", Some(Imbalance)),
            ("equity:imbalances:x", Some(Imbalance)),
            ("equity:imbalanced", Some(Equity)),
            ("Assets:Bank:Checking", Some(Cash)),
            ("other", None),
        ] {
            assert_eq!(account_name_infer_type(name), t, "{name}");
        }
    }

    /// The hand matching agrees with upstream's regular expressions on
    /// names made of every word they look for, misspelt, re-cased, with
    /// long s and Kelvin signs, empty components and line breaks.
    #[test]
    fn infer_type_as_the_regexes_do() {
        use AccountType::*;
        let re = |src: &str| to_regex_ci(src).unwrap();
        let cash = re("^assets?(:.+)?:(cash|bank|che(ck|que?)(ing)?|savings?|current)(:|$)");
        let conversion = re("^equity:(trade|trades|trading|conversion)(:|$)");
        let gain = re("^(income|revenue)s?:(capital[- ]?)?(gains?|loss(es)?)(:|$)");
        let unrealised = re("^equity:unreali[sz]ed([- ](capital[- ])?gains?)?(:|$)");
        let imbalance = re("^equity:imbalances?(:|$)");
        let by_regex = |a: &str| {
            let top = a.split(ACCT_SEP).next().unwrap_or("").to_lowercase();
            match top.as_str() {
                "asset" | "assets" if cash.is_match(a) => Some(Cash),
                "asset" | "assets" => Some(Asset),
                "debt" | "debts" | "liability" | "liabilities" => Some(Liability),
                "equity" if conversion.is_match(a) => Some(Conversion),
                "equity" if unrealised.is_match(a) => Some(UnrealisedGain),
                "equity" if imbalance.is_match(a) => Some(Imbalance),
                "equity" => Some(Equity),
                "income" | "incomes" | "revenue" | "revenues" if gain.is_match(a) => Some(Gain),
                "income" | "incomes" | "revenue" | "revenues" => Some(Revenue),
                "expense" | "expenses" => Some(Expense),
                _ => None,
            }
        };
        let tops = [
            "asset", "Assets", "ASSETſ", "assetſ", "equity", "EQUITY", "income", "Incomes",
            "revenue", "revenues", "expenses", "x",
        ];
        let words = [
            "",
            "cash",
            "CASH",
            "bank",
            "check",
            "cheq",
            "cheque",
            "checking",
            "cheqing",
            "chequeing",
            "chequing",
            "saving",
            "ſavingſ",
            "current",
            "cashx",
            "xcash",
            "trade",
            "trades",
            "trading",
            "conversion",
            "converſion",
            "gain",
            "gains",
            "loss",
            "losses",
            "capital",
            "capitalgain",
            "capital-gains",
            "capital loss",
            "capital_gain",
            "capital--gain",
            "unrealised",
            "unrealized",
            "UNREALIZED",
            "unrealised-gain",
            "unrealized gains",
            "unrealised capital gains",
            "unrealised-capital-gain",
            "unrealised capitalgain",
            "unrealised-",
            "unrealisedgain",
            "imbalance",
            "imbalances",
            "imbalanced",
            "\u{212A}",
            "ban\u{212A}",
            "gaın",
            "GAİN",
            "Ｇain",
            "a\nassets",
            "x\nequity",
        ];
        let mut names = Vec::new();
        for t in tops {
            for w in words {
                names.push(format!("{t}:{w}"));
                for v in ["", "x", "cash", "gain"] {
                    names.push(format!("{t}:{w}:{v}"));
                    names.push(format!("{t}:{v}:{w}"));
                    names.push(format!("{t}:{w}\n{t}:{v}"));
                }
                names.push(format!("{t}::{w}"));
                names.push(format!("{t}:x:y:{w}"));
            }
        }
        for name in names {
            assert_eq!(account_name_infer_type(&name), by_regex(&name), "{name:?}");
        }
    }

    // tests_AccountName "joinAccountNames", tests_Posting "joinAccountNames".
    #[cfg_attr(test, test)]
    pub(crate) fn join_names() {
        assert_eq!(join_account_names("assets", "cash"), "assets:cash");
        assert_eq!(join_account_names("assets:cash", "a"), "assets:cash:a");
        assert_eq!(join_account_names("assets", "(cash)"), "(assets:cash)");
        assert_eq!(join_account_names("assets", "[cash]"), "[assets:cash]");
        assert_eq!(join_account_names("(assets)", "cash"), "(assets:cash)");
        assert_eq!(join_account_names("", "assets"), "assets");
        assert_eq!(join_account_names("assets", ""), "assets");
        assert_eq!(join_account_names("a", "b:c"), "a:b:c");
        assert_eq!(join_account_names("[a]", "(b:c)"), "[a:b:c]");
    }

    // tests_AccountName "concatAccountNames", tests_Posting "concatAccountNames".
    #[cfg_attr(test, test)]
    pub(crate) fn concat_names() {
        assert_eq!(concat_account_names(&["assets", "cash"]), "assets:cash");
        assert_eq!(concat_account_names(&["assets:cash", "a"]), "assets:cash:a");
        assert_eq!(concat_account_names(&["assets", "(cash)"]), "(assets:cash)");
        assert_eq!(concat_account_names(&["assets", "[cash]"]), "[assets:cash]");
        assert_eq!(concat_account_names(&["(assets)", "cash"]), "(assets:cash)");
        assert_eq!(concat_account_names(&["", "assets"]), ":assets");
        assert_eq!(concat_account_names(&["assets", ""]), "assets:");
        assert_eq!(concat_account_names(&[]), "");
        assert_eq!(concat_account_names(&["a", "(b)", "[c:d]"]), "(a:b:c:d)");
    }

    // tests_Posting "accountNamePostingType", "accountNameWithoutPostingType",
    // "accountNameWithPostingType".
    #[cfg_attr(test, test)]
    pub(crate) fn posting_types() {
        assert_eq!(account_name_posting_type("a"), Realness::Real);
        assert_eq!(account_name_posting_type("(a)"), Realness::Virtual);
        assert_eq!(account_name_posting_type("[a]"), Realness::BalancedVirtual);
        assert_eq!(account_name_posting_type(""), Realness::Real);
        assert_eq!(account_name_without_posting_type("(a)"), "a");
        assert_eq!(account_name_without_posting_type("([a])"), "a");
        assert_eq!(account_name_without_posting_type("((a)"), "(a");
        assert_eq!(
            account_name_with_posting_type(Realness::Virtual, "[a]"),
            "(a)"
        );
        assert_eq!(text_unbracket("()"), "");
    }

    #[test]
    fn names() {
        assert_eq!(account_leaf_name("a:b:c"), "c");
        assert_eq!(account_leaf_name("a"), "a");
        assert_eq!(account_name_level(""), 0);
        assert_eq!(account_name_level("a:b"), 2);
        assert_eq!(parent_account_name("a:b:c"), "a:b");
        assert_eq!(parent_account_name("a"), "");
        assert_eq!(
            parent_account_names("a:b:c").collect::<Vec<_>>(),
            ["a:b", "a"]
        );
        assert_eq!(clip_account_name_to(2, "a:b:c"), "a:b");
        assert_eq!(clip_account_name_to(5, "a:b:c"), "a:b:c");
        assert_eq!(escape_name("a.b(c)"), "a\\.b\\(c\\)");
        assert!(account_name_to_account_regex_ci("Assets").is_match("assets:cash"));
        assert!(!account_name_to_account_only_regex_ci("assets").is_match("assets:cash"));
    }

    #[test]
    fn aliases() {
        let basic = AccountAlias::Basic("a".into(), "x:y".into());
        let regex = AccountAlias::Regex(crate::regex::to_regex("b$").unwrap(), "B".into());
        assert_eq!(
            account_name_apply_aliases(std::slice::from_ref(&basic), "a:b").unwrap(),
            "x:y:b"
        );
        assert_eq!(
            account_name_apply_aliases(std::slice::from_ref(&basic), "ab").unwrap(),
            "ab"
        );
        assert_eq!(
            account_name_apply_aliases(&[basic, regex], "(a:b)").unwrap(),
            "(x:y:B)"
        );
    }

    #[test]
    fn depth_specs() {
        let ds = DepthSpec {
            flat: Some(1),
            regexps: vec![(crate::regex::to_regex("^a").unwrap(), 2)],
        };
        assert_eq!(clip_account_name(&ds, "a:b:c"), "a:b");
        assert_eq!(clip_account_name(&ds, "x:y"), "x");
        let zero = DepthSpec {
            flat: Some(0),
            regexps: vec![],
        };
        assert_eq!(clip_or_ellipsify_account_name(&zero, "x:y"), "...");
        assert_eq!(
            DepthSpec {
                flat: Some(3),
                regexps: vec![]
            }
            .combine(zero)
            .flat,
            Some(0)
        );
    }

    #[test]
    fn account_types() {
        assert_eq!(AccountType::parse(false, "a"), Ok(AccountType::Asset));
        assert_eq!(AccountType::parse(false, "X"), Ok(AccountType::Expense));
        assert_eq!(AccountType::parse(false, "asset"), Err("asset".into()));
        assert_eq!(AccountType::parse(true, "Asset"), Ok(AccountType::Asset));
        assert_eq!(
            AccountType::parse(true, "unrealized-gains"),
            Ok(AccountType::UnrealisedGain)
        );
        assert!(AccountType::Cash.is_subtype_of(AccountType::Asset));
        assert!(!AccountType::Asset.is_subtype_of(AccountType::Cash));
        assert!(AccountType::Gain.is_income_statement());
        assert_eq!(AccountType::UnrealisedGain.to_string(), "U");
        assert_eq!(AccountType::choices(false), "A, L, E, R, X, C, V, G, U, M");
    }

    #[test]
    fn eliding() {
        assert_eq!(
            elide_account_name(20, "assets:bank:checking"),
            "assets:bank:checking"
        );
        assert_eq!(
            elide_account_name(15, "assets:bank:checking"),
            "as:ba:checking"
        );
        assert_eq!(elide_account_name(5, "assets:bank:checking"), "..ing");
    }
}
