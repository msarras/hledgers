//! The built-in self-tests that --test runs, with output as tasty's
//! console reporter shows it (runUnitTests): the unit tests and doctests
//! ported from upstream, which are also `cargo test`'s tests in their
//! modules.

use std::cell::RefCell;
use std::panic;
use std::time::{Duration, Instant};

/// A self-test, which panics if it fails.
type Test = fn();

const DATA: &str = "Data";
const READ: &str = "Read";
const REPORTS: &str = "Reports";
const UTILS: &str = "Utils";

/// The self-tests in upstream's order: each one's group path below
/// `hledger`, its name, and the test. A test porting several upstream
/// tests is named after them all (`A, B`, or `A ... B` for a run of them),
/// one porting a whole group after the group; one porting doctests, which
/// upstream's --test does not run, after the functions they document, in
/// a group named after their module.
#[rustfmt::skip]
const TESTS: &[(&[&str], &str, Test)] = {
    use crate::account::tests as account;
    use crate::reports::account_tree::tests as tree;
    use crate::amount::currency::tests as currency;
    use crate::amount::render::tests as render;
    use crate::amount::tests as amount;
    use crate::cli::anchor::tests as anchor;
    use crate::cli::args::tests as args;
    use crate::cli::commands::balance::tests as balance;
    use crate::cli::commands::register::tests as register;
    use crate::dates::tests as dates;
    use crate::i18n;
    use crate::model::tests as model;
    use crate::query::tests as query;
    use crate::read::amount::tests as read_amount;
    use crate::read::balancing::tests as balancing;
    use crate::read::common::tests as common;
    use crate::read::journal::tests as journal;
    use crate::read::modifiers::tests as modifiers;
    use crate::read::rules::tests as rules;
    use crate::read::timeclock::tests as timeclock;
    use crate::regex::tests as regex;
    use crate::reports::account_txns::tests as account_txns;
    use crate::reports::entries::tests as entries;
    use crate::reports::periodic::tests as periodic;
    use crate::reports::postings::tests as postings;
    use crate::text::tests as text;
    use crate::valuation::tests as valuation;
    use crate::write::beancount::tests as beancount;
    use crate::write::chart::tests as chart;
    use crate::write::chart_svg::tests as chart_svg;
    use crate::write::chart_text::tests as chart_text;
    use crate::write::html::tests as html;
    use crate::model::render::tests as write_journal;
    use crate::reports::string_format::tests as string_format;
    &[
        (&["Hledger", DATA, "Account"], "accountFromPostings", tree::account_from_no_postings),
        (&["Hledger", DATA, "AccountName"], "expandAccountNames", account::expand_names),
        (&["Hledger", DATA, "AccountName"], "isAccountNamePrefixOf", account::prefix_of),
        (&["Hledger", DATA, "AccountName"], "isSubAccountNameOf", account::sub_account_of),
        (&["Hledger", DATA, "AccountName"], "accountNameInferType", account::infer_type),
        (&["Hledger", DATA, "AccountName"], "joinAccountNames", account::join_names),
        (&["Hledger", DATA, "AccountName"], "concatAccountNames", account::concat_names),
        (&["Hledger", DATA, "Amount", "Amount"], "amountCost", amount::amount_cost),
        (&["Hledger", DATA, "Amount", "Amount"], "multiplyQuantities", crate::decimal::tests::multiplication_is_exact_and_normalized),
        (&["Hledger", DATA, "Amount", "Amount"], "amountLooksZero", amount::amount_looks_zero),
        (&["Hledger", DATA, "Amount", "Amount"], "negating amounts", amount::negating_amounts),
        (&["Hledger", DATA, "Amount", "Amount"], "adding amounts without costs", amount::adding_amounts_without_costs),
        (&["Hledger", DATA, "Amount", "Amount"], "showAmount", render::show_amount_of_zero),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "comparing mixed amounts compares based on quantities", amount::comparing_mixed_amounts),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "adding mixed amounts to zero, the commodity and amount style are preserved", amount::adding_mixed_amounts_to_zero),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "adding mixed amounts with total costs", amount::adding_mixed_amounts_with_total_costs),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "showMixedAmount", render::show_mixed_amounts),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "showMixedAmountWithoutCost", render::show_mixed_amount_without_cost),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "amounts", amount::amounts),
        (&["Hledger", DATA, "Amount", "MixedAmount"], "mixedAmountStripCosts", amount::mixed_amount_strip_costs),
        (&["Hledger", DATA, "BalanceData"], "opBalanceData maPlus, opBalanceData maMinus", tree::balance_data_ops),
        (&["Hledger", DATA, "Balancing"], "transactionInferBalancingAmount", balancing::inferring_balancing_amounts),
        (&["Hledger", DATA, "Balancing"], "balanceSingleTransaction", balancing::balancing_single_transactions),
        (&["Hledger", DATA, "Balancing"], "isTransactionBalanced", balancing::checking_balance),
        (&["Hledger", DATA, "Balancing"], "journalBalanceTransactions", balancing::balancing_journals),
        (&["Hledger", DATA, "Balancing"], "transactionCheckAssertions", balancing::checking_a_transaction_s_assertions),
        (&["Hledger", DATA, "Balancing"], "commodityStylesFromAmounts", balancing::commodity_styles_1091),
        (&["Hledger", DATA, "Currency"], "currencySymbolToCode, toCurrencyCode doctests", currency::symbols_and_codes),
        (&["Hledger", DATA, "Dates"], "showDateSpanForQuery doctests", dates::spans_for_queries),
        (&["Hledger", DATA, "Dates"], "spanIntersect ... spanExtend doctests", dates::span_operations),
        (&["Hledger", DATA, "Dates"], "fixSmartDate doctests", dates::fixing_smart_dates),
        (&["Hledger", DATA, "Dates"], "nextmonthandday ... prevNthWeekdayOfMonth doctests", dates::next_and_previous_dates),
        (&["Hledger", DATA, "Dates"], "parsedate doctests", dates::simple_dates),
        (&["Hledger", DATA, "Dates"], "smartdate doctests", dates::smart_dates),
        (&["Hledger", DATA, "Dates"], "periodexprp doctests", dates::period_expressions),
        (&["Hledger", DATA, "Dates"], "doubledatespanp ... quarterstartp doctests", dates::span_forms),
        (&["Hledger", DATA, "splitSpan"], "weekday", dates::splitting_by_weekdays),
        (&["Hledger", DATA, "splitSpan"], "match dayOfWeek", dates::splitting_by_one_weekday),
        (&["Hledger", DATA, "splitSpan"], "splitSpanToDateSpans and splitSpanStartEnd agree with splitSpan", dates::split_forms_agree),
        (&["Hledger", DATA, "splitSpan"], "boundariesToDayPartition doctests", dates::day_partitions),
        (&["Hledger", DATA, "splitSpan"], "splitSpan, splitSpanToDateSpans doctests", dates::splitting_spans),
        (&["Hledger", DATA, "Journal"], "journalDateSpan", model::journal_date_span),
        (&["Hledger", DATA, "Period"], "Period doctests", dates::periods),
        (&["Hledger", DATA, "PeriodData"], "periodDataFromList, Semigroup instance, Monoid instance", tree::period_data_semigroup),
        (&["Hledger", DATA, "PeriodicTransaction"], "runPeriodicTransaction doctests", modifiers::periodic_transactions),
        (&["Hledger", DATA, "Posting"], "accountNamePostingType, accountNameWithoutPostingType, accountNameWithPostingType", account::posting_types),
        (&["Hledger", DATA, "Posting"], "commentAddTag, commentAddTagNextLine", model::comment_tags),
        (&["Hledger", DATA, "Valuation"], "priceLookup", valuation::price_lookup),
        (&["Hledger", DATA, "Valuation"], "marketPriceReverse", valuation::market_price_reversed),
        (&["Hledger", DATA, "StringFormat"], "formatStringHelper", string_format::formatting_fields),
        (&["Hledger", DATA, "StringFormat"], "parseStringFormat", string_format::parsing_formats),
        (&["Hledger", DATA, "Timeclock"], "timeclockToTransactions tests", timeclock::timeclock_to_transactions_tests),
        (&["Hledger", DATA, "Transaction"], "postingsAsLines", write_journal::posting_lines),
        (&["Hledger", DATA, "Transaction"], "showTransaction", write_journal::transactions),
        (&["Hledger", DATA, "TransactionModifier"], "transactionModifierToFunction doctests", modifiers::auto_postings),
        (&["Hledger", "Query"], "simplifyQuery", query::simplifying),
        (&["Hledger", "Query"], "parseQuery", query::parsing_queries),
        (&["Hledger", "Query"], "parseBooleanQuery", query::parsing_boolean_queries),
        (&["Hledger", "Query"], "words''", query::splitting_words),
        (&["Hledger", "Query"], "filterQuery", query::filtering),
        (&["Hledger", "Query"], "parseQueryTerm", query::parsing_terms),
        (&["Hledger", "Query"], "parseAmountQueryTerm", query::parsing_amount_terms),
        (&["Hledger", "Query"], "queryStartDate, queryEndDate", query::start_and_end_dates),
        (&["Hledger", "Query"], "matchesAccount, matchesAccountExtra", query::matching_accounts),
        (&["Hledger", "Query"], "matchesPosting", query::matching_postings),
        (&["Hledger", "Query", "matchesPosting"], "any: and all: match a posting by looking at its siblings", query::matching_siblings),
        (&["Hledger", "Query"], "matchesTransaction", query::matching_transactions),
        (&["Hledger", "Query"], "parseQuery, parseBooleanQuery doctests", query::doctests),
        (&["Hledger", READ, "Common"], "amountp", read_amount::amounts),
        (&["Hledger", READ, "Common"], "postingamountp", read_amount::amount_expressions),
        (&["Hledger", READ, "Common"], "numberp", read_amount::numbers),
        (&["Hledger", READ, "Common"], "numberp with a suggested decimal mark", read_amount::numbers_with_a_suggested_decimal_mark),
        (&["Hledger", READ, "Common"], "amountp with a decimal-mark directive", read_amount::amounts_with_a_declared_decimal_mark),
        (&["Hledger", READ, "JournalReader"], "accountnamep", journal::account_names),
        (&["Hledger", READ, "JournalReader"], "datep", common::dates),
        (&["Hledger", READ, "JournalReader"], "periodictransactionp", journal::periodic_transactions),
        (&["Hledger", READ, "JournalReader"], "postingp", journal::postings),
        (&["Hledger", READ, "JournalReader"], "transactionmodifierp", journal::transaction_modifiers),
        (&["Hledger", READ, "JournalReader"], "transactionp", journal::transactions),
        (&["Hledger", READ, "JournalReader"], "directivep ... endtagdirectivep, journalp", journal::directives),
        (&["Hledger", READ, "RulesReader"], "parseCsvRules, rulesp", rules::rules_parsing),
        (&["Hledger", READ, "RulesReader"], "conditionalblockp ... Combine multiple matchers on the same line", rules::matchers_and_blocks),
        (&["Hledger", READ, "RulesReader"], "hledgerField", rules::field_templates),
        (&["Hledger", READ, "RulesReader"], "hledgerFieldValue", rules::field_values_with_match_groups),
        (&["Hledger", READ, "RulesReader"], "isAmountExpression, simplifySign doctests", rules::amount_texts),
        (&["Hledger", REPORTS, "BalanceReport"], "balanceReport", periodic::balance_reports),
        (&["Hledger", REPORTS, "AccountTransactionsReport"], "accountTransactionsReportWithStart", account_txns::account_transactions_report_with_start),
        (&["Hledger", REPORTS, "AccountTransactionsReport"], "transactionRegisterDateExtra", account_txns::transaction_register_date_with_types),
        (&["Hledger", REPORTS, "EntriesReport"], "entriesReport", entries::entries_report_selects),
        (&["Hledger", REPORTS, "MultiBalanceReport"], "multiBalanceReport", periodic::multi_balance_reports),
        (&["Hledger", REPORTS, "PostingsReport"], "postingsReport", postings::postings_report_counts),
        (&["Hledger", REPORTS, "PostingsReport"], "summarisePostingsByInterval", postings::summarise_nothing),
        (&["Hledger", UTILS, "I18n"], "substitutePlaceholders", i18n::test_substitute_placeholders),
        (&["Hledger", UTILS, "I18n"], "placeholders", i18n::test_placeholders),
        (&["Hledger", UTILS, "I18n"], "parsePo", i18n::test_parse_po),
        (&["Hledger", UTILS, "I18n"], "parsePo errors", i18n::test_parse_po_errors),
        (&["Hledger", UTILS, "I18n"], "parsePo tolerates", i18n::test_parse_po_tolerates),
        (&["Hledger", UTILS, "I18n"], "plural rules", i18n::test_plural_rules),
        (&["Hledger", UTILS, "I18n"], "normalizeLangTag", i18n::test_normalize_lang_tag),
        (&["Hledger", UTILS, "I18n"], "shortLangTag", i18n::test_short_lang_tag),
        (&["Hledger", UTILS, "I18n"], "resolveLang", i18n::test_resolve_lang),
        (&["Hledger", UTILS, "I18n"], "langPrefsFromEnv", i18n::test_lang_prefs_from_env),
        (&["Hledger", UTILS, "I18n"], "built-in catalogs", i18n::test_builtin_catalogs),
        (&["Hledger", UTILS, "Regex"], "regexReplaceUnmemo doctests", regex::replace),
        (&["Hledger", UTILS, "String"], "words', wordsmay doctests", text::quoted_words),
        (&["Hledger", UTILS, "Text"], "quoteIfSpaced", text::quoting),
        (&["Hledger", UTILS, "Text"], "fitText", text::fitting_text),
        (&["Hledger", "Write.Beancount"], "commodityToBeancount doctests", beancount::commodity_names),
        (&["Hledger", "Write.Chart"], "scaleSpans", chart::scale_spans_cases),
        (&["Hledger", "Write.Chart"], "totalIsBarEnd", chart::total_is_bar_end_cases),
        (&["Hledger", "Write.Chart"], "seriesColours", chart::series_colour_choices),
        (&["Hledger", "Write.Chart"], "stableSlots", chart::stable_slot_choices),
        (&["Hledger", "Write.Chart.Palette"], "colourHex, tint", chart::palette),
        (&["Hledger", "Write.Chart.Svg"], "num", chart_svg::nums),
        (&["Hledger", "Write.Chart.Svg"], "niceScale", chart_svg::nice_scales),
        (&["Hledger", "Write.Chart.Svg"], "plotIntervals", chart_svg::plot_interval_counts),
        (&["Hledger", "Write.Chart.Svg"], "barPath", chart_svg::bar_paths),
        (&["Hledger", "Write.Chart.Svg"], "a long line has tooltips all along it, at most one per 4px column", chart_svg::long_line_tooltips),
        (&["Hledger", "Write.Chart.Svg"], "a line by date has tooltips where it changes", chart_svg::line_by_date_tooltips_where_it_changes),
        (&["Hledger", "Write.Chart.Svg"], "dropFlat", chart_svg::flat_runs),
        (&["Hledger", "Write.Chart.Svg"], "a net total is marked across each period's bars ... since a line of one point would not show", chart_svg::net_total_marks),
        (&["Hledger", "Write.Chart.Svg"], "a line of one point is drawn as a short dash, so that it shows", chart_svg::one_point_line_is_a_dash),
        (&["Hledger", "Write.Chart.Text"], "trimEnd", chart_text::trimming),
        (&["Hledger", "Write.Chart.Text"], "single series, scaled to the width", chart_text::single_series_scaled_to_the_width),
        (&["Hledger", "Write.Chart.Text"], "mixed signs grow from an axis", chart_text::mixed_signs_grow_from_an_axis),
        (&["Hledger", "Write.Html"], "formatCell", html::format_cells),
        (&["Hledger", "Write.Html"], "formatRow, titledTableHtml", html::rows_and_tables),
        (&["Hledger.Cli"], "dropUnsupportedOpts doctests", args::unsupported_flags_and_cli_specific_flags_are_dropped),
        (&["Hledger.Cli", "Anchor"], "composeAnchor, composeAnchorWith doctests", anchor::anchors),
        (&["Hledger.Cli", "Commands", "Balance", "balanceReportAsText"], "unicode in balance layout", balance::unicode_in_balance_layout),
        (&["Hledger.Cli", "Commands", "Register", "postingsReportAsText"], "unicode in register layout", register::unicode_in_register_layout),
    ]
};

thread_local! {
    /// The last panic's message and place.
    static PANIC: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Run the self-tests whose full name (`hledger.GROUP...NAME`) contains
/// the pattern, all of them if it is empty, print the tree of results and
/// a summary, and return the exit status.
pub fn run_unit_tests(pattern: &str) -> i32 {
    let start = Instant::now();
    let tests: Vec<_> = (TESTS.iter())
        .filter(|(path, name, _)| {
            let full = format!("hledger.{}.{name}", path.join("."));
            full.contains(pattern)
        })
        .collect();
    // a failing test's panic is caught, its message kept instead of printed
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|info| PANIC.set(info.to_string())));
    let results: Vec<(Result<(), String>, Duration)> = (tests.iter())
        .map(|(_, _, t)| {
            let t0 = Instant::now();
            let result = panic::catch_unwind(*t).map_err(|_| PANIC.take());
            (result, t0.elapsed())
        })
        .collect();
    panic::set_hook(hook);
    let width = (tests.iter())
        .map(|(path, name, _)| 2 * (path.len() + 1) + name.chars().count() + 1)
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    let mut shown: &[&str] = &[];
    for ((path, name, _), (result, time)) in tests.iter().zip(&results) {
        if out.is_empty() {
            out.push_str("hledger\n");
        }
        let common = shown
            .iter()
            .zip(path.iter())
            .take_while(|(a, b)| a == b)
            .count();
        for (i, g) in path.iter().enumerate().skip(common) {
            out.push_str(&format!("{}{g}\n", "  ".repeat(i + 1)));
        }
        shown = path;
        let label = format!("{}{name}:", "  ".repeat(path.len() + 1));
        let pad = " ".repeat(width - label.chars().count());
        let status = if result.is_ok() { "OK" } else { "FAIL" };
        let timing = if time.as_secs_f64() >= 0.01 {
            format!(" ({:.2}s)", time.as_secs_f64())
        } else {
            String::new()
        };
        out.push_str(&format!("{label}{pad} {status}{timing}\n"));
        if let Err(e) = result {
            for l in e.lines() {
                out.push_str(&format!("{}{l}\n", "  ".repeat(path.len() + 2)));
            }
        }
    }
    let failed = results.iter().filter(|(r, _)| r.is_err()).count();
    let total = format!("({:.2}s)", start.elapsed().as_secs_f64());
    if failed == 0 {
        out.push_str(&format!("\nAll {} tests passed {total}\n", tests.len()));
    } else {
        out.push_str(&format!(
            "\n{failed} out of {} tests failed {total}\n",
            tests.len()
        ));
    }
    super::io::print_stdout(&out);
    i32::from(failed > 0)
}
