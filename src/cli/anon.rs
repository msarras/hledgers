//! Light obfuscation of a journal's text for --obfuscate (Hledger.Cli.Anon):
//! deterministic, so it gives little privacy.

use crate::model::{Journal, Posting, Transaction};

/// A text replaced by its hash: the low 32 bits of XXH3-64 of its UTF-8
/// bytes, in hexadecimal, as hashable's Text hash and showHex give it
/// (anon for Text).
pub fn anon(t: &str) -> String {
    format!("{:x}", xxhash_rust::xxh3::xxh3_64(t.as_bytes()) as u32)
}

/// An account name with each part replaced by its hash (anonAccount).
pub fn anon_account(a: &str) -> String {
    a.split(crate::account::ACCT_SEP)
        .map(anon)
        .collect::<Vec<_>>()
        .join(":")
}

/// The journal with its transactions' descriptions, codes and account names
/// hashed and their comments removed, its source items dropped, and its
/// declared account names hashed whole, as upstream does (anon for Journal).
/// Payee declarations, tags, and the account types found while reading are
/// kept, as upstream keeps them.
pub fn anon_journal(j: &mut Journal) {
    for t in &mut j.txns {
        anon_transaction(t);
    }
    j.items.clear();
    for (a, _) in &mut j.declared_accounts {
        *a = anon(a).into();
    }
    j.declared_account_tags = std::mem::take(&mut j.declared_account_tags)
        .into_iter()
        .map(|(a, tags)| (anon(&a).into(), tags))
        .collect();
    for names in j.declared_account_types.values_mut() {
        for a in names {
            *a = anon(a).into();
        }
    }
}

fn anon_transaction(t: &mut Transaction) {
    for p in &mut t.postings {
        anon_posting(p);
    }
    t.description = anon(&t.description);
    t.code = anon(&t.code);
    t.comment.clear();
    t.preceding_comment.clear();
}

fn anon_posting(p: &mut Posting) {
    p.account = anon_account(&p.account).into();
    p.comment.clear();
    if let Some(o) = &mut p.original {
        anon_posting(o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_as_upstream() {
        // values from hledger 1.99.6's --obfuscate
        assert_eq!(anon(""), "38d394c2");
        assert_eq!(anon("assets"), "fad4e635");
        assert_eq!(anon("desc"), "669e83b9");
        assert_eq!(anon_account("assets:bank"), "fad4e635:ae773ffa");
        assert_eq!(anon("assets:bank"), "dec101fa");
    }
}
