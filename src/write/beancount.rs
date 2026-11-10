//! Transactions and journals as Beancount data (Hledger.Write.Beancount):
//! account and commodity names converted to Beancount's rules, tags as
//! metadata, costs and lot cost bases in Beancount's syntax, and with
//! --export, options and directives.

use std::collections::BTreeMap;

use crate::account::{AccountName, AccountType, account_name_components};
use crate::amount::currency::currency_symbol_to_code;
use crate::amount::render::{
    AmountFormat, show_amount_b, show_amount_cost_b, show_mixed_amount_lines_parts_b,
};
use crate::amount::{Amount, AmountCost, Side, Sym};
use crate::dates::Day;
use crate::model::render::render_comment_lines;
use crate::model::{
    CONVERSION_POSTING_TAG, Journal, Posting, PriceDirective, Status, Tag, Transaction,
    is_hidden_tag_name, payee_and_note_from_description,
};
use crate::text::{Wide, display_width, fit_text, is_letter, is_space, is_upper, strip_quotes};

/// Beancount's top-level account names.
const TOP_LEVEL_ACCOUNTS: [&str; 5] = ["Assets", "Liabilities", "Equity", "Income", "Expenses"];

/// A transaction as Beancount data, ending with a blank line
/// (showTransactionBeancount): any preceding comment lines as Beancount
/// comments; the date, `!` if pending or else `*`, the payee and note in
/// double quotes, and the same-line comment; the transaction's tags as
/// metadata; its other comment lines; and its postings. An account name
/// Beancount can't take is an error.
pub fn show_transaction_beancount(t: &Transaction) -> Result<String, String> {
    let mut out = String::new();
    for l in t.preceding_comment.lines() {
        out.push_str(&beancount_comment_line(l));
        out.push('\n');
    }
    out.push_str(&t.date.to_string());
    out.push_str(if t.status == Status::Pending {
        " !"
    } else {
        " *"
    });
    let wrapq = |s: &str| format!(" \"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let (payee, note) = if t.description.contains('|') {
        payee_and_note_from_description(&t.description)
    } else {
        ("", t.description.as_str())
    };
    match (payee, note) {
        ("", "") => {}
        ("", n) => out.push_str(&wrapq(n)),
        (p, n) => {
            out.push_str(&wrapq(p));
            out.push_str(&wrapq(n));
        }
    }
    let comments = render_comment_lines(&t.comment);
    if let Some(c) = comments.first() {
        out.push_str(c);
    }
    out.push('\n');
    let mds = tags_to_beancount_metadata(&t.tags);
    let width = mds
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for md in &mds {
        out.push_str("    ");
        out.push_str(&show_beancount_metadata(Some(width), md));
        out.push('\n');
    }
    for c in comments.iter().skip(1) {
        out.push_str(c);
        out.push('\n');
    }
    for l in postings_as_lines_beancount(&t.postings)? {
        out.push_str(&l);
        out.push('\n');
    }
    out.push('\n');
    Ok(out)
}

/// Transactions prepared for Beancount (beancountTransactions): without
/// virtual postings, and without the conversion postings that duplicate a
/// cost, since Beancount can't have both.
pub fn beancount_transactions(ts: Vec<Transaction>) -> Vec<Transaction> {
    ts.into_iter()
        .map(|mut t| {
            t.postings.retain(Posting::is_real);
            let has_cost =
                (t.postings.iter()).any(|p| p.amount.amounts().iter().any(|a| a.cost.is_some()));
            if has_cost {
                t.postings.retain(|p| !p.has_tag(CONVERSION_POSTING_TAG));
            }
            t
        })
        .collect()
}

/// The journal's account declarations and these transactions' postings with
/// each account whose top-level name is not one of Beancount's, but whose
/// type is known, put under the Beancount top-level account for that type
/// (beancountRenameAccounts): `bonds:treasury`, of type A, becomes
/// `Assets:bonds:treasury`.
pub fn beancount_rename_accounts(j: &mut Journal, ts: &mut [Transaction]) {
    let rename = |a: &AccountName| -> AccountName {
        if has_beancount_top_level_account(a) {
            return a.clone();
        }
        match j.account_type(a) {
            Some(t) => format!("{}:{a}", top_level_account_for(t)).into(),
            None => a.clone(),
        }
    };
    let declared: Vec<(AccountName, _)> = (j.declared_accounts.iter())
        .map(|(a, info)| (rename(a), info.clone()))
        .collect();
    let tags: BTreeMap<AccountName, Vec<Tag>> = (j.declared_account_tags.iter())
        .map(|(a, tags)| (rename(a), tags.clone()))
        .collect();
    for p in ts.iter_mut().flat_map(|t| &mut t.postings) {
        p.account = rename(&p.account);
    }
    j.declared_accounts = declared;
    j.declared_account_tags = tags;
}

/// Whether this account name's top-level part becomes one of Beancount's
/// top-level accounts (hasBeancountTopLevelAccount).
fn has_beancount_top_level_account(a: &str) -> bool {
    let top = account_name_components(a).next().unwrap_or("");
    TOP_LEVEL_ACCOUNTS.contains(&top_level_component(top).as_str())
}

/// The Beancount top-level account for an account type
/// (beancountTopLevelAccountFor).
fn top_level_account_for(t: AccountType) -> &'static str {
    use AccountType::*;
    match t {
        Asset | Cash => "Assets",
        Liability => "Liabilities",
        Equity | Conversion | UnrealisedGain | Imbalance => "Equity",
        Revenue | Gain => "Income",
        Expense => "Expenses",
    }
}

/// The options and directives that begin a Beancount export of this journal
/// and these (prepared) transactions, each section followed by a blank line
/// (beancountDirectives): a sample tolerance option, commented out; an
/// operating_currency option for each currency used in costs; a commodity
/// directive for each declared commodity, with its tags as metadata; an open
/// directive for each declared or used account, dated on its first posting
/// (or the first date overall), with its tags as metadata and a `lots:` tag
/// as its booking method; and the price directives, by date.
pub fn beancount_directives(j: &Journal, ts: &[Transaction]) -> Result<String, String> {
    let tolerance = ";option \"inferred_tolerance_default\" \"*:0.005\"\n".to_string();
    let mut cost_currencies: Vec<String> = (ts.iter())
        .flat_map(|t| &t.postings)
        .flat_map(|p| p.amount.amounts())
        .filter_map(|a| match a.cost {
            Some(AmountCost::Unit(c) | AmountCost::Total(c)) => Some(c.commodity.to_string()),
            None => None,
        })
        .collect();
    cost_currencies.sort();
    cost_currencies.dedup();
    let operating: String = (cost_currencies.iter())
        .map(|c| {
            format!(
                "option \"operating_currency\" \"{}\"\n",
                commodity_to_beancount(c)
            )
        })
        .collect();
    let first_date: Day = ts.iter().map(|t| t.date).min().unwrap_or_else(|| {
        (j.price_directives.iter().map(|pd| pd.date).min())
            .unwrap_or_else(|| Day::from_ymd(1900, 1, 1).unwrap())
    });
    let mut commodity_tags: BTreeMap<&Sym, &[Tag]> = BTreeMap::new();
    for c in j.declared_commodities.keys() {
        commodity_tags.insert(c, &[]);
    }
    for (c, tags) in &j.declared_commodity_tags {
        commodity_tags.insert(c, tags);
    }
    let commodities: String = (commodity_tags.iter())
        .map(|(c, tags)| {
            let line = format!("{first_date} commodity {}", commodity_to_beancount(c));
            with_metadata(line, tags) + "\n"
        })
        .collect();
    let mut first_posting_dates: BTreeMap<&str, Day> = BTreeMap::new();
    for t in ts {
        for p in &t.postings {
            let d = first_posting_dates.entry(&p.account).or_insert(t.date);
            *d = (*d).min(t.date);
        }
    }
    let mut accounts: Vec<&str> = (j.declared_accounts.iter().map(|(a, _)| &**a))
        .chain(first_posting_dates.keys().copied())
        .collect();
    accounts.sort();
    accounts.dedup();
    let mut opens = String::new();
    for a in accounts {
        let d = first_posting_dates.get(a).copied().unwrap_or(first_date);
        let tags = j
            .declared_account_tags
            .get(a)
            .map_or(&[][..], Vec::as_slice);
        let is_lots = |(n, _): &&Tag| n.to_lowercase() == "lots";
        let booking = (tags.iter().find(is_lots))
            .map(|(_, v)| format!(" \"{v}\""))
            .unwrap_or_default();
        let line = format!("{d} open {}{booking}", account_name_to_beancount(a)?);
        let tags: Vec<Tag> = tags.iter().filter(|t| !is_lots(t)).cloned().collect();
        opens.push_str(&with_metadata(line, &tags));
        opens.push('\n');
    }
    let mut prices: Vec<&PriceDirective> = j.price_directives.iter().collect();
    prices.sort_by_key(|pd| pd.date);
    let prices: String = (prices.into_iter())
        .map(|pd| show_price_directive_beancount(pd) + "\n")
        .collect();
    let sections = [tolerance, operating, commodities, opens, prices];
    Ok(sections
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// A directive line followed by its tags as indented metadata lines.
fn with_metadata(line: String, tags: &[Tag]) -> String {
    let mds = tags_to_beancount_metadata(tags);
    let width = mds
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = line;
    for md in &mds {
        out.push_str("\n    ");
        out.push_str(&show_beancount_metadata(Some(width), md));
    }
    out
}

/// A journal comment line (starting with `;`, `#` or `*`) as a Beancount
/// comment line, starting with `;` (beancountCommentLine).
pub fn beancount_comment_line(l: &str) -> String {
    if l.trim_start_matches(is_space).starts_with(';') {
        l.to_string()
    } else {
        format!("; {l}")
    }
}

/// A comment block as Beancount comment lines, without its `comment` and
/// `end comment` lines (beancountItemRenderer's irCommentBlock).
pub fn beancount_comment_block(t: &str) -> String {
    (t.lines().skip(1))
        .filter(|l| !l.starts_with("end comment"))
        .map(|l| format!("; {l}\n"))
        .collect()
}

/// A price directive as Beancount's `DATE price COMMODITY AMOUNT`
/// (showPriceDirectiveBeancount).
fn show_price_directive_beancount(pd: &PriceDirective) -> String {
    format!(
        "{} price {} {}",
        pd.date,
        commodity_to_beancount(&pd.commodity),
        show_amount_b(&beancount_fmt(), &amount_to_beancount(&pd.amount)).s
    )
}

/// Amounts with zeros' commodities, a decimal mark after digit groups, and
/// commodity symbols unquoted.
fn beancount_fmt() -> AmountFormat {
    AmountFormat {
        zero_commodity: true,
        force_decimal_mark: true,
        quotes: false,
        ..AmountFormat::DEFAULT
    }
}

/// A metadata item as a metadata line, without indentation; with a name
/// width, the values of successive lines line up (showBeancountMetadata).
fn show_beancount_metadata(name_width: Option<usize>, (n, v): &Tag) -> String {
    let name = fit_text(
        name_width.map(|w| w as isize + 2),
        None,
        false,
        true,
        &format!("{n}: "),
    );
    let value: String = v
        .chars()
        .map(|c| match c {
            '"' => "c22".to_string(),
            c => c.to_string(),
        })
        .collect();
    format!("{name}\"{value}\"")
}

/// Tags as Beancount metadata (tagsToBeancountMetadata): hidden tags
/// dropped, the values of a repeated tag joined by commas, names encoded,
/// sorted by name and value.
fn tags_to_beancount_metadata(tags: &[Tag]) -> Vec<Tag> {
    let mut visible: Vec<&Tag> = tags
        .iter()
        .filter(|(n, _)| !is_hidden_tag_name(n))
        .collect();
    visible.sort();
    let mut unique: Vec<Tag> = Vec::new();
    for (n, v) in visible {
        match unique.last_mut() {
            Some((ln, lv)) if ln == n => {
                lv.push_str(", ");
                lv.push_str(v);
            }
            _ => unique.push((n.clone(), v.clone())),
        }
    }
    let mut mds: Vec<Tag> = (unique.into_iter())
        .map(|(n, v)| (metadata_name(&n), v))
        .collect();
    mds.sort();
    mds
}

/// A tag name as a Beancount metadata name, `[a-z][a-z0-9_-]+`
/// (toBeancountMetadataName): ASCII capitals lowercased, spaces made `-`,
/// other characters encoded as `cHEX`, and an `m` prepended if it is shorter
/// than two characters or does not start with a lowercase ASCII letter.
fn metadata_name(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' | '-' => c.to_string(),
            'A'..='Z' => c.to_ascii_lowercase().to_string(),
            c if is_space(c) => "-".to_string(),
            c => format!("c{:x}", c as u32),
        })
        .collect();
    let mut cs = t.chars();
    match (cs.next(), cs.next()) {
        (None, _) => "mm".to_string(),
        (Some(c), rest) if rest.is_none() || !c.is_ascii_lowercase() => format!("m{t}"),
        _ => t,
    }
}

/// Postings as indented Beancount lines, aligned with each other
/// (postingsAsLinesBeancount).
fn postings_as_lines_beancount(ps: &[Posting]) -> Result<Vec<String>, String> {
    let rendered = (ps.iter())
        .map(PostingBeancount::new)
        .collect::<Result<Vec<_>, String>>()?;
    let acct_width = rendered.iter().map(|r| r.acct_width).max().unwrap_or(0);
    let amt_width = (rendered.iter())
        .flat_map(|r| r.amounts.iter().map(|a| a.width))
        .max()
        .unwrap_or(0);
    Ok((ps.iter().zip(&rendered))
        .flat_map(|(p, r)| r.lines(p, acct_width, amt_width))
        .collect())
}

/// A posting's Beancount account name and amounts.
struct PostingBeancount {
    account: String,
    acct_width: usize,
    amounts: Vec<Wide>,
}

impl PostingBeancount {
    fn new(p: &Posting) -> Result<PostingBeancount, String> {
        let account = account_name_to_beancount(&p.account)?;
        // the amounts without costs, then their cost bases and costs in
        // Beancount's order, paired as upstream pairs them
        let base = AmountFormat {
            cost: false,
            cost_basis: false,
            ..beancount_fmt()
        };
        let m = p.amount.map(amount_to_beancount);
        let amounts = (show_mixed_amount_lines_parts_b(&base, &m).into_iter())
            .zip(m.amounts())
            .map(|((mut w, _), a)| {
                w.push(&show_cost_basis_beancount(&a));
                w.push(&show_amount_cost_b(&beancount_fmt(), &a));
                w
            })
            .collect();
        Ok(PostingBeancount {
            acct_width: display_width(&account),
            account,
            amounts,
        })
    }

    /// The posting's lines (postingAsLinesBeancount): one per amount, each
    /// followed by the posting's metadata (its tags written in its comment)
    /// and its other comment lines. Balance assertions are not shown, and a
    /// pending posting shows `!` in place of its account name, as upstream.
    fn lines(&self, p: &Posting, acct_width: usize, amt_width: usize) -> Vec<String> {
        let status_and_account = if p.status == Status::Pending {
            "! "
        } else {
            &self.account
        };
        let status_and_account = format!(
            "    {}",
            fit_text(
                Some(2 + acct_width as isize),
                None,
                false,
                true,
                status_and_account
            )
        );
        let in_comment = |n: &str| {
            crate::regex::to_regex(&format!("\\b{n}:")).is_ok_and(|r| r.is_match(&p.comment))
        };
        let tags: Vec<Tag> = p
            .tags
            .iter()
            .filter(|(n, _)| in_comment(n))
            .cloned()
            .collect();
        let mds = tags_to_beancount_metadata(&tags);
        let width = mds
            .iter()
            .map(|(n, _)| n.chars().count())
            .max()
            .unwrap_or(0);
        let comments = render_comment_lines(&p.comment);
        let (same_line, next_lines) = match comments.split_first() {
            None => ("", &[][..]),
            Some((c, cs)) => (c.as_str(), cs),
        };
        let mut out = Vec::new();
        for amt in &self.amounts {
            let pad = amt_width.max(12) - amt.width;
            let line = format!(
                "{status_and_account}  {}{}{same_line}",
                " ".repeat(pad),
                amt.s
            );
            out.extend(line.lines().map(|l| l.trim_end().to_string()));
            for md in &mds {
                out.push(format!(
                    "      {}",
                    show_beancount_metadata(Some(width), md)
                ));
            }
            out.extend(next_lines.iter().map(|c| format!("  {c}")));
        }
        out
    }
}

/// An hledger account name as a Beancount account name
/// (accountNameToBeancount): each part encoded and capitalised, `revenue`
/// or `revenues` at the top made `Income`, and a second part `A` added to
/// a lone top-level part. A top-level part that is not one of Beancount's
/// is an error.
pub fn account_name_to_beancount(a: &str) -> Result<String, String> {
    let mut parts = account_name_components(a);
    let mut cs: Vec<String> = Vec::new();
    if let Some(top) = parts.next() {
        cs.push(top_level_component(top));
        cs.extend(parts.map(account_name_component_to_beancount));
    }
    let joined = cs.join(":");
    match cs.as_slice() {
        [c, ..] if !TOP_LEVEL_ACCOUNTS.contains(&c.as_str()) => Err(format!(
            "bad top-level account: {c}\n\
             in beancount account name:           {joined}\n\
             converted from hledger account name: {a}\n\
             For Beancount, top-level accounts must be one of {}.\n\
             Declare this account's type (eg: account {a}  ; type:A) so that hledger can add the right one,\n\
             or use --alias to rename it.\n",
            TOP_LEVEL_ACCOUNTS.join(", ")
        )),
        [_] => Ok(format!("{joined}:A")),
        _ => Ok(joined),
    }
}

/// An account name's top-level part for Beancount
/// (beancountTopLevelComponent).
fn top_level_component(c: &str) -> String {
    match c.to_lowercase().as_str() {
        "revenue" | "revenues" => "Income".to_string(),
        _ => account_name_component_to_beancount(c),
    }
}

/// An account name part for Beancount (accountNameComponentToBeancount):
/// characters other than letters, digits and `-` encoded, the first letter
/// made title case, and an `A` prepended if it then doesn't start with a
/// capital letter or digit.
fn account_name_component_to_beancount(part: &str) -> String {
    if part.is_empty() {
        return String::new();
    }
    let encoded: String = part
        .chars()
        .map(|c| {
            if is_letter(c) || c.is_ascii_digit() || c == '-' {
                c.to_string()
            } else {
                char_to_beancount(c)
            }
        })
        .collect();
    let mut cs = encoded.chars();
    let first = cs.next().unwrap_or_default();
    let t = to_title(first) + cs.as_str();
    match t.chars().next() {
        Some(c) if !((is_letter(c) && is_upper(c)) || c.is_ascii_digit()) => format!("A{t}"),
        _ => t,
    }
}

/// A character not allowed in a Beancount name: `-` for a space or
/// underscore, otherwise `CHEX` (charToBeancount).
fn char_to_beancount(c: char) -> String {
    if is_space(c) || c == '_' {
        "-".to_string()
    } else {
        format!("C{:x}", c as u32)
    }
}

/// A character in title case, as Data.Text's toTitle converts it: its
/// uppercase form except for the letters whose title case differs (the
/// Latin digraphs and ligatures, Armenian ligatures, Georgian letters and
/// Greek letters with ypogegrammeni).
fn to_title(c: char) -> String {
    let s = |cs: &[u32]| cs.iter().filter_map(|&u| char::from_u32(u)).collect();
    match c as u32 {
        0x1c4..=0x1c6 => s(&[0x1c5]),
        0x1c7..=0x1c9 => s(&[0x1c8]),
        0x1ca..=0x1cc => s(&[0x1cb]),
        0x1f1..=0x1f3 => s(&[0x1f2]),
        0xdf => s(&[0x53, 0x73]),
        0x587 => s(&[0x535, 0x582]),
        0x10d0..=0x10fa | 0x10fd..=0x10ff => c.to_string(),
        0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => s(&[c as u32 + 8]),
        0x1f88..=0x1f8f | 0x1f98..=0x1f9f | 0x1fa8..=0x1faf | 0x1fbc | 0x1fcc | 0x1ffc => {
            c.to_string()
        }
        0x1fb3 => s(&[0x1fbc]),
        0x1fc3 => s(&[0x1fcc]),
        0x1ff3 => s(&[0x1ffc]),
        0x1fb2 => s(&[0x1fba, 0x345]),
        0x1fb4 => s(&[0x386, 0x345]),
        0x1fc2 => s(&[0x1fca, 0x345]),
        0x1fc4 => s(&[0x389, 0x345]),
        0x1ff2 => s(&[0x1ffa, 0x345]),
        0x1ff4 => s(&[0x38f, 0x345]),
        0x1fb7 => s(&[0x391, 0x342, 0x345]),
        0x1fc7 => s(&[0x397, 0x342, 0x345]),
        0x1ff7 => s(&[0x3a9, 0x342, 0x345]),
        0xfb00 => s(&[0x46, 0x66]),
        0xfb01 => s(&[0x46, 0x69]),
        0xfb02 => s(&[0x46, 0x6c]),
        0xfb03 => s(&[0x46, 0x66, 0x69]),
        0xfb04 => s(&[0x46, 0x66, 0x6c]),
        0xfb05 | 0xfb06 => s(&[0x53, 0x74]),
        0xfb13 => s(&[0x544, 0x576]),
        0xfb14 => s(&[0x544, 0x565]),
        0xfb15 => s(&[0x544, 0x56b]),
        0xfb16 => s(&[0x54e, 0x576]),
        0xfb17 => s(&[0x544, 0x56d]),
        _ => c.to_uppercase().collect(),
    }
}

/// An amount as Beancount can read it (amountToBeancount): its commodity
/// converted, written on the right after a space, also in its cost.
fn amount_to_beancount(a: &Amount) -> Amount {
    let mut b = a.clone();
    b.commodity = commodity_to_beancount(&a.commodity).into();
    let mut style = (*a.style).clone();
    style.side = Side::R;
    style.spaced = true;
    b.style = style.into();
    b.cost = a.cost.as_ref().map(|c| match c {
        AmountCost::Unit(c) => AmountCost::Unit(Box::new(amount_to_beancount(c))),
        AmountCost::Total(c) => AmountCost::Total(Box::new(amount_to_beancount(c))),
    });
    b
}

/// An amount's cost basis in Beancount's lot syntax, ` {COST, DATE, "LABEL"}`
/// with the parts it has, ` {}` with none, or nothing
/// (showAmountCostBasisBeancountB).
fn show_cost_basis_beancount(a: &Amount) -> Wide {
    let Some(basis) = a.cost_basis.as_deref() else {
        return Wide::default();
    };
    let mut parts: Vec<Wide> = Vec::new();
    if let Some(cost) = &basis.cost {
        parts.push(show_amount_b(&beancount_fmt(), &amount_to_beancount(cost)));
    }
    if let Some(date) = basis.date {
        parts.push(Wide::new(&date.to_string()));
    }
    if let Some(label) = &basis.label {
        parts.push(Wide::new(&format!("\"{label}\"")));
    }
    if parts.is_empty() {
        return Wide::new(" {}");
    }
    let mut out = Wide::new(" {");
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(", ", 2);
        }
        out.push(part);
    }
    out.push_str("}", 1);
    out
}

/// An hledger commodity symbol as a Beancount commodity name, two to 24
/// capitals, digits or `'._-`, starting with a letter and ending with a
/// letter or digit (commodityToBeancount): a currency symbol made its ISO
/// 4217 code, or the symbol unquoted and capitalised, its other characters
/// encoded, with a `C` prepended or appended as needed, and doubled if it
/// is one character long.
pub fn commodity_to_beancount(com: &str) -> String {
    if com.is_empty() {
        return "CC".to_string();
    }
    let com = strip_quotes(com);
    if let Some(code) = currency_symbol_to_code(com) {
        return code.to_string();
    }
    let capital = |c: char| is_letter(c) && is_upper(c);
    let mut b: String = (com.to_uppercase().chars())
        .map(|c| {
            if capital(c) || c.is_ascii_digit() || "'._-".contains(c) {
                c.to_string()
            } else {
                char_to_beancount(c)
            }
        })
        .collect();
    if !b.chars().next().is_some_and(capital) {
        b.insert(0, 'C');
    }
    if !b
        .chars()
        .last()
        .is_some_and(|c| capital(c) || c.is_ascii_digit())
    {
        b.push('C');
    }
    if b.chars().count() < 2 {
        b = b.repeat(2);
    }
    b
}

pub(crate) mod tests {
    use super::*;

    // commodityToBeancount's doctests.
    #[cfg_attr(test, test)]
    pub(crate) fn commodity_names() {
        assert_eq!(commodity_to_beancount(""), "CC");
        assert_eq!(commodity_to_beancount("$"), "USD");
        assert_eq!(commodity_to_beancount("Usd"), "USD");
        assert_eq!(commodity_to_beancount("\"a1\""), "A1");
        assert_eq!(commodity_to_beancount("\"A 1!\""), "A-1C21");
        assert_eq!(commodity_to_beancount("K"), "KK");
    }

    #[test]
    fn account_names() {
        assert_eq!(account_name_to_beancount("assets").unwrap(), "Assets:A");
        assert_eq!(
            account_name_to_beancount("equity:$-€:$").unwrap(),
            "Equity:C24-C20ac:C24"
        );
        assert_eq!(
            account_name_to_beancount("revenues:wells_fargo:ßx").unwrap(),
            "Income:Wells-fargo:Ssx"
        );
        assert!(
            account_name_to_beancount("other")
                .unwrap_err()
                .starts_with("bad top-level account: Other\n")
        );
    }

    #[test]
    fn metadata_names() {
        assert_eq!(metadata_name(""), "mm");
        assert_eq!(metadata_name("a"), "ma");
        assert_eq!(metadata_name("Empfänger"), "empfce4nger");
        assert_eq!(metadata_name("\"troublesome"), "c22troublesome");
        assert_eq!(metadata_name("😀"), "c1f600");
        assert_eq!(metadata_name("1x"), "m1x");
    }
}
