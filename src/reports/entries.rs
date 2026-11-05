//! The transactions a report shows (Hledger.Reports.EntriesReport).

use crate::model::{Journal, Transaction};
use crate::reports::opts::ReportSpec;
use crate::valuation::ConversionOp;

/// The transactions the query selects (but not by depth), sorted by the
/// report's transaction dates, converted to cost or value as the options
/// say (to no cost by default), with --invert's signs (entriesReport).
pub fn entries_report(spec: &ReportSpec, j: Journal) -> Result<Vec<Transaction>, String> {
    let mut txns = entries_report_unsorted(spec, j)?.txns;
    txns.sort_by_key(|t| spec.opts.transaction_date(t));
    Ok(txns)
}

/// Like [`entries_report`], keeping the journal order, as the journal
/// with just those transactions (entriesReportUnsorted).
pub fn entries_report_unsorted(spec: &ReportSpec, mut j: Journal) -> Result<Journal, String> {
    let q = spec.query.clone().filter(|q| !q.is_depth());
    // account type terms match through the journal's account types (filterJournalTransactions)
    let txns = std::mem::take(&mut j.txns);
    j.txns = txns
        .into_iter()
        .filter(|t| q.matches_transaction_extra(&|a| j.account_type(a), t))
        .collect();
    let mut j = spec
        .clone()
        .with_default_conversion_op(ConversionOp::NoConversionOp)
        .journal_apply_valuation(j)?;
    if spec.opts.invert {
        for t in &mut j.txns {
            t.negate();
        }
    }
    Ok(j)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{DateSpan, Day};
    use crate::query::Query;
    use crate::read::{InputOpts, read_journal_file};
    use crate::regex::to_regex;
    use crate::reports::opts::ReportOpts;

    // tests_EntriesReport, on examples/sample.journal for samplejournal.
    #[test]
    fn entries_report_selects() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/hledger/examples/sample.journal"
        );
        let j = read_journal_file(&InputOpts::default(), path).unwrap();
        let today = Day::from_ymd(2026, 1, 1).unwrap();
        let spec = |q| ReportSpec {
            query: q,
            ..ReportSpec::new(today, ReportOpts::default()).unwrap()
        };
        let not_bank = Query::Not(Box::new(Query::Acct(to_regex("bank").unwrap())));
        assert_eq!(entries_report(&spec(not_bank), j.clone()).unwrap().len(), 1);
        let june = DateSpan::exact(Day::from_ymd(2008, 6, 1), Day::from_ymd(2008, 7, 1));
        assert_eq!(
            entries_report(&spec(Query::Date(june)), j).unwrap().len(),
            3
        );
    }
}
