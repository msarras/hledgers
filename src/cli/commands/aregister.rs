//! The aregister command (Hledger.Cli.Commands.Aregister): one account's
//! transactions, like the account register of hledger-ui and hledger-web,
//! with the other accounts involved, the change to the account, and the
//! account's running balance; as text laid out like register's, or as
//! csv, tsv, json, html or fods, or as a chart.

use std::collections::BTreeMap;

use crate::account::{account_name_drop, account_name_to_account_regex, account_summarised_name};
use crate::amount::render::AmountFormat;
use crate::amount::{Rounding, styles_with_rounding};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::chart::{
    account_transactions_report_as_chart, chart_requested, check_chart_format, render_chart,
};
use crate::cli::commands::balance::CHART_FLAG;
use crate::cli::commands::print::unsupported_output_format_error;
use crate::cli::commands::register::{
    ALIGN_ALL_FLAG, WIDTH_FLAG, postings_or_transactions_report_as_text, register_columns,
    register_line,
};
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::model::Journal;
use crate::query::Query;
use crate::read::finalise::journal_account_names;
use crate::reports::account_txns::{
    AccountTransactionsReportItem, account_transactions_report, transaction_register_date,
};
use crate::reports::opts::{BalanceAccumulation, ReportOpts, ReportSpec};
use crate::write::csv::{print_csv, print_tsv};
use crate::write::fods::{locale_encoding_name, print_fods};
use crate::write::html::{escape, format_row, format_title, stylesheet, table_style};
use crate::write::json::to_json_text;
use crate::write::spreadsheet::{
    self as spr, CellType, add_header_borders, cell_from_mixed_amount, horizontal_span,
    integer_cell, raw_table_content,
};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "aregister",
    aliases: &["areg"],
    flags: &[
        Flag::new(&["txn-dates"], NONE, "", "filter strictly by transaction date, not posting date. Warning: this can show a wrong running balance."),
        Flag::new(&["no-elide"], NONE, "", "don't show only 2 commodities per amount"),
        Flag::new(&["cumulative"], NONE, "", "accumulation mode: show running total from report start date"),
        Flag::new(&["historical"], NONE, "", "accumulation mode: show historical running total/balance (includes postings before report start date) (default)").short(&['H']),
        Flag::new(&["average"], NONE, "", "show running average of transaction amounts instead of balance (implies --empty and, unless -H is used, --cumulative)").short(&['A']),
        Flag::new(&["related"], NONE, "", "show the other accounts in each transaction (default)").short(&['r']),
        Flag::new(&["matched"], NONE, "", "show the matched accounts (this account or its subaccounts) instead"),
        Flag::new(&["invert"], NONE, "", "display all amounts with reversed sign"),
        Flag::new(&["drop"], REQ, "N", "omit N leading account name parts"),
        Flag::new(&["heading"], REQ, "YN", "show heading row above table: yes (default) or no"),
        WIDTH_FLAG,
        ALIGN_ALL_FLAG,
        output_format_flag("select the output format. Supported formats:\ntxt, html, csv, tsv, json, svg."),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        CHART_FLAG,
    ],
    args: Some("ACCTPAT [QUERY]"),
    doc: crate::command_doc!("Aregister"),
    ..Mode::DEFAULT
};

/// What the report shows and how, besides the options: the report query
/// and the account query.
struct Aregister<'a> {
    opts: &'a CliOpts,
    reportq: &'a Query,
    thisacctq: &'a Query,
}

/// Run the aregister command (aregister): the transactions of the first
/// account the first argument matches (a case-insensitive regular
/// expression), filtered by the other arguments, with the account's
/// historical running balance by default; depth limits ignored; with
/// --average, the running average change, over the report period unless
/// --historical, zero changes included.
pub fn aregister(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let help = "aregister needs an ACCTPAT argument to select an account";
    let mut args = opts.raw.args();
    let Some(apat) = args.next() else {
        return Err(format!(
            "{help}.\nPlease provide an account name or a (case-insensitive, infix, regexp) pattern."
        ));
    };
    let querystr: Vec<String> = args.map(str::to_string).collect();
    let names = journal_account_names(j);
    let acct = crate::regex::to_regex_ci(apat)
        .ok()
        .and_then(|re| names.iter().find(|a| re.is_match(a)));
    // no matching account is an error only once the account is needed, as
    // upstream's lazy one is: to select transactions, or for the heading
    let fmt = opts.output_format();
    let shows_default_title = opts.spec.opts.title.is_none()
        && opts
            .raw
            .string("heading")
            .is_none_or(|h| !matches!(h.to_lowercase().as_str(), "n" | "no" | "never"))
        && (matches!(fmt.as_str(), "txt" | "html")
            || (chart_requested(opts) && !matches!(fmt.as_str(), "csv" | "tsv")));
    let not_found = format!(
        "{help}, but {} did not match any account.",
        crate::text::show_string(apat)
    );
    let (acct, thisacctq) = match acct {
        Some(a) => (a.as_ref(), Query::Acct(account_name_to_account_regex(a))),
        None if !shows_default_title => ("", Query::None),
        None => return Err(not_found),
    };
    let ropts = &opts.spec.opts;
    let ropts2 = ReportOpts {
        depth: Default::default(),
        balance_accum: match ropts.balance_accum {
            BalanceAccumulation::PerPeriod if ropts.average => BalanceAccumulation::Cumulative,
            BalanceAccumulation::PerPeriod => BalanceAccumulation::Historical,
            a => a,
        },
        empty: ropts.empty || ropts.average,
        query_string: querystr,
        ..ropts.clone()
    };
    let spec = ReportSpec::new(opts.spec.today, ropts2)?;
    let found = if acct.is_empty() {
        Err(not_found.as_str())
    } else {
        Ok(&thisacctq)
    };
    let (_, items) = account_transactions_report(&spec, j, found)?;
    let styles = styles_with_rounding(Rounding::Hard, &j.commodity_styles());
    let mut items: Vec<AccountTransactionsReportItem> = (items.into_iter().rev())
        .filter(|i| spec.opts.empty || !i.amount.looks_zero())
        .collect();
    items.iter_mut().for_each(|i| i.style_amounts(&styles));
    let ar = Aregister {
        opts,
        reportq: &spec.query,
        thisacctq: &thisacctq,
    };
    if chart_requested(opts) {
        check_chart_format(opts)?;
        let title = ar.title()?;
        let types = |a: &str| j.account_type(a);
        let no_types = |_: &str| None;
        let date = |t: &crate::model::Transaction| {
            transaction_register_date(&no_types, spec.opts.date2, ar.reportq, ar.thisacctq, t)
        };
        let chart = account_transactions_report_as_chart(&types, &title, acct, &date, &items);
        return render_chart(opts, chart, || ar.as_html(&items));
    }
    match fmt.as_str() {
        "txt" => ar.as_text(&items),
        "html" => ar.as_html(&items),
        "csv" => Ok(print_csv(&ar.as_csv(&items)?)),
        "tsv" => Ok(print_tsv(&ar.as_csv(&items)?)),
        "fods" => {
            let rows = ar.as_spreadsheet(&AmountFormat::ONE_LINE_NO_COST, ar.heading()?, &items);
            Ok(print_fods(
                locale_encoding_name(),
                &BTreeMap::from([("Aregister".to_string(), ((1, 0), rows))]),
            ))
        }
        "json" => Ok(to_json_text(&items)),
        _ => Err(unsupported_output_format_error(&fmt)),
    }
}

impl Aregister<'_> {
    /// Whether to show the heading (--heading, yes by default) (headingopt).
    fn heading(&self) -> Result<bool, String> {
        match self
            .opts
            .raw
            .string("heading")
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("y" | "yes" | "always") => Ok(true),
            Some("n" | "no" | "never") => Ok(false),
            Some(_) => Err("heading value should be one of y, yes, n, no".to_string()),
        }
    }

    /// Whether the account column shows the matched accounts (--matched)
    /// rather than the other accounts (--related, the default); the last
    /// of these flags wins (matchedopt).
    fn matched(&self) -> bool {
        (self.opts.raw)
            .choice(|n| match n {
                "matched" => Some(true),
                "related" => Some(false),
                _ => None,
            })
            .unwrap_or(false)
    }

    /// The report's heading: which account it shows, and whether an extra
    /// query restricts it; or --title's, if given; nothing with
    /// --heading=no (accountTransactionsReportTitle).
    fn title(&self) -> Result<String, String> {
        if !self.heading()? {
            return Ok(String::new());
        }
        // the account name, as escaped in the account query's regex
        let acct = match self.thisacctq.clone().filter(Query::is_acct) {
            Query::Acct(r) => {
                let src = r.src();
                let n = src.chars().count();
                Some(
                    src.chars()
                        .skip(1)
                        .take(n.saturating_sub(6))
                        .collect::<String>(),
                )
            }
            _ => None,
        };
        let ropts = &self.opts.spec.opts;
        let extra_query = ropts.query_string.len() > 1
            && !(self.reportq.clone())
                .filter(|q| !(q.is_depth() || q.is_date_or_date2()))
                .is_null();
        let default = acct.map_or(String::new(), |a| {
            let qmsg = if extra_query { " (matching query)" } else { "" };
            format!("Transactions in {a} and subaccounts{qmsg}:")
        });
        Ok(ropts.effective_title(&default).to_string())
    }

    /// The accounts to show for an item: the other accounts in the
    /// transaction, or with --matched, the accounts the account query
    /// matches among the postings the report query selected, without
    /// repeats (itemAccountNames).
    fn item_account_names(&self, i: &AccountTransactionsReportItem) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut add = |a: &str| {
            if !names.iter().any(|n| n == a) {
                names.push(a.to_string());
            }
        };
        if self.matched() {
            let t = &i.acct_txn;
            (t.postings.iter())
                .filter(|p| (self.thisacctq).matches_posting(crate::model::PostingRef::new(t, p)))
                .for_each(|p| add(&p.account));
        } else {
            i.other_accounts.iter().for_each(|a| add(a));
        }
        names
    }

    /// The date shown for a transaction (transactionRegisterDate, without
    /// account types).
    fn date(&self, t: &crate::model::Transaction) -> crate::dates::Day {
        let date2 = self.opts.spec.opts.date2;
        transaction_register_date(&|_| None, date2, self.reportq, self.thisacctq, t)
    }

    /// The report as text, after its heading line (accountTransactionsReportAsText).
    fn as_text(&self, items: &[AccountTransactionsReportItem]) -> Result<String, String> {
        let title = self.title()?;
        let mut out = if title.is_empty() {
            String::new()
        } else {
            format!("{title}\n")
        };
        let drop = self.opts.spec.opts.drop;
        out += &postings_or_transactions_report_as_text(
            self.opts,
            items,
            |i| &i.amount,
            |i| &i.balance,
            |widths, i, amt, bal| {
                let cols = register_columns(widths, 10, true);
                let accts: Vec<String> = (self.item_account_names(i).iter())
                    .map(|a| account_name_drop(drop, &account_summarised_name(a)))
                    .collect();
                register_line(
                    &cols,
                    &self.date(&i.txn).to_string(),
                    &i.txn.description,
                    &accts.join(", "),
                    amt,
                    bal,
                )
            },
        )?;
        Ok(out)
    }

    /// The report as html: the heading, then a table of the items, with a
    /// heading row unless --heading=no (accountTransactionsReportAsHTML).
    fn as_html(&self, items: &[AccountTransactionsReportItem]) -> Result<String, String> {
        let rules = table_style();
        let rules: Vec<(&str, &str)> = rules
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let mut out = format!("<style>{}</style>\n", stylesheet(&rules));
        out.push_str("<link rel=\"stylesheet\" href=\"hledger.css\">\n");
        let title = self.title()?;
        if !title.is_empty() {
            out.push_str(&format_title(&title));
        }
        out.push_str("<table>\n");
        if self.heading()? {
            out.push_str("<thead><tr>");
            for h in ["date", "description", "otheraccounts", "amount", "balance"] {
                out.push_str(&format!("<th>{}</th>", escape(h)));
            }
            out.push_str("</tr></thead>\n");
        }
        out.push_str("<tbody>");
        for i in items {
            out.push_str(&format_row(&self.item_as_record(
                &AmountFormat::ONE_LINE_NO_COST,
                false,
                i,
            )));
        }
        out.push_str("</tbody></table>\n");
        Ok(out)
    }

    /// The report as csv or tsv records: the heading row unless
    /// --heading=no, after --title's title if any
    /// (accountTransactionsReportAsCsv).
    fn as_csv(&self, items: &[AccountTransactionsReportItem]) -> Result<Vec<Vec<String>>, String> {
        let rows = self.as_spreadsheet(&AmountFormat::MACHINE, self.heading()?, items);
        let title = self.opts.spec.opts.effective_title("");
        let mut out = Vec::new();
        if let (false, Some(first)) = (title.is_empty(), rows.first()) {
            out.push(horizontal_span(first.len(), spr::Cell::header(title)));
        }
        out.extend(rows);
        Ok(raw_table_content(out))
    }

    /// The report as spreadsheet rows: a heading row if `heading`, then a
    /// row per item (accountTransactionsReportAsSpreadsheet).
    fn as_spreadsheet(
        &self,
        fmt: &AmountFormat,
        heading: bool,
        items: &[AccountTransactionsReportItem],
    ) -> spr::Rows {
        let mut rows = Vec::new();
        if heading {
            let headings = [
                "txnidx",
                "date",
                "code",
                "description",
                "otheraccounts",
                "amount",
                "balance",
            ];
            rows.push(add_header_borders(
                headings.into_iter().map(spr::Cell::header).collect(),
            ));
        }
        rows.extend(items.iter().map(|i| self.item_as_record(fmt, true, i)));
        rows
    }

    /// An item as a row: with `internals`, the transaction's number and
    /// code; its date, description, the accounts, the change and the
    /// balance (accountTransactionsReportItemAsRecord).
    fn item_as_record(
        &self,
        fmt: &AmountFormat,
        internals: bool,
        i: &AccountTransactionsReportItem,
    ) -> Vec<spr::Cell<String>> {
        let t = &i.txn;
        let drop = self.opts.spec.opts.drop;
        let accts: Vec<String> = (self.item_account_names(i).iter())
            .map(|a| account_name_drop(drop, a))
            .collect();
        let date = spr::Cell {
            typ: CellType::Date,
            ..spr::Cell::new(self.date(t).to_string())
        };
        let amount = |m| cell_from_mixed_amount(fmt, "amount", m).into_text();
        let mut row = Vec::new();
        if internals {
            row.push(integer_cell(t.index));
        }
        row.push(date);
        if internals {
            row.push(spr::Cell::new(t.code.clone()));
        }
        row.extend([
            spr::Cell::new(t.description.clone()),
            spr::Cell::new(accts.join(", ")),
            amount(&i.amount),
            amount(&i.balance),
        ]);
        row
    }
}
