//! Turning reports into charts, and the --chart option (Hledger.Cli.Chart).
//! Each adapter chooses a useful default chart for its report, so that
//! charts work without configuration. Register and aregister charts come
//! with those commands (hledgers milestone 6).

use std::collections::{BTreeMap, BTreeSet};

use crate::account::{AccountType, parent_account_names};
use crate::amount::{Amount, AmountStyle, MixedAmount, Sym};
use crate::cli::commands::print::unsupported_output_format_error;
use crate::cli::opts::CliOpts;
use crate::decimal::Decimal;
use crate::model::{Commodity, SourcePos};
use crate::read::amount::{AmountCtx, parse_amount_with};
use crate::reports::opts::{BalanceAccumulation, ReportOpts};
use crate::reports::periodic::{
    BalanceReport, CompoundPeriodicReport, DisplayName, MultiBalanceReport, MultiBalanceReportRow,
    PeriodicReportRow,
};
use crate::write::chart::{
    BarLayout, Chart, ChartLabel, MAX_TINTS, Mark, Panel, Series, SeriesRole, Summary, SummaryKind,
    XAxis, chart_as_csv, main_account_type, set_chart_layout, set_chart_scales,
};
use crate::write::chart_svg::chart_as_svg;
use crate::write::chart_text::{
    ColourMode, TextChartOpts, chart_as_styled_lines, styled_lines_as_text,
};
use crate::write::csv::{print_csv, print_tsv};
use crate::write::fods::locale_encoding_name;
use crate::write::html::escape;
use crate::write::json::to_json_text;

/// The chart kinds --chart accepts (chartKinds): auto, or the bar layouts
/// of reports with several groups of accounts.
const CHART_KINDS: [&str; 3] = ["auto", "diverging", "grouped"];

/// --chart's value: comma-separated parts, an optional chart kind (default
/// auto) followed by optional non-zero scale amounts, read with these
/// commodity styles; a comma between digits is an error, since commas
/// separate the parts (parseChartArg).
pub fn parse_chart_arg(
    styles: &BTreeMap<Sym, AmountStyle>,
    s: &str,
) -> Result<(String, Vec<Amount>), String> {
    let cs: Vec<char> = s.chars().collect();
    if cs
        .windows(3)
        .any(|w| w[0].is_ascii_digit() && w[1] == ',' && w[2].is_ascii_digit())
    {
        return Err(format!(
            "--chart's SCALE amounts should be written without a comma (commas separate them; use . as decimal mark, eg 2.5 BTC, and leave out digit group marks or use spaces, eg 10000 EUR or 10 000 EUR), but this was not: {s}"
        ));
    }
    let commodities: BTreeMap<Sym, Commodity> = (styles.iter())
        .map(|(c, st)| {
            (
                c.clone(),
                Commodity {
                    symbol: c.clone(),
                    format: Some(st.clone()),
                    comment: String::new(),
                    tags: Vec::new(),
                    aliases: Vec::new(),
                    pos: SourcePos::null(),
                },
            )
        })
        .collect();
    let ctx = AmountCtx {
        commodities: Some(&commodities),
        ..AmountCtx::default()
    };
    let scale = |p: &str| -> Result<Amount, String> {
        if CHART_KINDS.contains(&p) {
            return Err(format!(
                "--chart's KIND ({p}) should come before any SCALE amounts"
            ));
        }
        match parse_amount_with(&ctx, p) {
            Ok(a) if !a.quantity.is_zero() => Ok(a),
            _ => Err(format!(
                "--chart's value should be a chart KIND ({}), and/or non-zero SCALE amounts after it, separated by commas (and written without digit group marks), but this part was not: {p}",
                CHART_KINDS.join(", ")
            )),
        }
    };
    let parts: Vec<&str> = s
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    match parts.split_first() {
        Some((k, rest)) if CHART_KINDS.contains(k) => Ok((
            k.to_string(),
            rest.iter().map(|p| scale(p)).collect::<Result<_, _>>()?,
        )),
        _ => Ok((
            "auto".to_string(),
            parts.iter().map(|p| scale(p)).collect::<Result<_, _>>()?,
        )),
    }
}

/// The check of --chart's value on the command line, with no commodity
/// styles (chartFlagWith's setchart).
pub fn check_chart_arg(s: &str) -> Result<(), String> {
    parse_chart_arg(&BTreeMap::new(), s).map(|_| ())
}

/// The check of a chart's --width (chartWidthFlag).
pub fn check_chart_width(s: &str) -> Result<(), String> {
    match s.parse::<i64>() {
        Ok(n) if n > 0 => Ok(()),
        _ => Err("--width should be a positive whole number".to_string()),
    }
}

/// Whether a chart was asked for, with --chart or an output format only
/// charts have (svg) (chartRequested).
pub fn chart_requested(opts: &CliOpts) -> bool {
    opts.raw.string("chart").is_some() || opts.output_format() == "svg"
}

/// The error for an output format a chart can't be shown in, if the
/// options select one: txt, svg, csv, tsv and json can show charts, and
/// html when the report has html (writeChart's checks, made before the
/// chart).
pub fn check_chart_format(opts: &CliOpts) -> Result<(), String> {
    const FORMATS: [&str; 10] = [
        "txt",
        "beancount",
        "csv",
        "json",
        "html",
        "ledger",
        "sql",
        "tsv",
        "fods",
        "svg",
    ];
    match opts.output_format().as_str() {
        "txt" | "svg" | "csv" | "tsv" | "json" | "html" => Ok(()),
        f if FORMATS.contains(&f) => Err(format!(
            "--chart does not yet support {f} output; try txt, svg, html, csv, tsv or json"
        )),
        f => Err(unsupported_output_format_error(f)),
    }
}

/// A chart in the output format the options select, with --chart's kind
/// and scales applied, the scale amounts read with the chart's commodity
/// styles (writeChart): as text, svg, the chart's data (csv, tsv, json),
/// or with html, the chart above the report's html, collapsed.
pub fn render_chart(
    opts: &CliOpts,
    mut chart: Chart,
    report_html: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let styles: BTreeMap<Sym, AmountStyle> = (chart.panels.iter())
        .map(|p| (p.unit.commodity.clone(), (*p.unit.style).clone()))
        .collect();
    let (kind, scales) = (opts.raw.string("chart"))
        .and_then(|s| parse_chart_arg(&styles, s).ok())
        .unwrap_or(("auto".to_string(), Vec::new()));
    let layout = match kind.as_str() {
        "grouped" => Some(BarLayout::Grouped),
        "diverging" => Some(BarLayout::Diverging),
        _ => None,
    };
    set_chart_scales(&scales, &mut chart);
    set_chart_layout(layout, &mut chart);
    Ok(match opts.output_format().as_str() {
        "txt" => chart_as_text_for_opts(opts, &chart),
        "svg" => chart_as_svg(true, &chart),
        "csv" => print_csv(&chart_as_csv(&chart)),
        "tsv" => print_tsv(&chart_as_csv(&chart)),
        "json" => to_json_text(&chart) + "\n",
        // without the dark variant, since the html is always light
        _ => chart_as_svg(false, &chart) + &collapsed_html("Report details", &report_html()?),
    })
}

/// A chart as text, as the options say (chartAsTextForOpts): --width
/// characters wide or the available width, coloured if colour is on
/// (exact colours if the terminal advertises 24-bit colour; their light
/// variants, since hledgers does not query the terminal's background),
/// with unicode block characters if the locale's encoding is UTF-8.
/// Warns when the chart has more than 8 panels.
fn chart_as_text_for_opts(opts: &CliOpts, chart: &Chart) -> String {
    let unicode = locale_encoding_name() == "UTF-8";
    let npanels = chart.panels.len();
    if npanels > 8 {
        crate::console::warn(
            &opts.input.prog_args,
            &format!(
                "this chart has {npanels} panels, one per commodity. To show fewer, convert to one commodity (eg -X USD)\nor select some (eg cur:USD)."
            ),
        );
    }
    let truecolour = std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
    let mode = if !opts.spec.opts.color {
        ColourMode::NoColour
    } else if truecolour {
        ColourMode::RgbColour(false)
    } else {
        ColourMode::BasicColour
    };
    let width = (opts.width.as_deref())
        .and_then(|w| w.parse().ok())
        .unwrap_or(opts.available_width);
    styled_lines_as_text(
        mode,
        &chart_as_styled_lines(TextChartOpts { width, unicode }, chart),
    )
}

/// Some html in a collapsed section with this label, its report title
/// hidden since the chart above shows it (collapsedHtml).
fn collapsed_html(label: &str, html: &str) -> String {
    format!(
        "<style>details.hledger-details{{font:14px/1.5 system-ui,-apple-system,\"Segoe UI\",sans-serif;font-variant-numeric:tabular-nums}}details.hledger-details>summary{{width:fit-content;margin:8px 0;padding:2px 10px;border:1px solid #e1e0d9;border-radius:6px;color:#52514e;cursor:pointer}}details.hledger-details>summary:hover{{background:#f3f2ee;color:#0b0b0b}}details.hledger-details .report-title{{display:none}}</style>\n<details class=\"hledger-details\"><summary>{}</summary>\n{html}</details>\n",
        escape(label)
    )
}

/// The most series in each section of a compound report chart
/// (maxSectionSeries).
const MAX_SECTION_SERIES: usize = MAX_TINTS + 1;

/// The most series in a panel; beyond, the smallest are combined into one
/// "other" series (maxSeries).
const MAX_SERIES: usize = 8;

/// A multi-period balance report's chart (multiBalanceReportAsChart):
/// period changes as stacked bars with the net total as a marker, end
/// balances as lines with the total as a line; a panel per commodity,
/// most used first; in tree mode each account's own amounts; no total
/// with -N, --percent, or a single series.
pub fn multi_balance_report_as_chart(
    account_type: &dyn Fn(&str) -> Option<AccountType>,
    ropts: &ReportOpts,
    title: &str,
    report: &MultiBalanceReport,
) -> Chart {
    let flow = ropts.balance_accum == BalanceAccumulation::PerPeriod;
    let mark = if flow { Mark::Bar } else { Mark::Line };
    let tree = ropts.account_list_mode == crate::reports::opts::AccountListMode::Tree;
    let rows = if tree {
        own_amount_rows(&report.rows)
    } else {
        report.rows.clone()
    };
    let cells: Vec<&MixedAmount> = report.rows.iter().flat_map(|r| &r.amounts).collect();
    let allamts: Vec<&Amount> = cells.iter().flat_map(|m| m.amounts_raw()).collect();
    let panels = (commodities_by_use(&cells).into_iter())
        .filter_map(|c| {
            let unit = unit_amount(&c, &allamts);
            let series = limit_series(
                MAX_SERIES,
                mark,
                (rows.iter())
                    .filter_map(|r| {
                        let qs: Vec<Decimal> =
                            r.amounts.iter().map(|m| quantity_in(&c, m)).collect();
                        qs.iter()
                            .any(|q| !q.is_zero())
                            .then(|| (account_label(account_type, &r.name), qs))
                    })
                    .collect(),
            );
            if series.is_empty() {
                return None;
            }
            let mut all = series;
            if all.len() > 1 && !ropts.no_total && !ropts.percent {
                all.push(Series {
                    label: ChartLabel::plain("Total"),
                    role: SeriesRole::Total,
                    mark: if flow { Mark::Marker } else { Mark::Line },
                    values: (report.totals.amounts.iter())
                        .map(|m| Some(quantity_in(&c, m)))
                        .collect(),
                    group: 0,
                    sign: 0,
                });
            }
            Some(Panel {
                summary: row_summaries(ropts, &unit, &report.totals),
                unit,
                series: all,
                group_names: Vec::new(),
                scale: None,
                layout: BarLayout::Diverging,
                counts: is_count(ropts),
                value_labels: Vec::new(),
            })
        })
        .collect();
    Chart {
        title: title.to_string(),
        x_axis: XAxis::Periods(
            (report.dates.iter())
                .map(|s| {
                    (
                        *s,
                        crate::cli::commands::balance::report_period_name(ropts, &report.dates, s),
                    )
                })
                .collect(),
        ),
        panels,
    }
}

/// A single-period balance report's chart, a bar per account (horizontal,
/// since accounts are categories), a panel per commodity, most used
/// first, accounts without the commodity having no value there; all rows
/// shown, indented in tree mode (balanceReportAsChart).
pub fn balance_report_as_chart(
    account_type: &dyn Fn(&str) -> Option<AccountType>,
    ropts: &ReportOpts,
    title: &str,
    report: &BalanceReport,
) -> Chart {
    let cells: Vec<&MixedAmount> = report.items.iter().map(|(_, m)| m).collect();
    let allamts: Vec<&Amount> = cells.iter().flat_map(|m| m.amounts_raw()).collect();
    let panels = (commodities_by_use(&cells).into_iter())
        .map(|c| Panel {
            unit: unit_amount(&c, &allamts),
            series: vec![Series {
                label: ChartLabel::plain("Balance"),
                role: SeriesRole::Item,
                mark: Mark::Bar,
                values: (report.items.iter())
                    .map(|(_, m)| {
                        (m.amounts_raw().iter())
                            .any(|a| a.commodity == c)
                            .then(|| quantity_in(&c, m))
                    })
                    .collect(),
                group: 0,
                sign: 0,
            }],
            group_names: Vec::new(),
            scale: None,
            layout: BarLayout::Diverging,
            counts: is_count(ropts),
            summary: Vec::new(),
            value_labels: Vec::new(),
        })
        .collect();
    Chart {
        title: title.to_string(),
        x_axis: XAxis::Categories(
            (report.items.iter())
                .map(|(n, _)| ChartLabel {
                    full: n.full.to_string(),
                    short: n.name.to_string(),
                    indent: n.indent,
                    typ: account_type(&n.full),
                    account: true,
                })
                .collect(),
        ),
        panels,
    }
}

/// A compound balance report's chart: its sections' accounts and the net
/// total (compoundBalanceReportAsChart). Amounts are their contribution to
/// the net, negative for sections that subtract from it; each section's
/// accounts stacked in a bar of their own, side by side (the grouped
/// layout, for changes), or all in one diverging bar (for end balances);
/// each series with its section as group and the section's usual sign, so
/// that contra amounts show; the net as a marker, or a line for end
/// balances; a panel per commodity; at most 5 series per section; in tree
/// mode each account's own amounts; no net with -N or --percent. Accounts
/// without a type of their own get their section's; "..." is labelled
/// with its section's title.
pub fn compound_balance_report_as_chart(
    account_type: &dyn Fn(&str) -> Option<AccountType>,
    section_types: &[Option<AccountType>],
    ropts: &ReportOpts,
    cbr: &CompoundPeriodicReport<DisplayName, MixedAmount>,
) -> Chart {
    let flow = ropts.balance_accum == BalanceAccumulation::PerPeriod;
    let tree = ropts.account_list_mode == crate::reports::opts::AccountListMode::Tree;
    let sections: Vec<Section> = (cbr.subreports.iter().enumerate())
        .map(|(i, (t, r, increases))| {
            let sectype = section_types.get(i).copied().flatten();
            let rows = if tree {
                own_amount_rows(&r.rows)
            } else {
                r.rows.clone()
            };
            ((t.clone(), sectype), if *increases { 1 } else { -1 }, rows)
        })
        .collect();
    let section_label = |(t, sectype): &(String, Option<AccountType>),
                         r: &MultiBalanceReportRow| {
        let l = account_label(account_type, &r.name);
        if l.full == "..." {
            ChartLabel {
                full: t.clone(),
                short: t.clone(),
                indent: 0,
                typ: *sectype,
                account: true,
            }
        } else {
            ChartLabel {
                typ: l.typ.or(*sectype),
                ..l
            }
        }
    };
    // a section's usual sign; cash (cf's section) has none, flowing both ways
    let usual_sign = |(_, sectype): &(String, Option<AccountType>), sign: i32| {
        if *sectype == Some(AccountType::Cash) {
            0
        } else {
            sign
        }
    };
    let nettotals = &cbr.totals.amounts;
    let showtotal = !ropts.no_total && !ropts.percent;
    let netlabel = "Net:".trim_end_matches([':', '\u{ff1a}']);
    let cells: Vec<&MixedAmount> = (sections.iter())
        .flat_map(|(_, _, rows)| rows.iter().flat_map(|r| &r.amounts))
        .chain(nettotals)
        .collect();
    let allamts: Vec<&Amount> = cells.iter().flat_map(|m| m.amounts_raw()).collect();
    let panels = (commodities_by_use(&cells).into_iter())
        .filter_map(|c| {
            let unit = unit_amount(&c, &allamts);
            let mut series: Vec<Series> = Vec::new();
            for (k, (section, sign, rows)) in sections.iter().enumerate() {
                let accts: Vec<(ChartLabel, Vec<Decimal>)> = (rows.iter())
                    .filter_map(|r| {
                        let s = Decimal::from_i64(i64::from(*sign));
                        let qs: Vec<Decimal> =
                            r.amounts.iter().map(|m| &quantity_in(&c, m) * &s).collect();
                        qs.iter()
                            .any(|q| !q.is_zero())
                            .then(|| (section_label(section, r), qs))
                    })
                    .collect();
                for s in limit_series(MAX_SECTION_SERIES, Mark::Bar, accts) {
                    series.push(Series {
                        group: k,
                        sign: usual_sign(section, *sign),
                        ..s
                    });
                }
            }
            if series.is_empty() {
                return None;
            }
            if showtotal {
                series.push(Series {
                    label: ChartLabel::plain(netlabel),
                    role: SeriesRole::Total,
                    mark: if flow { Mark::Marker } else { Mark::Line },
                    values: nettotals.iter().map(|m| Some(quantity_in(&c, m))).collect(),
                    group: 0,
                    sign: 0,
                });
            }
            Some(Panel {
                summary: row_summaries(ropts, &unit, &cbr.totals),
                unit,
                series,
                group_names: cbr.subreports.iter().map(|(t, _, _)| t.clone()).collect(),
                scale: None,
                layout: if flow {
                    BarLayout::Grouped
                } else {
                    BarLayout::Diverging
                },
                counts: is_count(ropts),
                value_labels: Vec::new(),
            })
        })
        .collect();
    let spans = &cbr.dates;
    Chart {
        title: cbr.title.clone(),
        // the title names a single period
        x_axis: XAxis::Periods(
            (spans.iter())
                .map(|s| {
                    let name = if spans.len() == 1 {
                        String::new()
                    } else {
                        crate::cli::commands::balance::report_period_name(ropts, spans, s)
                    };
                    (*s, name)
                })
                .collect(),
        ),
        panels,
    }
}

/// A compound report's section for charting: its title and account type,
/// its sign in the net total, and its rows.
type Section = (
    (String, Option<AccountType>),
    i32,
    Vec<MultiBalanceReportRow>,
);

/// Whether a report's values are counts (balanceCalcIsCount).
fn is_count(ropts: &ReportOpts) -> bool {
    use crate::reports::opts::BalanceCalculation::*;
    matches!(ropts.balance_calc, PostingsCount | TransactionsCount)
}

/// At most this many series: if there are more, the largest (by total
/// absolute value) in their order, and the rest combined in a final
/// "other" series per account type (limitSeries).
fn limit_series(maxn: usize, mark: Mark, items: Vec<(ChartLabel, Vec<Decimal>)>) -> Vec<Series> {
    let sizes: Vec<(Option<AccountType>, Decimal)> = (items.iter())
        .map(|(l, qs)| {
            let size = qs.iter().fold(Decimal::ZERO, |a, q| &a + &q.abs());
            (l.typ.map(main_account_type), size)
        })
        .collect();
    let (kept, othergroups) = choose_series(maxn, &sizes);
    let mut series: Vec<Series> = (kept.iter())
        .map(|i| {
            let (l, qs) = &items[*i];
            Series {
                label: l.clone(),
                role: SeriesRole::Item,
                mark,
                values: qs.iter().cloned().map(Some).collect(),
                group: 0,
                sign: 0,
            }
        })
        .collect();
    let nothers = othergroups.len();
    for grp in othergroups {
        let members: Vec<&(ChartLabel, Vec<Decimal>)> = grp.iter().map(|i| &items[*i]).collect();
        let names: Vec<&str> = members.iter().map(|(l, _)| l.full.as_str()).collect();
        let n = members.first().map_or(0, |(_, qs)| qs.len());
        let values = (0..n)
            .map(|j| {
                Some(
                    (members.iter())
                        .filter_map(|(_, qs)| qs.get(j))
                        .fold(Decimal::ZERO, |a, q| &a + q),
                )
            })
            .collect();
        series.push(Series {
            label: ChartLabel {
                typ: members.first().and_then(|(l, _)| l.typ),
                ..other_label(nothers, &names)
            },
            role: SeriesRole::Other,
            mark,
            values,
            group: 0,
            sign: 0,
        });
    }
    series
}

/// Which items (their account type and size) to show as series of their
/// own, when at most n can be: all if they fit; otherwise the largest, at
/// least the largest of each type, the rest of each type combined into an
/// "other" series (a type with one left over shows it instead)
/// (chooseSeries). The positions to show, and of each type's to combine.
fn choose_series(
    n: usize,
    items: &[(Option<AccountType>, Decimal)],
) -> (Vec<usize>, Vec<Vec<usize>>) {
    let positions: Vec<usize> = (0..items.len()).collect();
    if items.len() <= n {
        return (positions, Vec::new());
    }
    let type_at = |i: usize| items[i].0;
    let mut types: Vec<Option<AccountType>> = Vec::new();
    for i in &positions {
        if !types.contains(&type_at(*i)) {
            types.push(type_at(*i));
        }
    }
    let mut ranked = positions.clone();
    ranked.sort_by(|a, b| items[*b].1.cmp(&items[*a].1));
    let leftovers = |ks: &BTreeSet<usize>| -> Vec<Vec<usize>> {
        (types.iter())
            .map(|t| {
                (positions.iter())
                    .filter(|i| type_at(**i) == *t && !ks.contains(i))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let fits = |ks: &BTreeSet<usize>| {
        ks.len() + leftovers(ks).iter().filter(|l| !l.is_empty()).count() <= n
    };
    let mut kept: BTreeSet<usize> = (types.iter())
        .filter_map(|t| ranked.iter().find(|i| type_at(**i) == *t).copied())
        .collect();
    for i in &ranked {
        let mut ks = kept.clone();
        ks.insert(*i);
        if fits(&ks) {
            kept = ks;
        }
    }
    let (singles, multiples): (Vec<Vec<usize>>, Vec<Vec<usize>>) = (leftovers(&kept).into_iter())
        .filter(|l| !l.is_empty())
        .partition(|l| l.len() == 1);
    let mut shown: Vec<usize> = kept
        .into_iter()
        .chain(singles.into_iter().flatten())
        .collect();
    shown.sort_unstable();
    (shown, multiples)
}

/// The label of an "other" series combining these accounts, naming their
/// common parent when there are several "other" series (otherLabel).
fn other_label(nothers: usize, accts: &[&str]) -> ChartLabel {
    let n = accts.len();
    let parent = match accts.split_first() {
        None => String::new(),
        Some((first, rest)) => {
            let mut common: Vec<&str> = first.split(':').collect();
            for a in rest {
                let parts: Vec<&str> = a.split(':').collect();
                let k = common
                    .iter()
                    .zip(&parts)
                    .take_while(|(x, y)| x == y)
                    .count();
                common.truncate(k);
            }
            common.join(":")
        }
    };
    let accounts = if n == 1 { "account" } else { "accounts" };
    if nothers > 1 && !parent.is_empty() {
        ChartLabel::plain(&format!("other {parent} ({n} {accounts})"))
    } else {
        ChartLabel::plain(&format!("other ({n} {accounts})"))
    }
}

/// In tree mode, a parent's row includes its displayed subaccounts' rows:
/// subtract those, leaving each row its own amounts (ownAmountRows).
fn own_amount_rows(rows: &[MultiBalanceReportRow]) -> Vec<MultiBalanceReportRow> {
    let names: BTreeSet<&str> = rows.iter().map(|r| &*r.name.full).collect();
    let mut children: BTreeMap<&str, Vec<&MultiBalanceReportRow>> = BTreeMap::new();
    for r in rows {
        if let Some(p) = parent_account_names(&r.name.full).find(|p| names.contains(p)) {
            children.entry(p).or_default().push(r);
        }
    }
    (rows.iter())
        .map(|r| match children.get(&*r.name.full) {
            None => r.clone(),
            Some(cs) => {
                let mut own = r.clone();
                for c in cs {
                    for (a, b) in own.amounts.iter_mut().zip(&c.amounts) {
                        *a = &*a - b;
                    }
                }
                own
            }
        })
        .collect()
}

/// The commodities with non-zero quantities in these cells, those in the
/// most cells first, then alphabetically (commoditiesByUse).
fn commodities_by_use(cells: &[&MixedAmount]) -> Vec<Sym> {
    let mut counts: BTreeMap<Sym, usize> = BTreeMap::new();
    for m in cells {
        for c in m.commodities() {
            if !quantity_in(&c, m).is_zero() {
                *counts.entry(c).or_default() += 1;
            }
        }
    }
    let mut cs: Vec<(Sym, usize)> = counts.into_iter().collect();
    cs.sort_by(|(c1, n1), (c2, n2)| n2.cmp(n1).then_with(|| c1.cmp(c2)));
    cs.into_iter().map(|(c, _)| c).collect()
}

/// A unit for values in this commodity: an amount of it from the list
/// (for its style), with zero quantity and no cost (unitAmount).
fn unit_amount(c: &Sym, amounts: &[&Amount]) -> Amount {
    match amounts.iter().find(|a| &a.commodity == c) {
        Some(a) => Amount {
            quantity: Decimal::ZERO,
            cost: None,
            cost_basis: None,
            ..(*a).clone()
        },
        None => Amount {
            commodity: c.clone(),
            ..Amount::null()
        },
    }
}

/// With -T or -A in a chart of changes per period, the total and the
/// average per period of the report's totals row in the unit's commodity,
/// labelled like the report's columns, each in its own style
/// (rowSummaries).
fn row_summaries<N>(
    ropts: &ReportOpts,
    unit: &Amount,
    totals: &PeriodicReportRow<N, MixedAmount>,
) -> Vec<Summary> {
    if ropts.balance_accum != BalanceAccumulation::PerPeriod || ropts.percent {
        return Vec::new();
    }
    let c = &unit.commodity;
    let amount_in = |m: &MixedAmount| match m.amounts_raw().iter().find(|a| &a.commodity == c) {
        Some(a) => Amount {
            quantity: quantity_in(c, m),
            style: a.style.clone(),
            ..unit.clone()
        },
        None => unit.clone(),
    };
    let mut summary = Vec::new();
    if ropts.row_total {
        summary.push(Summary {
            kind: SummaryKind::Total,
            label: "Total".to_string(),
            value: amount_in(&totals.total),
        });
    }
    if ropts.average {
        summary.push(Summary {
            kind: SummaryKind::Average,
            label: "Average".to_string(),
            value: amount_in(&totals.average),
        });
    }
    summary
}

/// The total quantity of this commodity in a mixed amount, ignoring costs
/// (quantityIn).
fn quantity_in(c: &Sym, m: &MixedAmount) -> Decimal {
    (m.amounts_raw().iter())
        .filter(|a| &a.commodity == c)
        .fold(Decimal::ZERO, |s, a| &s + &a.quantity)
}

/// A label for an account (or accounts), from a report row's name; a
/// "..." row, all accounts, is labelled as not an account (accountLabel).
fn account_label(
    account_type: &dyn Fn(&str) -> Option<AccountType>,
    n: &DisplayName,
) -> ChartLabel {
    ChartLabel {
        full: n.full.to_string(),
        short: n.name.to_string(),
        indent: n.indent,
        typ: account_type(&n.full),
        account: &*n.full != "...",
    }
}
