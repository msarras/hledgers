//! A journal's items as journal text (journalItemsAsText in
//! Hledger.Write.Journal); transactions themselves are rendered by
//! [`crate::model::render`].

use crate::model::{JournalItem, Transaction};

/// How print --export renders a journal's items (ItemRenderer): a
/// transaction, without its trailing blank line; a directive's source text,
/// or nothing to drop it; a comment line; a comment block.
pub struct ItemRenderer<'a> {
    pub transaction: &'a dyn Fn(&Transaction) -> String,
    pub directive: &'a dyn Fn(&str) -> Option<String>,
    pub comment: &'a dyn Fn(&str) -> String,
    pub comment_block: &'a dyn Fn(&str) -> String,
}

/// A journal's items as text, with these transactions in place of the
/// transaction items they started at (and any others at the end), and
/// blank lines normalised: one between transactions, and between groups of
/// directive and comment lines (journalItemsAsText).
pub fn journal_items_as_text(
    r: &ItemRenderer,
    items: &[JournalItem],
    txns: &[Transaction],
) -> String {
    enum Block {
        Lines(String),
        Txn(String),
        Break,
    }
    let key = |p: &crate::model::SourcePos| (p.line, p.col, p.file.clone());
    let mut by_pos: std::collections::BTreeMap<_, std::collections::VecDeque<&Transaction>> =
        Default::default();
    for t in txns {
        by_pos.entry(key(&t.pos.0)).or_default().push_back(t);
    }
    let mut blocks = Vec::new();
    for item in items {
        match item {
            JournalItem::Transaction(pos) => {
                match by_pos.get_mut(&key(pos)).and_then(|q| q.pop_front()) {
                    Some(t) => blocks.push(Block::Txn((r.transaction)(t))),
                    None => blocks.push(Block::Break),
                }
            }
            JournalItem::Blank | JournalItem::Include(_) => blocks.push(Block::Break),
            JournalItem::Comment(t) => blocks.push(Block::Lines((r.comment)(t))),
            JournalItem::CommentBlock(t) => blocks.push(Block::Lines((r.comment_block)(t))),
            JournalItem::Directive(t) => {
                if let Some(s) = (r.directive)(t) {
                    blocks.push(Block::Lines(s));
                }
            }
            JournalItem::NonExportedDirective(_) => {}
        }
    }
    for q in by_pos.into_values() {
        for t in q {
            blocks.push(Block::Txn((r.transaction)(t)));
        }
    }
    // merge adjacent groups of lines; a break keeps them apart, and is dropped
    let mut merged: Vec<Block> = Vec::new();
    let mut after_break = false;
    for b in blocks {
        match (merged.last_mut(), b) {
            (_, Block::Break) => after_break = true,
            (Some(Block::Lines(a)), Block::Lines(b)) if !after_break => a.push_str(&b),
            (_, b) => {
                merged.push(b);
                after_break = false;
            }
        }
    }
    // each block is followed by a blank line, except final comment or directive lines
    let n = merged.len();
    let mut out = String::new();
    for (i, b) in merged.into_iter().enumerate() {
        match b {
            Block::Lines(s) => {
                out.push_str(&s);
                if i + 1 < n {
                    out.push('\n');
                }
            }
            Block::Txn(s) => {
                out.push_str(&s);
                out.push('\n');
            }
            Block::Break => {}
        }
    }
    out
}
