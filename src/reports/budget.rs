//! Budget reports (Hledger.Reports.BudgetReport): each account's actual
//! balance changes in each report period beside its budget goals, the
//! changes the journal's periodic transactions describe.

use std::collections::HashSet;

use crate::account::tree::{AccountTree, BalanceData, PeriodData};
use crate::account::{ACCT_SEP, AccountName, UNBUDGETED_ACCOUNT_NAME, expand_account_name};
use crate::amount::{CommodityStyles, MixedAmount};
use crate::dates::{DateSpan, Interval, interval_boundary_before};
use crate::model::{Journal, PostingRef, Transaction};
use crate::query::Query;
use crate::read::BalancingOpts;
use crate::read::balancing::journal_balance_transactions;
use crate::read::finalise::{journal_number_and_tie_transactions, journal_style_amounts};
use crate::read::modifiers::run_periodic_transaction;
use crate::reports::opts::{AccountListMode, BalanceAccumulation, ReportOpts, ReportSpec};
use crate::reports::periodic::{
    Balance, DisplayName, PeriodicReport, PeriodicReportRow, generate_multi_balance_account,
    generate_periodic_report, get_postings,
};
use crate::valuation::PriceOracle;

/// A budget report cell: an actual balance change and a budget goal,
/// either perhaps absent (BudgetCell).
pub type BudgetCell = (Option<MixedAmount>, Option<MixedAmount>);

/// A budget report: each account's cells in each period (BudgetReport).
pub type BudgetReport = PeriodicReport<DisplayName, BudgetCell>;

impl PeriodicReport<DisplayName, BudgetCell> {
    /// With every amount styled (HasAmounts' styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        let style = |m: &Option<MixedAmount>| m.as_ref().map(|m| m.clone().styled(styles));
        self.map_values(|(actual, goal)| (style(actual), style(goal)));
    }
}

/// The budget report of the matched accounts: their actual balance
/// changes, from the regular transactions, and their goals, from the
/// periodic transactions (those whose description --budget's argument is
/// part of) in each period of the report span and before it; accounts
/// with no goal of their own or above them moved under `<unbudgeted>`,
/// and without --empty, subaccounts with no goal of their own merged into
/// their nearest parent with one. Reported in tree mode
/// (budgetReport).
pub fn budget_report(
    spec: &ReportSpec,
    bopts: &BalancingOpts,
    report_span: DateSpan,
    j: &Journal,
) -> Result<BudgetReport, String> {
    let ropts = ReportOpts {
        account_list_mode: AccountListMode::Tree,
        ..spec.opts.clone()
    };
    let budgeted: Vec<Transaction> = (j.periodic_txns.iter())
        .flat_map(|pt| run_periodic_transaction(false, pt, report_span))
        .collect();
    let budgeted: HashSet<&str> = (budgeted.iter())
        .flat_map(|t| t.postings.iter().map(|p| &*p.account))
        .flat_map(expand_account_name)
        .collect();
    let actualj = journal_with_budget_account_names(&budgeted, ropts.empty, j);
    let budgetj = journal_add_budget_goal_transactions(bopts, &ropts, report_span, j)?;
    let oracle = PriceOracle::new(ropts.infer_prices, ropts.debug, j);
    let budget_spec = ReportSpec {
        query: budget_goal_query(&spec.query),
        ..spec.clone()
    };
    let (_, actual_spans) = spec.report_span(&actualj);
    let (_, budget_spans) = budget_spec.report_span(&budgetj);
    let all_spans = match (&ropts.interval, &budget_spans) {
        // without an interval, the goals' span may be shorter than the
        // actual changes', and can be replaced by it
        (Interval::NoInterval, _) | (_, None) => actual_spans.clone(),
        (_, Some(b)) => actual_spans.as_ref().and_then(|a| b.union(a)),
    };
    let actual = get_postings(spec, &actualj, &oracle, report_span)?;
    let goals = get_postings(&budget_spec, &budgetj, &oracle, report_span)?;
    let actual: Vec<PostingRef> = actual.postings().collect();
    let goals: Vec<PostingRef> = goals.postings().collect();
    let actual =
        generate_multi_balance_account(spec, &actualj, &oracle, actual_spans.as_ref(), &actual)?;
    // without goals, no zero goals either
    let combined = if goals.is_empty() {
        actual.map_data(|d| d.map(|b| (Some(b.clone()), None)))
    } else {
        let goals = generate_multi_balance_account(
            &budget_spec,
            &budgetj,
            &oracle,
            budget_spans.as_ref(),
            &goals,
        )?;
        actual.merge(&goals)
    };
    Ok(generate_budget_report(&ropts, all_spans.as_ref(), combined))
}

/// The report's query without the terms that describe actual transactions
/// and postings (status, code, description, payee, note, tag, realness,
/// amount), which would discard budget goals, whose transactions are made
/// up: just its account, account type, depth, date and commodity terms
/// (budgetGoalQuery).
fn budget_goal_query(q: &Query) -> Query {
    fn selects_account_or_period(q: &Query) -> bool {
        match q {
            Query::Acct(_)
            | Query::DepthAcct(..)
            | Query::Depth(_)
            | Query::Type(_)
            | Query::Date(_)
            | Query::Date2(_)
            | Query::Cur(_)
            | Query::None => true,
            Query::Not(q) => selects_account_or_period(q),
            _ => false,
        }
    }
    q.clone().filter(selects_account_or_period)
}

/// The budget report from the tree of actual changes and goals, each
/// period's perhaps with either missing (generateBudgetReport): sorted by
/// the actual changes.
fn generate_budget_report(
    ropts: &ReportOpts,
    colspans: Option<&crate::dates::DayPartition>,
    acct: AccountTree<(Option<BalanceData>, Option<BalanceData>)>,
) -> BudgetReport {
    generate_periodic_report(
        ropts,
        colspans,
        acct,
        |balance, data| make_budget_report_row(ropts, balance, data),
        |(actual, _)| {
            actual
                .as_ref()
                .map_or_else(MixedAmount::null, |b| b.incl.clone())
        },
        |(actual, _)| actual.clone().unwrap_or_default(),
    )
}

/// A budget report row from an account's period data: each period's actual
/// change (zero when it has only a goal) and goal if any; the sums and
/// averages of the actual changes and of the goals, both present; for
/// accumulated balances, the last period's cell as the total
/// (makeBudgetReportRow).
fn make_budget_report_row(
    ropts: &ReportOpts,
    balance: Balance,
    data: &PeriodData<(Option<BalanceData>, Option<BalanceData>)>,
) -> PeriodicReportRow<(), BudgetCell> {
    let amounts: Vec<BudgetCell> = (data.periods.values())
        .map(|(actual, goal)| {
            let actual = actual
                .as_ref()
                .map_or_else(MixedAmount::null, |b| balance(b).clone());
            (Some(actual), goal.as_ref().map(|b| balance(b).clone()))
        })
        .collect();
    let (actual_total, actual_average) =
        MixedAmount::sum_and_average(amounts.iter().filter_map(|c| c.0.as_ref()));
    let (goal_total, goal_average) =
        MixedAmount::sum_and_average(amounts.iter().filter_map(|c| c.1.as_ref()));
    let total = match ropts.balance_accum {
        BalanceAccumulation::PerPeriod => (Some(actual_total), Some(goal_total)),
        _ => (amounts.last().cloned()).unwrap_or((Some(MixedAmount::null()), None)),
    };
    PeriodicReportRow {
        name: (),
        amounts,
        total,
        average: (Some(actual_average), Some(goal_average)),
    }
}

/// The journal with its periodic transactions (those whose description
/// --budget's argument is part of, ignoring case) generating budget goal
/// transactions, balanced, in place of its transactions: in the report
/// span, and before it from the journal's or report's start, whichever is
/// earlier, moved back to the start of the longest interval of those
/// periodic transactions unless that one starts later
/// (journalAddBudgetGoalTransactions).
fn journal_add_budget_goal_transactions(
    bopts: &BalancingOpts,
    ropts: &ReportOpts,
    report_span: DateSpan,
    j: &Journal,
) -> Result<Journal, String> {
    let pattern = ropts.budget_pattern.as_deref().unwrap_or("").to_lowercase();
    let pts: Vec<_> = (j.periodic_txns.iter())
        .filter(|pt| pt.description.to_lowercase().contains(&pattern))
        .collect();
    let start = [j.date_span(false).start(), report_span.start()]
        .into_iter()
        .flatten()
        .min()
        .map(|d| {
            // the interval and dates of the periodic transaction with the
            // longest interval, the last of those if several
            let (interval, span) = (pts.iter())
                .max_by(|a, b| a.interval.cmp(&b.interval))
                .map_or((Interval::Days(1), DateSpan::default()), |pt| {
                    (pt.interval.clone(), pt.span)
                });
            let interval_start = interval_boundary_before(&interval, d);
            d.min(
                span.start()
                    .map_or(interval_start, |s| s.max(interval_start)),
            )
        });
    let budget_span = DateSpan::exact(start, report_span.end());
    let txns = (pts.iter())
        .flat_map(|pt| run_periodic_transaction(false, pt, budget_span))
        .map(|t| Transaction {
            description: "Budget transaction".to_string(),
            ..t
        })
        .collect();
    let mut budgetj = Journal { txns, ..j.clone() };
    journal_number_and_tie_transactions(&mut budgetj);
    journal_style_amounts(&mut budgetj)?;
    Ok(journal_balance_transactions(bopts, budgetj, false)?.0)
}

/// The journal with its postings' accounts as the budget report shows
/// them: an account with no goal of its own or above it replaced by the
/// `<unbudgeted>` top-level account (with --empty, moved under it), and
/// unless --empty, an account with no goal of its own but one above it
/// replaced by its nearest parent with one; a changed posting keeps the
/// original, whose account the query's account terms also match
/// (journalWithBudgetAccountNames, which keeps every posting's original,
/// the same posting when its account is unchanged).
fn journal_with_budget_account_names(
    budgeted: &HashSet<&str>,
    show_unbudgeted: bool,
    j: &Journal,
) -> Journal {
    let mut j = j.clone();
    for t in &mut j.txns {
        for p in &mut t.postings {
            let a = &*p.account;
            let remapped = if budgeted.contains(a) {
                continue;
            } else if let Some(parent) =
                crate::account::parent_account_names(a).find(|p| budgeted.contains(p))
            {
                if show_unbudgeted {
                    continue;
                }
                parent.to_string()
            } else if show_unbudgeted {
                format!("{UNBUDGETED_ACCOUNT_NAME}{ACCT_SEP}{a}")
            } else {
                UNBUDGETED_ACCOUNT_NAME.to_string()
            };
            p.original = Some(Box::new(p.original().clone()));
            p.account = AccountName::from(remapped);
        }
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::Day;
    use crate::query::parse_query;

    #[test]
    fn budget_goal_queries() {
        let today = Day::from_ymd(2026, 1, 1).unwrap();
        let q = |s: &str| parse_query(today, s).unwrap().0;
        assert_eq!(
            budget_goal_query(&q("expenses desc:x not:cur:X status:* date:2025 tag:t")),
            q("expenses not:cur:X date:2025")
        );
        assert_eq!(budget_goal_query(&q("desc:x")), Query::Any);
        assert_eq!(budget_goal_query(&q("not:desc:x type:X")), q("type:X"));
    }
}
