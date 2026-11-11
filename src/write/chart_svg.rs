//! Drawing a chart as SVG (Hledger.Write.Chart.Svg): charts over periods
//! or dates with time along the x axis, bar series as vertical stacked
//! bars, lines as 2px lines, totals as ink marks across each period's bars
//! or as an ink line; charts of categories as vertical bars, or horizontal
//! ones when there are many. A panel per commodity, a legend when a panel
//! has several series, a tooltip (an SVG title) on each mark, its own
//! styles with a dark variant. Text widths are estimated, coordinates
//! rounded to one decimal place.

use std::collections::BTreeMap;

use crate::account::AccountType;
use crate::amount::render::{AmountFormat, show_amount_b};
use crate::amount::{Amount, AmountStyle, Precision, Sym};
use crate::dates::Day;
use crate::decimal::Decimal;
use crate::text::display_width;
use crate::write::chart::{
    BarLayout, Chart, ChartLabel, Colour, Colouring, MAX_TINTS, Mark, Panel, Series, SeriesRole,
    SummaryKind, XAxis, chart_colouring, colour_hex, is_contra, is_hatched, main_account_type,
    round, scale_spans_f64, series_colours, series_groups, show_value, total_is_bar_end,
};
use crate::write::html::escape;

/// A chart as a complete SVG document, one element per line, with or
/// without the dark variant of its styles (chartAsSvgWith; chartAsSvg is
/// with it).
pub fn chart_as_svg(dark: bool, chart: &Chart) -> String {
    let mut out = Out::default();
    chart_svg(&mut out, dark, chart);
    out.0.replace("><", ">\n<") + "\n"
}

/// The SVG markup being written: elements as blaze renders them.
#[derive(Default)]
struct Out(String);

impl Out {
    fn attrs(&mut self, attrs: &[(&str, &str)]) {
        for (k, v) in attrs {
            self.0.push_str(&format!(" {k}=\"{}\"", escape(v)));
        }
    }

    /// An element without content: `<tag a="v" />`.
    fn leaf(&mut self, tag: &str, attrs: &[(&str, &str)]) {
        self.0.push('<');
        self.0.push_str(tag);
        self.attrs(attrs);
        self.0.push_str(" />");
    }

    fn open(&mut self, tag: &str, attrs: &[(&str, &str)]) {
        self.0.push('<');
        self.0.push_str(tag);
        self.attrs(attrs);
        self.0.push('>');
    }

    fn close(&mut self, tag: &str) {
        self.0.push_str(&format!("</{tag}>"));
    }

    /// An element with text content.
    fn text(&mut self, tag: &str, attrs: &[(&str, &str)], text: &str) {
        self.open(tag, attrs);
        self.0.push_str(&escape(text));
        self.close(tag);
    }

    /// Markup with a tooltip: grouped with an SVG title (withTooltip).
    fn tooltip(&mut self, tip: &str, f: impl FnOnce(&mut Out)) {
        self.open("g", &[]);
        self.text("title", &[], tip);
        f(self);
        self.close("g");
    }
}

// Layout constants, in pixels.
const PAD: f64 = 16.0;
const PANEL_GAP: f64 = 16.0;
const LEGEND_ROW_HEIGHT: f64 = 24.0;
/// The height of a row in a horizontal bar chart.
const ROW_HEIGHT: f64 = 28.0;
/// The chart's width (svgWidth).
const WIDTH: f64 = 880.0;
/// The height of each plot over time or of columns (svgPlotHeight).
const PLOT_HEIGHT: f64 = 260.0;
/// The narrowest a chart's periods can be for a total to be marked across
/// their bars (minMarkedPeriodWidth).
const MIN_MARKED_PERIOD_WIDTH: f64 = 32.0;
/// The most categories drawn as vertical bars (maxColumns).
const MAX_COLUMNS: usize = 30;
/// The hatching pattern's id (hatchId).
const HATCH_ID: &str = "hledger-chart-hatch";

/// A coordinate, rounded to one decimal place, without a trailing ".0"
/// (num).
fn num(x: f64) -> String {
    let r = round(x * 10.0) as f64 / 10.0;
    if r == 0.0 {
        return "0".to_string();
    }
    let t = format!("{r:.1}");
    t.strip_suffix(".0").map(str::to_string).unwrap_or(t)
}

/// How many intervals a plot this high divides its y axis into: about
/// one per 50px, from 2 to 5 (plotIntervals).
fn plot_intervals(h: f64) -> usize {
    ((h / 50.0).floor() as usize).clamp(2, 5)
}

/// The estimated width of some text in the 14px sans font (textWidth).
fn text_width(t: &str) -> f64 {
    t.chars()
        .map(|c| {
            let w = display_width(c.encode_utf8(&mut [0; 4]));
            if w == 2 {
                14.0
            } else if w == 0 {
                0.0
            } else if c.is_ascii_digit() {
                8.7
            } else if "mwMW".contains(c) {
                12.0
            } else if c.is_uppercase() {
                9.5
            } else if "fijlrt".contains(c) {
                4.2
            } else if " .,:;'|!()".contains(c) {
                3.8
            } else {
                7.9
            }
        })
        .sum()
}

fn to_d(q: &Decimal) -> f64 {
    q.to_f64()
}

/// A series to draw, with its css class (DrawSeries).
#[derive(Clone)]
struct DrawSeries<'a> {
    class: String,
    series: &'a Series,
}

/// The CSS class of a colour (colourClass).
fn colour_class(c: Colour) -> String {
    match c {
        Colour::Type(at, k) => format!("t{}{}", type_code(main_account_type(at)), k % MAX_TINTS),
        Colour::Index(k) => format!("s{}", k % 8),
        Colour::Lone => "lone".to_string(),
        Colour::Neutral => "neutral".to_string(),
        Colour::Other(None) => "other".to_string(),
        Colour::Other(Some(at)) => format!("o{}", type_code(main_account_type(at))),
        Colour::Total => "total".to_string(),
    }
}

/// (typeCode)
fn type_code(at: AccountType) -> &'static str {
    match at {
        AccountType::Asset => "a",
        AccountType::Liability => "l",
        AccountType::Equity => "e",
        AccountType::Revenue => "r",
        AccountType::Expense => "x",
        _ => "e",
    }
}

/// (chartSvgMarkupColoured)
fn chart_svg(out: &mut Out, dark: bool, chart: &Chart) {
    let colouring = chart_colouring(chart);
    let title = &chart.title;
    let titleheight = if title.is_empty() { 0.0 } else { 38.0 };
    let show_headings = chart.panels.len() > 1;
    let mut top = PAD + titleheight;
    let mut panels = Vec::new();
    for p in &chart.panels {
        let mut m = Out::default();
        let h = panel_svg(&mut m, &chart.x_axis, colouring, show_headings, top, p);
        panels.push(m.0);
        top += h + PANEL_GAP;
    }
    let totalheight = if chart.panels.is_empty() {
        top + 14.0 + PAD
    } else {
        top
    };
    let size = format!("0 0 {} {}", num(WIDTH), num(totalheight));
    let mut attrs: Vec<(&str, &str)> = Vec::new();
    let (w, h) = (num(WIDTH), num(totalheight));
    attrs.extend([
        ("xmlns", "http://www.w3.org/2000/svg"),
        ("class", "hledger-chart"),
        ("width", w.as_str()),
        ("height", h.as_str()),
        ("viewBox", size.as_str()),
        ("role", "img"),
    ]);
    if !title.is_empty() {
        attrs.push(("aria-label", title.as_str()));
    }
    out.open("svg", &attrs);
    if !title.is_empty() {
        out.text("title", &[], title);
    }
    out.open("style", &[]);
    out.0.push_str(&stylesheet(dark));
    out.close("style");
    out.leaf(
        "rect",
        &[("class", "bg"), ("width", "100%"), ("height", "100%")],
    );
    if !title.is_empty() {
        out.text(
            "text",
            &[
                ("class", "title"),
                ("x", &num(PAD)),
                ("y", &num(PAD + 17.0)),
            ],
            title,
        );
    }
    for m in panels {
        out.0.push_str(&m);
    }
    if chart.panels.is_empty() {
        out.text(
            "text",
            &[
                ("class", "label"),
                ("x", &num(PAD)),
                ("y", &num(PAD + titleheight + 16.0)),
            ],
            "No data",
        );
    }
    out.close("svg");
}

/// Draw a panel with its top at this y; its height (panelSvg).
fn panel_svg(
    out: &mut Out,
    xaxis: &XAxis,
    colouring: Colouring,
    show_heading: bool,
    top: f64,
    p: &Panel,
) -> f64 {
    let unit = &p.unit;
    let headingh = if show_heading && !unit.commodity.is_empty() {
        28.0
    } else {
        0.0
    };
    let summaryh = if p.summary.is_empty() { 0.0 } else { 26.0 };
    let show_summary = |v: &Amount| show_amount_b(&AmountFormat::ONE_LINE_NO_COST, v).s;
    let summarytext = (p.summary.iter())
        .map(|su| format!("{} {}", su.label, show_summary(&su.value)))
        .collect::<Vec<_>>()
        .join(", ");
    let maverage = match xaxis {
        XAxis::Periods(_) => p.summary.iter().find(|s| s.kind == SummaryKind::Average),
        _ => None,
    };
    let items: Vec<&Series> = (p.series.iter())
        .filter(|s| s.role != SeriesRole::Total && s.mark != Mark::Marker)
        .collect();
    let mtotal = p.series.iter().find(|s| s.role == SeriesRole::Total);
    let hasbars = items.iter().any(|s| s.mark == Mark::Bar);
    let xcount = match xaxis {
        XAxis::Periods(ps) => Some(ps.len()),
        XAxis::Dates(ds) => Some(ds.len()),
        XAxis::Categories(_) => None,
    };
    let crowded = xcount.is_some_and(|k| WIDTH / (k.max(1) as f64) < MIN_MARKED_PERIOD_WIDTH);
    let onestack = !(p.layout == BarLayout::Grouped && p.group_names.len() > 1);
    let marked: Option<Series> = match mtotal {
        Some(t)
            if onestack
                && items.iter().all(|s| s.mark == Mark::Bar)
                && total_is_bar_end(&items, t) =>
        {
            None
        }
        Some(t) if crowded && t.mark != Mark::Line && hasbars => None,
        Some(t) if xcount == Some(1) && t.mark == Mark::Line && hasbars => Some(Series {
            mark: Mark::Marker,
            ..t.clone()
        }),
        t => t.cloned(),
    };
    let drawn: Vec<DrawSeries> = (series_colours(colouring, &items).into_iter())
        .zip(&items)
        .map(|(c, s)| DrawSeries {
            class: colour_class(c),
            series: s,
        })
        .collect();
    let listseries = drawn.len() > 1 || drawn.iter().any(|d| d.series.role == SeriesRole::Other);
    let bar_values = || {
        (items.iter())
            .filter(|s| s.mark == Mark::Bar)
            .flat_map(|s| s.values.iter().flatten().map(move |q| (*s, q)))
    };
    let hascontra = bar_values().any(|(s, q)| is_contra(s, q));
    let hashatching = bar_values().any(|(s, q)| is_hatched(s, q));
    let entries: Vec<(String, Mark, String, bool)> = (drawn.iter())
        .map(|d| {
            let s = d.series;
            (
                d.class.clone(),
                s.mark,
                s.label.full.clone(),
                s.role == SeriesRole::Shortfall,
            )
        })
        .collect();
    let legend_total = if items.len() > 1 {
        marked.as_ref()
    } else {
        None
    };
    let mut legend = Out::default();
    let legendh = legend_svg(
        &mut legend,
        top + headingh,
        listseries,
        &entries,
        legend_total,
        hascontra,
        maverage.map(|s| s.label.as_str()),
    );
    let averageline = maverage.map(|su| {
        (
            format!("{} · {}", su.label, show_summary(&su.value)),
            su.value.quantity.clone(),
        )
    });
    let mut body = Out::default();
    let bodytop = top + headingh + legendh;
    let bodyh = match xaxis {
        XAxis::Categories(ls) => categories_svg(
            &mut body,
            bodytop,
            p.scale.as_ref(),
            unit,
            &drawn,
            mtotal,
            marked.as_ref(),
            &p.value_labels,
            ls,
        ),
        XAxis::Periods(ps) => {
            let labels: Vec<String> = ps.iter().map(|(_, l)| l.clone()).collect();
            time_svg(
                &mut body,
                bodytop,
                p.scale.as_ref(),
                p.layout,
                unit,
                &drawn,
                marked.as_ref(),
                averageline,
                &p.value_labels,
                &XScale::Even(labels),
            )
        }
        XAxis::Dates(ds) => time_svg(
            &mut body,
            bodytop,
            p.scale.as_ref(),
            p.layout,
            unit,
            &drawn,
            marked.as_ref(),
            None,
            &p.value_labels,
            &XScale::Dates(ds.clone()),
        ),
    };
    if headingh > 0.0 {
        out.text(
            "text",
            &[
                ("class", "heading"),
                ("x", &num(PAD)),
                ("y", &num(top + 16.0)),
            ],
            &unit.commodity,
        );
    }
    if hashatching {
        out.open("defs", &[]);
        out.open(
            "pattern",
            &[
                ("id", HATCH_ID),
                ("width", "5"),
                ("height", "5"),
                ("patternUnits", "userSpaceOnUse"),
                ("patternTransform", "rotate(45)"),
            ],
        );
        out.leaf(
            "line",
            &[
                ("class", "hatch"),
                ("x1", "0"),
                ("y1", "0"),
                ("x2", "0"),
                ("y2", "5"),
            ],
        );
        out.close("pattern");
        out.close("defs");
    }
    out.0.push_str(&legend.0);
    out.0.push_str(&body.0);
    if summaryh > 0.0 {
        out.text(
            "text",
            &[
                ("class", "label"),
                ("x", &num(PAD)),
                ("y", &num(top + headingh + legendh + bodyh + 16.0)),
            ],
            &summarytext,
        );
    }
    headingh + legendh + bodyh + summaryh
}

/// A legend, flowing left to right: a swatch and label for each series
/// if they are listed, the total's, a dashed swatch for an average line
/// (even when the series are not listed), and a neutral hatched one for
/// contra amounts (legendSvg). Its height.
fn legend_svg(
    out: &mut Out,
    top: f64,
    listseries: bool,
    entries: &[(String, Mark, String, bool)],
    mtotal: Option<&Series>,
    hascontra: bool,
    average_label: Option<&str>,
) -> f64 {
    let average_entry: Vec<(String, Mark, String, bool)> = average_label
        .map(|l| ("average".to_string(), Mark::Line, l.to_string(), false))
        .into_iter()
        .collect();
    let mut all: Vec<(String, Mark, String, bool)> = Vec::new();
    if listseries {
        all.extend(entries.iter().cloned());
        if let Some(t) = mtotal {
            let mark =
                if t.mark == Mark::Line || entries.iter().all(|(_, m, _, _)| *m == Mark::Line) {
                    Mark::Line
                } else {
                    Mark::Marker
                };
            all.push(("total".to_string(), mark, t.label.full.clone(), false));
        }
        all.extend(average_entry);
        if hascontra {
            all.push((
                colour_class(Colour::Neutral),
                Mark::Bar,
                "contra".to_string(),
                true,
            ));
        }
    } else {
        all.extend(average_entry);
    }
    if all.is_empty() {
        return 0.0;
    }
    let item_width = |e: &(String, Mark, String, bool)| 20.0 + text_width(&e.2) + 18.0;
    let mut rows: Vec<Vec<(String, Mark, String, bool)>> = Vec::new();
    for e in all {
        match rows.last_mut() {
            Some(r)
                if r.iter().map(item_width).sum::<f64>() + item_width(&e) <= WIDTH - 2.0 * PAD =>
            {
                r.push(e)
            }
            _ => rows.push(vec![e]),
        }
    }
    for (i, row) in rows.iter().enumerate() {
        let y = top + i as f64 * LEGEND_ROW_HEIGHT + 10.0;
        let mut x = PAD;
        for e in row {
            let (cls, mark, lbl, hatched) = e;
            let line = |out: &mut Out, class: &str, len: f64| {
                out.leaf(
                    "line",
                    &[
                        ("class", class),
                        ("x1", &num(x)),
                        ("x2", &num(x + len)),
                        ("y1", &num(y)),
                        ("y2", &num(y)),
                    ],
                );
            };
            let swatch = |out: &mut Out, first: (&str, &str)| {
                out.leaf(
                    "rect",
                    &[
                        ("x", &num(x)),
                        ("y", &num(y - 6.0)),
                        ("width", "12"),
                        ("height", "12"),
                        ("rx", "3"),
                        first,
                    ],
                );
            };
            match mark {
                Mark::Line if cls == "average" => line(out, "average", 14.0),
                Mark::Line => line(out, &format!("line {cls}"), 12.0),
                Mark::Marker => line(out, &format!("mark {cls}"), 12.0),
                Mark::Bar => {
                    swatch(out, ("class", &format!("fill {cls}")));
                    if *hatched {
                        swatch(out, ("fill", &format!("url(#{HATCH_ID})")));
                    }
                }
            }
            out.text(
                "text",
                &[
                    ("class", "label"),
                    ("x", &num(x + 20.0)),
                    ("y", &num(y + 5.0)),
                ],
                lbl,
            );
            x += item_width(e);
        }
    }
    rows.len() as f64 * LEGEND_ROW_HEIGHT + 8.0
}

/// How x positions are placed along a time axis: evenly, with these
/// labels, or by date (XScale).
enum XScale {
    Even(Vec<String>),
    Dates(Vec<Day>),
}

/// Draw series over time (timeSvg): vertical stacked bars (a stack per
/// group with the grouped layout, side by side), lines, total marks or
/// lines, and the average line. Its height.
#[allow(clippy::too_many_arguments)]
fn time_svg(
    out: &mut Out,
    top: f64,
    mscale: Option<&Decimal>,
    layout: BarLayout,
    unit: &Amount,
    drawn: &[DrawSeries],
    mtotal: Option<&Series>,
    maverage: Option<(String, Decimal)>,
    vlabels: &[Option<String>],
    xscale: &XScale,
) -> f64 {
    let xlabels: Vec<String> = match xscale {
        XScale::Even(ls) => ls.clone(),
        XScale::Dates(ds) => ds.iter().map(Day::to_string).collect(),
    };
    let n = xlabels.len();
    let dates: Option<&Vec<Day>> = match xscale {
        XScale::Dates(ds) => Some(ds),
        XScale::Even(_) => None,
    };
    let (d0, d1) = match dates {
        Some(ds) if !ds.is_empty() => (
            *ds.iter().min().expect("dates"),
            *ds.iter().max().expect("dates"),
        ),
        _ => {
            let d = Day::from_ymd(2000, 1, 1).expect("a date");
            (d, d)
        }
    };
    let bars: Vec<&DrawSeries> = drawn
        .iter()
        .filter(|d| d.series.mark == Mark::Bar)
        .collect();
    let lines: Vec<&DrawSeries> = drawn
        .iter()
        .filter(|d| d.series.mark == Mark::Line)
        .collect();
    let stacks: Vec<Vec<&DrawSeries>> = match layout {
        BarLayout::Grouped => {
            let groups = series_groups(bars.iter().map(|d| d.series));
            (groups.iter())
                .map(|k| {
                    bars.iter()
                        .filter(|d| d.series.group == *k)
                        .copied()
                        .collect()
                })
                .collect()
        }
        BarLayout::Diverging => vec![bars.clone()],
    };
    let stacked_sums = |sign: i32| -> Vec<f64> {
        let mut sums = Vec::new();
        for j in 0..n {
            for stk in &stacks {
                sums.push(
                    (stk.iter())
                        .filter_map(|d| d.series.value_at(j))
                        .filter(|q| signum(q) == sign)
                        .map(to_d)
                        .sum(),
                );
            }
        }
        sums
    };
    let hasbars =
        |j: usize| (bars.iter()).any(|d| d.series.value_at(j).is_some_and(|q| !q.is_zero()));
    let pointvalues: Vec<f64> = (lines.iter().map(|d| d.series).chain(mtotal))
        .flat_map(|s| s.values.iter().flatten().map(to_d))
        .collect();
    let datamax = stacked_sums(1)
        .into_iter()
        .chain(pointvalues.iter().copied())
        .fold(0.0, f64::max);
    let datamin = stacked_sums(-1)
        .into_iter()
        .chain(pointvalues.iter().copied())
        .fold(0.0, f64::min);
    let (lo, hi, ticks, tickdecimals) = panel_scale(
        plot_intervals(PLOT_HEIGHT),
        mscale,
        datamin.min(0.0),
        datamax.max(0.0),
    );
    let ticklabel = |t: f64| tick_label(unit, tickdecimals, &ticks, t);
    let plotleft = PAD
        + ticks
            .iter()
            .map(|t| text_width(&ticklabel(*t)))
            .fold(0.0, f64::max)
        + 12.0;
    let plotright = WIDTH - PAD;
    let labelroom = if vlabels.is_empty() { 0.0 } else { 14.0 };
    let plottop = top + 6.0 + labelroom;
    let plotbottom = plottop + PLOT_HEIGHT;
    let yofline = |v: f64| plottop + PLOT_HEIGHT * (hi - v) / (hi - lo);
    let yof = |v: f64| clamp(plottop, plotbottom, yofline(v));
    let clipid = format!("hledger-chart-clip-{}", num(top));
    let band = (plotright - plotleft) / n.max(1) as f64;
    let span_days = d1.diff_days(d0).max(1) as f64;
    let xofdays = |k: f64| plotleft + 8.0 + (plotright - plotleft - 16.0) * k / span_days;
    let xofday = |d: Day| xofdays(d.diff_days(d0) as f64);
    let xcenter = |j: usize| match dates {
        Some(ds) if j < ds.len() => xofday(ds[j]),
        _ => plotleft + band * (j as f64 + 0.5),
    };
    let nbars = stacks.len().max(1) as f64;
    let barwidth = if band < 4.0 {
        (band * 0.9 / nbars).max(1.0)
    } else {
        ((band * 0.75 - 2.0 * (nbars - 1.0)) / nbars).clamp(1.0, 56.0)
    };
    let groupwidth = nbars * barwidth + 2.0 * (nbars - 1.0);
    let labelevery = ((xlabels.iter().map(|l| text_width(l)).fold(0.0, f64::max) + 12.0) / band)
        .ceil()
        .max(1.0) as usize;
    let tooltip = |lbl: &str, s: &Series, q: &Decimal| {
        [lbl.to_string(), s.label.full.clone(), show_value(unit, q)]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    // gridlines and y tick labels
    gridlines(out, &ticks, (plotleft, plotright), &yof, &ticklabel);
    // x labels: every k'th period label, with the smallest k that leaves
    // them all room; or date ticks at calendar boundaries
    let shown: Vec<(f64, String)> = match xscale {
        XScale::Even(_) => {
            let everykth = |k: usize| -> Vec<(f64, String)> {
                (xlabels.iter().enumerate())
                    .filter(|(j, _)| j % k == 0)
                    .map(|(j, l)| (xcenter(j), l.clone()))
                    .collect()
            };
            (labelevery..=labelevery.max(n))
                .map(everykth)
                .find(|ls| place_x_labels(ls).len() == ls.len())
                .unwrap_or_default()
        }
        XScale::Dates(_) => (date_ticks(d0, d1).into_iter())
            .map(|(d, l)| (xofday(d), l))
            .collect(),
    };
    for (x, lbl) in place_x_labels(&shown) {
        out.text(
            "text",
            &[
                ("class", "tick"),
                ("x", &num(x)),
                ("y", &num(plotbottom + 21.0)),
                ("text-anchor", "middle"),
            ],
            &lbl,
        );
    }
    // bars: the stacks side by side, positive segments upward, negative
    // ones downward, contra amounts nearest the axis
    for (j, lbl) in xlabels.iter().enumerate() {
        for (k, stk) in stacks.iter().enumerate() {
            let x = xcenter(j) - groupwidth / 2.0 + k as f64 * (barwidth + 2.0);
            for sign in [1, -1] {
                let mut segs: Vec<(&DrawSeries, &Decimal)> = (stk.iter())
                    .filter_map(|d| d.series.value_at(j).map(|q| (*d, q)))
                    .filter(|(_, q)| signum(q) == sign)
                    .collect();
                segs.sort_by_key(|(d, q)| !is_contra(d.series, q));
                let segs: Vec<(&str, &Series, &Decimal)> = (segs.into_iter())
                    .map(|(d, q)| (d.class.as_str(), d.series, q))
                    .collect();
                let tip = |s: &Series, q: &Decimal| tooltip(lbl, s, q);
                let sign = sign as f64;
                stack(out, true, (lo, hi), &yof, x, barwidth, &segs, &tip, sign);
            }
        }
    }
    // value labels above each x position's bars, where they fit
    for j in 0..n {
        if let Some(Some(t)) = vlabels.get(j)
            && text_width(t) <= band - 2.0
        {
            let ends = |sign: i32| -> Vec<f64> {
                (stacks.iter())
                    .map(|stk| {
                        (stk.iter())
                            .filter_map(|d| d.series.value_at(j))
                            .filter(|q| signum(q) == sign)
                            .map(to_d)
                            .sum()
                    })
                    .collect()
            };
            let up = ends(1).into_iter().fold(0.0, f64::max);
            let down = ends(-1).into_iter().fold(0.0, f64::min);
            let vy = if up > 0.0 || down == 0.0 {
                yof(up) - 6.0
            } else {
                yof(down) + 14.0
            };
            out.text(
                "text",
                &[
                    ("class", "value"),
                    ("x", &num(xcenter(j))),
                    ("y", &num(vy)),
                    ("text-anchor", "middle"),
                ],
                t,
            );
        }
    }
    // with a fixed scale, lines are clipped at the plot's edges
    let clipped = mscale.is_some();
    if clipped {
        out.open("defs", &[]);
        out.open("clipPath", &[("id", &clipid)]);
        out.leaf(
            "rect",
            &[
                ("x", &num(plotleft)),
                ("y", &num(plottop)),
                ("width", &num(plotright - plotleft)),
                ("height", &num(PLOT_HEIGHT)),
            ],
        );
        out.close("clipPath");
        out.close("defs");
    }
    let clip_url = format!("url(#{clipid})");
    let line_svg = |out: &mut Out, cls: &str, s: &Series| {
        let pts: Vec<Option<(usize, f64, f64, &Decimal)>> = (0..n)
            .map(|j| s.value_at(j).map(|q| (j, xcenter(j), yofline(to_d(q)), q)))
            .collect();
        for run in runs(&pts) {
            let xy: Vec<(f64, f64)> = run.iter().map(|(_, x, y, _)| (*x, *y)).collect();
            let points = (line_points(dates.is_some(), &xy).iter())
                .map(|(x, y)| format!("{},{}", num(*x), num(*y)))
                .collect::<Vec<_>>()
                .join(" ");
            out.leaf(
                "polyline",
                &[("class", &format!("line {cls}")), ("points", &points)],
            );
        }
        // invisible larger targets, for tooltips
        let mut targets: Vec<(usize, f64, f64, &Decimal)> = pts.iter().flatten().copied().collect();
        if dates.is_some() {
            // the points where the value changes, and the first
            let mut changed = Vec::new();
            for (i, p) in targets.iter().enumerate() {
                if i == 0 || p.3 != targets[i - 1].3 {
                    changed.push(*p);
                }
            }
            targets = changed;
        }
        if targets.len() > 400 {
            let mut columns: BTreeMap<i64, (usize, f64, f64, &Decimal)> = BTreeMap::new();
            for p in targets {
                columns.insert((p.1 / 4.0).floor() as i64, p);
            }
            targets = columns.into_values().collect();
        }
        for (j, x, y, q) in targets {
            let lbl = xlabels.get(j).cloned().unwrap_or_default();
            out.tooltip(&tooltip(&lbl, s, q), |out| {
                out.leaf(
                    "circle",
                    &[
                        ("class", "hit"),
                        ("cx", &num(x)),
                        ("cy", &num(y)),
                        ("r", "6"),
                    ],
                );
            });
        }
    };
    let with_clip = |out: &mut Out, f: &dyn Fn(&mut Out)| {
        if clipped {
            out.open("g", &[("clip-path", &clip_url)]);
            f(out);
            out.close("g");
        } else {
            f(out);
        }
    };
    with_clip(out, &|out| {
        for d in &lines {
            line_svg(out, &d.class, d.series);
        }
    });
    // the total: a line, or a mark across each period's bars
    if let Some(t) = mtotal {
        if t.mark == Mark::Line || bars.is_empty() {
            with_clip(out, &|out| line_svg(out, "total", t));
        } else {
            for (j, lbl) in xlabels.iter().enumerate() {
                if let Some(q) = t.value_at(j)
                    && hasbars(j)
                {
                    let mut tip = tooltip(lbl, t, q);
                    if let Some(Some(v)) = vlabels.get(j) {
                        tip.push_str(&format!(" · {v}"));
                    }
                    out.tooltip(&tip, |out| {
                        let y = num(yof(to_d(q)));
                        let halfw = groupwidth / 2.0 + 4.0;
                        out.leaf(
                            "line",
                            &[
                                ("class", "mark total"),
                                ("x1", &num(xcenter(j) - halfw)),
                                ("x2", &num(xcenter(j) + halfw)),
                                ("y1", &y),
                                ("y2", &y),
                            ],
                        );
                    });
                }
            }
        }
    }
    // the average: a dashed line across the plot
    if let Some((tip, q)) = maverage {
        out.tooltip(&tip, |out| {
            let y = num(yof(to_d(&q)));
            out.leaf(
                "line",
                &[
                    ("class", "average"),
                    ("x1", &num(plotleft)),
                    ("x2", &num(plotright)),
                    ("y1", &y),
                    ("y2", &y),
                ],
            );
        });
    }
    PLOT_HEIGHT + 32.0 + labelroom
}

/// The sign of a quantity: 1, 0 or -1 (signum).
fn signum(q: &Decimal) -> i32 {
    if q.is_zero() {
        0
    } else if q.is_negative() {
        -1
    } else {
        1
    }
}

/// The points a line is drawn through: a lone point as a short dash
/// across it, otherwise thinned, and stepped on a date scale
/// (linePoints).
fn line_points(by_date: bool, ps: &[(f64, f64)]) -> Vec<(f64, f64)> {
    match ps {
        [(x, y)] => vec![(x - 6.0, *y), (x + 6.0, *y)],
        _ if by_date => thin_points(&drop_flat(steps(ps))),
        _ => thin_points(ps),
    }
}

/// The runs of consecutive values present (runs).
fn runs<T: Copy>(ms: &[Option<T>]) -> Vec<Vec<T>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    for m in ms {
        match m {
            Some(v) => run.push(*v),
            None if !run.is_empty() => out.push(std::mem::take(&mut run)),
            None => {}
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// Place x axis labels centred at their positions, kept within the chart,
/// leaving out any that would overlap the previous one (placeXLabels).
fn place_x_labels(ls: &[(f64, String)]) -> Vec<(f64, String)> {
    let mut kept: Vec<(f64, String)> = Vec::new();
    for (x, lbl) in ls {
        let halfw = text_width(lbl) / 2.0;
        let x = x.min(WIDTH - PAD - halfw).max(PAD + halfw);
        if let Some((px, plbl)) = kept.last()
            && x - halfw < px + text_width(plbl) / 2.0 + 12.0
        {
            continue;
        }
        kept.push((x, lbl.clone()));
    }
    kept
}

/// Draw categories (categoriesSvg): as vertical bars, or as horizontal
/// ones beyond 30 categories; categories without values left out; with a
/// single series, each category's bar in its own account type's colour.
/// Its height.
#[allow(clippy::too_many_arguments)]
fn categories_svg(
    out: &mut Out,
    top: f64,
    mscale: Option<&Decimal>,
    unit: &Amount,
    drawn: &[DrawSeries],
    mtotal: Option<&Series>,
    mtotalmark: Option<&Series>,
    vlabels: &[Option<String>],
    labels: &[ChartLabel],
) -> f64 {
    let category_coloured = |lbl: &ChartLabel, d: &DrawSeries<'_>| -> String {
        match (drawn.len(), lbl.typ) {
            (1, Some(at)) => colour_class(Colour::Type(main_account_type(at), 0)),
            _ => d.class.clone(),
        }
    };
    let rows: Vec<CategoryRow> = (labels.iter().enumerate())
        .filter(|(j, _)| drawn.iter().any(|d| d.series.value_at(*j).is_some()))
        .map(|(j, l)| CategoryRow {
            label: l.clone(),
            index: j,
            segs: (drawn.iter())
                .filter_map(|d| {
                    d.series
                        .value_at(j)
                        .map(|q| (category_coloured(l, d), d.series, q.clone()))
                })
                .collect(),
        })
        .collect();
    let mmark = mtotalmark.map(|t| t.label.full.clone());
    let value_text = |j: usize, q: &Decimal| -> Option<String> {
        if vlabels.is_empty() {
            Some(show_value(unit, q))
        } else {
            vlabels.get(j).cloned().flatten()
        }
    };
    let row_value = |j: usize| -> Option<Decimal> {
        match (mtotal, drawn) {
            (Some(t), _) => t.value_at(j).cloned(),
            (None, [d]) => d.series.value_at(j).cloned(),
            _ => None,
        }
    };
    if rows.len() <= MAX_COLUMNS {
        columns_svg(
            out,
            top,
            mscale,
            unit,
            &rows,
            &row_value,
            &value_text,
            mmark.as_deref(),
        )
    } else {
        bars_svg(
            out,
            top,
            mscale,
            unit,
            &rows,
            &row_value,
            &value_text,
            mmark.as_deref(),
        )
    }
}

/// A category's row: its label, its x position's index, and its segments
/// (class, series, value).
struct CategoryRow<'a> {
    label: ChartLabel,
    index: usize,
    segs: Vec<(String, &'a Series, Decimal)>,
}

/// Draw categories as vertical bars, names below (angled if they don't
/// fit), values above the bars where they fit (columnsSvg). Its height.
#[allow(clippy::too_many_arguments)]
fn columns_svg(
    out: &mut Out,
    top: f64,
    mscale: Option<&Decimal>,
    unit: &Amount,
    rows: &[CategoryRow],
    row_value: &dyn Fn(usize) -> Option<Decimal>,
    value_text: &dyn Fn(usize, &Decimal) -> Option<String>,
    mmark: Option<&str>,
) -> f64 {
    let n = rows.len();
    let stacked_sums = |sign: i32| -> Vec<f64> {
        (rows.iter())
            .map(|r| {
                r.segs
                    .iter()
                    .filter(|(_, _, q)| signum(q) == sign)
                    .map(|(_, _, q)| to_d(q))
                    .sum()
            })
            .collect()
    };
    let (lo, hi, ticks, tickdecimals) = panel_scale(
        plot_intervals(PLOT_HEIGHT),
        mscale,
        stacked_sums(-1).into_iter().fold(0.0, f64::min),
        stacked_sums(1).into_iter().fold(0.0, f64::max),
    );
    let ticklabel = |t: f64| tick_label(unit, tickdecimals, &ticks, t);
    let ticksright = PAD
        + ticks
            .iter()
            .map(|t| text_width(&ticklabel(*t)))
            .fold(0.0, f64::max)
        + 12.0;
    let plotright = WIDTH - PAD;
    let bandwith = |left: f64| (plotright - left) / n.max(1) as f64;
    let short_name = |l: &ChartLabel| -> String {
        if l.full.chars().count() > 30 {
            l.full.chars().take(29).collect::<String>() + "…"
        } else {
            l.full.clone()
        }
    };
    let namewidth = rows
        .iter()
        .map(|r| text_width(&short_name(&r.label)))
        .fold(0.0, f64::max);
    let angled = namewidth > bandwith(ticksright) - 4.0;
    let plotleft = if angled {
        (rows.iter().enumerate())
            .map(|(i, r)| {
                PAD + 0.71 * text_width(&short_name(&r.label))
                    - 4.0
                    - bandwith(ticksright) * (i as f64 + 0.5)
            })
            .fold(0.0, f64::max)
            .max(ticksright)
    } else {
        ticksright
    };
    let plottop = top + 20.0;
    let plotbottom = plottop + PLOT_HEIGHT;
    let yof = |v: f64| plottop + PLOT_HEIGHT * (hi - clamp(lo, hi, v)) / (hi - lo);
    let band = bandwith(plotleft);
    let xcenter = |i: usize| plotleft + band * (i as f64 + 0.5);
    let barwidth = (band * 0.6).clamp(1.0, 64.0);
    let labelheight = if angled {
        namewidth * 0.71 + 16.0
    } else {
        24.0
    };
    let anyneg = rows
        .iter()
        .any(|r| r.segs.iter().any(|(_, _, q)| q.is_negative()));
    let negroom = if anyneg {
        if mscale.is_some() { 22.0 } else { 16.0 }
    } else {
        0.0
    };
    gridlines(out, &ticks, (plotleft, plotright), &yof, &ticklabel);
    for (i, r) in rows.iter().enumerate() {
        let x = xcenter(i);
        let name = short_name(&r.label);
        let ly = plotbottom + negroom + 16.0;
        if angled {
            let rotate = format!("rotate(-45 {} {})", num(x + 4.0), num(ly));
            out.text(
                "text",
                &[
                    ("class", "label"),
                    ("x", &num(x + 4.0)),
                    ("y", &num(ly)),
                    ("text-anchor", "end"),
                    ("transform", &rotate),
                ],
                &name,
            );
        } else {
            out.text(
                "text",
                &[
                    ("class", "label"),
                    ("x", &num(x)),
                    ("y", &num(ly)),
                    ("text-anchor", "middle"),
                ],
                &name,
            );
        }
        let tip = |d: &Series, q: &Decimal| {
            let mut parts = vec![r.label.full.clone()];
            for p in [d.label.full.clone(), show_value(unit, q)] {
                if !parts.contains(&p) {
                    parts.push(p);
                }
            }
            parts.join(" · ")
        };
        let bx = x - barwidth / 2.0;
        for sign in [1, -1] {
            let segs: Vec<(&str, &Series, &Decimal)> = (r.segs.iter())
                .filter(|(_, _, q)| signum(q) == sign)
                .map(|(c, s, q)| (c.as_str(), *s, q))
                .collect();
            let sign = sign as f64;
            stack(out, true, (lo, hi), &yof, bx, barwidth, &segs, &tip, sign);
        }
        if let (Some(tname), Some(q)) = (mmark, row_value(r.index)) {
            let mut parts = vec![r.label.full.clone()];
            let more = [
                Some(tname.to_string()),
                Some(show_value(unit, &q)),
                value_text(r.index, &q),
            ];
            for p in more.into_iter().flatten() {
                if !parts.contains(&p) {
                    parts.push(p);
                }
            }
            out.tooltip(&parts.join(" · "), |out| {
                let y = num(yof(to_d(&q)));
                out.leaf(
                    "line",
                    &[
                        ("class", "mark total"),
                        ("x1", &num(bx - 4.0)),
                        ("x2", &num(bx + barwidth + 4.0)),
                        ("y1", &y),
                        ("y2", &y),
                    ],
                );
            });
        }
        if let Some(q) = row_value(r.index)
            && let Some(v) = value_text(r.index, &q)
        {
            let pos: f64 = r
                .segs
                .iter()
                .filter(|(_, _, a)| *a > Decimal::ZERO)
                .map(|(_, _, a)| to_d(a))
                .sum();
            let neg: f64 = r
                .segs
                .iter()
                .filter(|(_, _, a)| a.is_negative())
                .map(|(_, _, a)| to_d(a))
                .sum();
            let cliproom = if pos > hi || neg < lo { 6.0 } else { 0.0 };
            let vy = if !q.is_negative() {
                yof(pos) - 6.0 - cliproom
            } else {
                yof(neg) + 14.0 + cliproom
            };
            if text_width(&v) <= band - 2.0 {
                out.text(
                    "text",
                    &[
                        ("class", "value"),
                        ("x", &num(x)),
                        ("y", &num(vy)),
                        ("text-anchor", "middle"),
                    ],
                    &v,
                );
            }
        }
    }
    PLOT_HEIGHT + 22.0 + negroom + labelheight
}

/// Draw categories as horizontal bars, names on the left (indented as in
/// the report), values in a column on the right (barsSvg). Its height.
#[allow(clippy::too_many_arguments)]
fn bars_svg(
    out: &mut Out,
    top: f64,
    mscale: Option<&Decimal>,
    unit: &Amount,
    rows: &[CategoryRow],
    row_value: &dyn Fn(usize) -> Option<Decimal>,
    value_text: &dyn Fn(usize, &Decimal) -> Option<String>,
    mmark: Option<&str>,
) -> f64 {
    let stacked_sums = |sign: i32| -> Vec<f64> {
        (rows.iter())
            .map(|r| {
                r.segs
                    .iter()
                    .filter(|(_, _, q)| signum(q) == sign)
                    .map(|(_, _, q)| to_d(q))
                    .sum()
            })
            .collect()
    };
    let (lo, hi, ticks, tickdecimals) = panel_scale(
        5,
        mscale,
        stacked_sums(-1).into_iter().fold(0.0, f64::min),
        stacked_sums(1).into_iter().fold(0.0, f64::max),
    );
    let bare_unit = Amount {
        commodity: Sym::from(""),
        ..unit.clone()
    };
    let ticklabel = |t: f64| tick_label(&bare_unit, tickdecimals, &[], t);
    let labelwidth = (rows.iter())
        .map(|r| 12.0 * r.label.indent as f64 + text_width(&r.label.short))
        .fold(0.0, f64::max)
        .min(WIDTH * 0.4);
    let valuewidth = (rows.iter())
        .filter_map(|r| row_value(r.index).and_then(|q| value_text(r.index, &q)))
        .map(|v| text_width(&v))
        .fold(0.0, f64::max);
    let edge = ticks
        .iter()
        .map(|t| text_width(&ticklabel(*t)))
        .fold(0.0, f64::max)
        / 2.0;
    let plotleft = PAD + labelwidth.max(edge) + 12.0;
    let plotright = WIDTH - PAD - valuewidth.max(edge) - 16.0;
    let plottop = top + 4.0;
    let plotbottom = plottop + ROW_HEIGHT * rows.len() as f64;
    let xof = |v: f64| plotleft + (plotright - plotleft) * (clamp(lo, hi, v) - lo) / (hi - lo);
    let tickstep = match ticks.as_slice() {
        [t1, t2, ..] => t2 - t1,
        _ => 1.0,
    };
    let labelevery = ((ticks
        .iter()
        .map(|t| text_width(&ticklabel(*t)))
        .fold(0.0, f64::max)
        + 12.0)
        / (xof(tickstep) - xof(0.0)).max(1.0))
    .ceil()
    .max(1.0) as i64;
    let thickness = 16.0;
    for t in &ticks {
        let class = if *t == 0.0 { "axis" } else { "grid" };
        out.leaf(
            "line",
            &[
                ("class", class),
                ("x1", &num(xof(*t))),
                ("x2", &num(xof(*t))),
                ("y1", &num(plottop)),
                ("y2", &num(plotbottom)),
            ],
        );
        if round(t / tickstep).rem_euclid(labelevery) == 0 {
            out.text(
                "text",
                &[
                    ("class", "tick"),
                    ("x", &num(xof(*t))),
                    ("y", &num(plotbottom + 21.0)),
                    ("text-anchor", "middle"),
                ],
                &ticklabel(*t),
            );
        }
    }
    for (i, r) in rows.iter().enumerate() {
        let ymid = plottop + ROW_HEIGHT * (i as f64 + 0.5);
        out.text(
            "text",
            &[
                ("class", "label"),
                ("x", &num(PAD + 12.0 * r.label.indent as f64)),
                ("y", &num(ymid + 5.0)),
            ],
            &r.label.short,
        );
        if let Some(q) = row_value(r.index)
            && let Some(v) = value_text(r.index, &q)
        {
            out.text(
                "text",
                &[
                    ("class", "value"),
                    ("x", &num(WIDTH - PAD)),
                    ("y", &num(ymid + 5.0)),
                    ("text-anchor", "end"),
                ],
                &v,
            );
        }
        for sign in [1.0, -1.0] {
            let segs: Vec<(&str, &Series, &Decimal)> = (r.segs.iter())
                .filter(|(_, _, q)| signum(q) as f64 == sign)
                .map(|(c, s, q)| (c.as_str(), *s, q))
                .collect();
            let tip = |s: &Series, q: &Decimal| {
                [
                    r.label.full.clone(),
                    s.label.full.clone(),
                    show_value(unit, q),
                ]
                .join(" · ")
            };
            stack(
                out,
                false,
                (lo, hi),
                &xof,
                ymid,
                thickness,
                &segs,
                &tip,
                sign,
            );
        }
        if let (Some(tname), Some(q)) = (mmark, row_value(r.index)) {
            let mut parts = vec![r.label.full.clone()];
            let more = [
                Some(tname.to_string()),
                Some(show_value(unit, &q)),
                value_text(r.index, &q),
            ];
            for p in more.into_iter().flatten() {
                if !parts.contains(&p) {
                    parts.push(p);
                }
            }
            out.tooltip(&parts.join(" · "), |out| {
                let x = num(xof(to_d(&q)));
                out.leaf(
                    "line",
                    &[
                        ("class", "mark total"),
                        ("x1", &x),
                        ("x2", &x),
                        ("y1", &num(ymid - thickness / 2.0 - 4.0)),
                        ("y2", &num(ymid + thickness / 2.0 + 4.0)),
                    ],
                );
            });
        }
    }
    rows.len() as f64 * ROW_HEIGHT + 32.0
}

/// Horizontal lines at the y axis ticks between these x positions, the
/// zero line as the axis, with the ticks' labels left of them.
fn gridlines(
    out: &mut Out,
    ticks: &[f64],
    (plotleft, plotright): (f64, f64),
    yof: &dyn Fn(f64) -> f64,
    ticklabel: &dyn Fn(f64) -> String,
) {
    for t in ticks {
        let class = if *t == 0.0 { "axis" } else { "grid" };
        out.leaf(
            "line",
            &[
                ("class", class),
                ("x1", &num(plotleft)),
                ("x2", &num(plotright)),
                ("y1", &num(yof(*t))),
                ("y2", &num(yof(*t))),
            ],
        );
        out.text(
            "text",
            &[
                ("class", "tick"),
                ("x", &num(plotleft - 8.0)),
                ("y", &num(yof(*t) + 5.0)),
                ("text-anchor", "end"),
            ],
            &ticklabel(*t),
        );
    }
}

/// One direction of a stacked bar (vstack, and barsSvg's rows): segments
/// from zero outward (up or right if sign is 1), 2px apart except tiny
/// ones, each with its tooltip, with a rounded end; clipped beyond the
/// scale's range, with a triangle there instead. `at` places a value
/// along the bar, which is `size` across: from `cross` if vertical,
/// centred on it if not.
#[allow(clippy::too_many_arguments)]
fn stack(
    out: &mut Out,
    vertical: bool,
    (lo, hi): (f64, f64),
    at: &dyn Fn(f64) -> f64,
    cross: f64,
    size: f64,
    segs: &[(&str, &Series, &Decimal)],
    tip: &dyn Fn(&Series, &Decimal) -> String,
    sign: f64,
) {
    // the bar's first side across it, and its middle
    let (side, mid) = if vertical {
        (cross, cross + size / 2.0)
    } else {
        (cross - size / 2.0, cross)
    };
    let cums: Vec<f64> = segs
        .iter()
        .scan(0.0, |acc, (_, _, q)| {
            *acc += to_d(q);
            Some(*acc)
        })
        .collect();
    let clipped = cums.last().is_some_and(|c| *c > hi || *c < lo);
    let outer = cums.last().map_or(at(0.0), |c| at(*c));
    let segments = |out: &mut Out| {
        let mut c0 = 0.0;
        for (k, ((class, s, q), c1)) in segs.iter().zip(&cums).enumerate() {
            let gap = if k > 0 && (at(*c1) - at(c0)).abs() >= 4.0 {
                2.0
            } else {
                0.0
            };
            // up is towards a smaller y
            let (len, path) = if vertical {
                let ybase = at(c0) - sign * gap;
                let len = (ybase - at(*c1)) * sign;
                (len, bar_path(true, side, ybase, size, -sign * len, false))
            } else {
                let xbase = at(c0) + sign * gap;
                let len = (at(*c1) - xbase) * sign;
                (len, bar_path(false, xbase, side, size, sign * len, false))
            };
            if len > 0.5 {
                out.tooltip(&tip(s, q), |out| {
                    out.leaf("path", &[("class", &format!("fill {class}")), ("d", &path)]);
                    if is_hatched(s, q) {
                        out.leaf(
                            "path",
                            &[("fill", &format!("url(#{HATCH_ID})")), ("d", &path)],
                        );
                    }
                });
            }
            c0 = *c1;
        }
    };
    let (a, b) = if vertical {
        (side, at(0.0))
    } else {
        (at(0.0), side)
    };
    rounded_end(
        out,
        !clipped,
        vertical,
        a,
        b,
        size,
        outer - at(0.0),
        &segments,
    );
    if clipped {
        let edge = at(if sign > 0.0 { hi } else { lo });
        clip_mark(out, vertical, edge, mid, size, sign);
    }
}

/// A bar's segments clipped to its outline with a rounded outer end
/// (roundedEnd), when rounded and not tiny.
#[allow(clippy::too_many_arguments)]
fn rounded_end(
    out: &mut Out,
    rounded: bool,
    vertical: bool,
    a: f64,
    b: f64,
    size: f64,
    len: f64,
    segments: &dyn Fn(&mut Out),
) {
    if !rounded || len.abs() < 0.5 {
        segments(out);
        return;
    }
    let clipid = format!(
        "hledger-chart-clip-bar{}",
        [
            if vertical {
                "v".to_string()
            } else {
                "h".to_string()
            },
            num(a),
            num(b),
            num(len)
        ]
        .join("_")
    );
    out.open("defs", &[]);
    out.open("clipPath", &[("id", &clipid)]);
    out.leaf("path", &[("d", &bar_path(vertical, a, b, size, len, true))]);
    out.close("clipPath");
    out.close("defs");
    out.open("g", &[("clip-path", &format!("url(#{clipid})"))]);
    segments(out);
    out.close("g");
}

/// A small triangle pointing outward from a plot's edge, marking a bar
/// clipped there (clipMark).
fn clip_mark(out: &mut Out, vertical: bool, edge: f64, mid: f64, barsize: f64, sign: f64) {
    let w = barsize.clamp(6.0, 10.0);
    let pts = [(mid - w / 2.0, 0.0), (mid + w / 2.0, 0.0), (mid, w / 2.0)];
    let points = (pts.iter())
        .map(|(across, outward)| {
            let (a, b) = if vertical {
                (*across, edge - sign * outward)
            } else {
                (edge + sign * outward, *across)
            };
            format!("{},{}", num(a), num(b))
        })
        .collect::<Vec<_>>()
        .join(" ");
    out.leaf("polygon", &[("class", "clip"), ("points", &points)]);
}

/// The points of a step line: each value holds until the next point's x
/// position (steps).
fn steps(ps: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = ps.iter().take(1).copied().collect();
    for w in ps.windows(2) {
        let ((_, py), (x, y)) = (w[0], w[1]);
        out.push((x, py));
        out.push((x, y));
    }
    out
}

/// Without the points inside runs at the same height (dropFlat).
fn drop_flat(ps: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for p in ps {
        let n = out.len();
        if n >= 2 && out[n - 2].1 == out[n - 1].1 && out[n - 1].1 == p.1 {
            out[n - 1] = p;
        } else {
            out.push(p);
        }
    }
    out
}

/// A line's points reduced to at most four per pixel column: the first,
/// lowest, highest and last, in their order (thinPoints).
fn thin_points(ps: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ps.len() {
        let column = round(ps[i].0);
        let mut j = i;
        while j < ps.len() && round(ps[j].0) == column {
            j += 1;
        }
        let group = &ps[i..j];
        if group.len() <= 4 {
            out.extend_from_slice(group);
        } else {
            let extreme = |better: &dyn Fn(f64, f64) -> bool| -> usize {
                let mut best = 0;
                for (k, p) in group.iter().enumerate().skip(1) {
                    if better(p.1, group[best].1) {
                        best = k;
                    }
                }
                best
            };
            let mut keep: Vec<usize> = vec![
                0,
                extreme(&|a, b| a < b),
                extreme(&|a, b| a > b),
                group.len() - 1,
            ];
            keep.sort_unstable();
            keep.dedup();
            out.extend(keep.into_iter().map(|k| group[k]));
        }
        i = j;
    }
    out
}

/// Up to 8 ticks from the first to the last date at calendar boundaries,
/// with labels (dateTicks).
fn date_ticks(d0: Day, d1: Day) -> Vec<(Day, String)> {
    let (y0, y1) = (d0.year(), d1.year());
    let inrange = |d: &Day| *d >= d0 && *d <= d1;
    let mut candidates: Vec<Vec<(Day, String)>> = Vec::new();
    for k in [1, 2, 7, 14] {
        let mut ticks = Vec::new();
        let mut d = d0;
        while d <= d1 {
            ticks.push((d, d.to_string()));
            d = d.add_days(k);
        }
        candidates.push(ticks);
    }
    for k in [1, 2, 3, 6] {
        let mut ticks = Vec::new();
        for y in y0..=y1 {
            for m in 1..=12 {
                if (m - 1) % k == 0
                    && let Some(d) = Day::from_ymd(y, m, 1)
                    && inrange(&d)
                {
                    ticks.push((d, format!("{y:04}-{m:02}")));
                }
            }
        }
        candidates.push(ticks);
    }
    for k in [1, 2, 5, 10, 20, 50, 100, 1000] {
        let mut ticks = Vec::new();
        for y in y0..=y1 {
            if y.rem_euclid(k) == 0
                && let Some(d) = Day::from_ymd(y, 1, 1)
                && inrange(&d)
            {
                ticks.push((d, y.to_string()));
            }
        }
        candidates.push(ticks);
    }
    candidates
        .into_iter()
        .find(|c| c.len() <= 8)
        .unwrap_or_default()
}

/// An SVG path for a bar growing from a base line, its far end rounded
/// (radius up to 3px) if requested (barPath).
fn bar_path(vertical: bool, a: f64, b: f64, size: f64, len: f64, rounded: bool) -> String {
    let sgn = if len < 0.0 { -1.0 } else { 1.0 };
    let r = if rounded {
        3.0f64.min(size / 2.0).min(len.abs())
    } else {
        0.0
    };
    let parts: Vec<String> = if vertical {
        let e = b + len;
        vec![
            "M".into(),
            num(a),
            num(b),
            "V".into(),
            num(e - sgn * r),
            "Q".into(),
            num(a),
            num(e),
            num(a + r),
            num(e),
            "H".into(),
            num(a + size - r),
            "Q".into(),
            num(a + size),
            num(e),
            num(a + size),
            num(e - sgn * r),
            "V".into(),
            num(b),
            "Z".into(),
        ]
    } else {
        let e = a + len;
        vec![
            "M".into(),
            num(a),
            num(b),
            "H".into(),
            num(e - sgn * r),
            "Q".into(),
            num(e),
            num(b),
            num(e),
            num(b + r),
            "V".into(),
            num(b + size - r),
            "Q".into(),
            num(e),
            num(b + size),
            num(e - sgn * r),
            num(b + size),
            "H".into(),
            num(a),
            "Z".into(),
        ]
    };
    parts.join(" ")
}

/// The scale of a panel's values (panelScale): a nice scale covering their
/// range, or with a fixed scale, exactly the range it spans, with the nice
/// scale's ticks within it.
fn panel_scale(
    intervals: usize,
    mscale: Option<&Decimal>,
    datamin: f64,
    datamax: f64,
) -> (f64, f64, Vec<f64>, usize) {
    match mscale {
        Some(s) if *s > Decimal::ZERO => {
            let (hi, neg) = scale_spans_f64(to_d(s), datamax.max(0.0), datamin.min(0.0).abs());
            let lo = -neg;
            let (_, _, ticks, decimals) = nice_scale(intervals, lo, hi);
            let eps = (hi - lo) * 1e-9;
            let ticks = ticks
                .into_iter()
                .filter(|t| *t >= lo - eps && *t <= hi + eps)
                .collect();
            (lo, hi, ticks, decimals)
        }
        _ => nice_scale(intervals, datamin, datamax),
    }
}

fn clamp(lo: f64, hi: f64, v: f64) -> f64 {
    v.min(hi).max(lo)
}

/// A nice linear scale covering the range, in about this many intervals:
/// its ends, its ticks at multiples of 1, 2 or 5 times a power of ten, and
/// the decimal places the ticks need (niceScale).
fn nice_scale(intervals: usize, lo0: f64, hi0: f64) -> (f64, f64, Vec<f64>, usize) {
    let (a, b) = if hi0 - lo0 <= 0.0 {
        (lo0, lo0 + 1.0)
    } else {
        (lo0, hi0)
    };
    let rawstep = (b - a) / intervals as f64;
    let magnitude = 10f64.powi(rawstep.log10().floor() as i32);
    let err = rawstep / magnitude;
    let step = if err >= 50f64.sqrt() {
        10.0 * magnitude
    } else if err >= 10f64.sqrt() {
        5.0 * magnitude
    } else if err >= 2f64.sqrt() {
        2.0 * magnitude
    } else {
        magnitude
    };
    let kmin = (a / step).floor() as i64;
    let kmax = (b / step).ceil() as i64;
    let ticks = (kmin..=kmax).map(|k| k as f64 * step).collect();
    let decimals = (-((step.log10() + 1e-9).floor() as i64)).max(0) as usize;
    (kmin as f64 * step, kmax as f64 * step, ticks, decimals)
}

/// A y axis tick's label: its value in the unit's style with this many
/// decimal places, the commodity shown only on the outermost tick (the top
/// one, or the bottom one if the top is zero) (tickLabel).
fn tick_label(unit: &Amount, decimals: usize, ticks: &[f64], t: f64) -> String {
    let unittick = match ticks.last() {
        Some(top) if *top != 0.0 => Some(*top),
        _ => ticks.first().copied(),
    };
    let u = Amount {
        commodity: if unittick == Some(t) {
            unit.commodity
        } else {
            Sym::from("")
        },
        style: AmountStyle {
            precision: Precision::Fixed(decimals as u8),
            ..unit.style
        },
        ..unit.clone()
    };
    show_value(&u, &tick_quantity(decimals, t))
}

/// A double as a decimal with this many places, rounded half to even
/// (realFracToDecimal). The mantissa is unbounded, like Haskell's round:
/// a chart of 12345678901234567890 USD has ticks beyond i64.
fn tick_quantity(decimals: usize, t: f64) -> Decimal {
    let m = (t * 10f64.powi(decimals as i32)).round_ties_even();
    let mantissa: dashu_int::IBig = format!("{m:.0}")
        .parse()
        .expect("a rounded double is a whole number");
    Decimal::new(mantissa, decimals as u8)
}

/// The chart's styles, with a dark variant if wanted (stylesheet), without
/// the rules for shading the past and future, which upstream's svgToday
/// option uses and this port does not have.
fn stylesheet(dark: bool) -> String {
    let mut lines: Vec<String> = [
        ".hledger-chart{max-width:100%;height:auto;font-family:system-ui,-apple-system,\"Segoe UI\",sans-serif;font-size:14px}",
        ".hledger-chart .title{font-size:17px;font-weight:600}",
        ".hledger-chart .heading{font-size:15px;font-weight:600}",
        ".hledger-chart .value,.hledger-chart .tick{font-variant-numeric:tabular-nums}",
        ".hledger-chart .grid,.hledger-chart .axis{stroke-width:1}",
        ".hledger-chart .line{fill:none;stroke-width:3;stroke-linejoin:round;stroke-linecap:round}",
        ".hledger-chart .mark{stroke-width:2.5}",
        ".hledger-chart .average{stroke-width:2;stroke-dasharray:5 4}",
        ".hledger-chart .hit,.hledger-chart .plot{fill:transparent}",
        ".hledger-chart .hatch{stroke-width:2.5}",
    ]
    .map(String::from)
    .to_vec();
    let rules: [(&str, &str, &str); 13] = [
        (".bg", "fill", "surface"),
        (".title", "fill", "ink"),
        (".heading", "fill", "ink"),
        (".label", "fill", "ink2"),
        (".value", "fill", "ink2"),
        (".tick", "fill", "muted"),
        (".grid", "stroke", "grid"),
        (".axis", "stroke", "axis"),
        (".line.total", "stroke", "ink"),
        (".mark.total", "stroke", "ink"),
        (".average", "stroke", "ink"),
        (".clip", "fill", "ink"),
        (".hatch", "stroke", "surface"),
    ];
    let light = [
        ("surface", "#fcfcfb"),
        ("ink", "#0b0b0b"),
        ("ink2", "#52514e"),
        ("muted", "#898781"),
        ("grid", "#e1e0d9"),
        ("axis", "#c3c2b7"),
    ];
    let darkcolours = [
        ("surface", "#1a1a19"),
        ("ink", "#ffffff"),
        ("ink2", "#c3c2b7"),
        ("muted", "#898781"),
        ("grid", "#2c2c2a"),
        ("axis", "#383835"),
    ];
    let mut series_colours: Vec<Colour> = Vec::new();
    use AccountType::*;
    for at in [Asset, Liability, Equity, Revenue, Expense] {
        series_colours.extend((0..MAX_TINTS).map(|k| Colour::Type(at, k)));
    }
    series_colours.extend((0..8).map(Colour::Index));
    series_colours.push(Colour::Other(None));
    series_colours
        .extend([Asset, Liability, Equity, Revenue, Expense].map(|at| Colour::Other(Some(at))));
    series_colours.extend([Colour::Lone, Colour::Neutral]);
    let colour_rules = |isdark: bool, cs: &[(&str, &str)]| -> Vec<String> {
        let mut out: Vec<String> = (rules.iter())
            .filter_map(|(sel, prop, role)| {
                cs.iter()
                    .find(|(r, _)| r == role)
                    .map(|(_, c)| format!(".hledger-chart {sel}{{{prop}:{c}}}"))
            })
            .collect();
        for c in &series_colours {
            let cls = colour_class(*c);
            let hex = colour_hex(isdark, *c);
            out.push(format!(".hledger-chart .fill.{cls}{{fill:{hex}}}"));
            out.push(format!(".hledger-chart .line.{cls}{{stroke:{hex}}}"));
        }
        out
    };
    lines.extend(colour_rules(false, &light));
    if dark {
        lines.push("@media (prefers-color-scheme:dark){".to_string());
        lines.extend(colour_rules(true, &darkcolours));
        lines.push("}".to_string());
    }
    lines.into_iter().map(|l| l + "\n").collect()
}

pub(crate) mod tests {
    use super::*;

    #[test]
    fn tick_quantities_beyond_i64() {
        assert_eq!(tick_quantity(0, 1.5e19).to_string(), "15000000000000000000");
        assert_eq!(tick_quantity(1, -0.25).to_string(), "-0.2");
        assert_eq!(tick_quantity(0, -0.0).to_string(), "0");
    }

    // tests_Hledger_Write_Chart_Svg's pure cases.
    #[cfg_attr(test, test)]
    #[allow(clippy::approx_constant)]
    pub(crate) fn nums() {
        let shown: Vec<String> = [0.0, -0.04, 1.0, 1.26, -3.14159, 760.0]
            .iter()
            .map(|x| num(*x))
            .collect();
        assert_eq!(shown, ["0", "0", "1", "1.3", "-3.1", "760"]);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn nice_scales() {
        assert_eq!(
            nice_scale(5, 0.0, 87.0),
            (0.0, 100.0, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0], 0)
        );
        assert_eq!(
            nice_scale(5, -15.0, 100.0),
            (
                -20.0,
                100.0,
                vec![-20.0, 0.0, 20.0, 40.0, 60.0, 80.0, 100.0],
                0
            )
        );
        assert_eq!(
            nice_scale(5, 0.0, 0.0),
            (
                0.0,
                1.0,
                vec![0.0, 0.2, 0.4, 0.6000000000000001, 0.8, 1.0],
                1
            )
        );
        assert_eq!(
            nice_scale(5, 0.0, 7.3),
            (0.0, 8.0, vec![0.0, 2.0, 4.0, 6.0, 8.0], 0)
        );
        assert_eq!(
            nice_scale(5, 0.0, 117.0),
            (
                0.0,
                120.0,
                vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0, 120.0],
                0
            )
        );
        assert_eq!(
            nice_scale(3, 0.0, 117.0),
            (0.0, 150.0, vec![0.0, 50.0, 100.0, 150.0], 0)
        );
    }

    #[cfg_attr(test, test)]
    pub(crate) fn plot_interval_counts() {
        let counts: Vec<usize> = [260.0, 150.0, 100.0, 40.0]
            .iter()
            .map(|h| plot_intervals(*h))
            .collect();
        assert_eq!(counts, [5, 3, 2, 2]);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn bar_paths() {
        assert_eq!(
            bar_path(true, 10.0, 100.0, 20.0, -50.0, true),
            "M 10 100 V 53 Q 10 50 13 50 H 27 Q 30 50 30 53 V 100 Z"
        );
        assert_eq!(
            bar_path(false, 10.0, 5.0, 12.0, -30.0, false),
            "M 10 5 H -20 Q -20 5 -20 5 V 17 Q -20 17 -20 17 H 10 Z"
        );
    }

    fn panel(series: Vec<Series>) -> Panel {
        Panel {
            unit: Amount::null(),
            series,
            group_names: Vec::new(),
            scale: None,
            layout: BarLayout::Diverging,
            counts: false,
            summary: Vec::new(),
            value_labels: Vec::new(),
        }
    }

    fn series(name: &str, role: SeriesRole, mark: Mark, values: Vec<i64>) -> Series {
        Series {
            label: ChartLabel {
                account: true,
                ..ChartLabel::plain(name)
            },
            role,
            mark,
            values: values
                .into_iter()
                .map(|v| Some(Decimal::from_i64(v)))
                .collect(),
            group: 0,
            sign: 0,
        }
    }

    /// A chart of one line, with these values on successive days from
    /// 2024-01-01.
    fn line_chart(values: Vec<i64>) -> Chart {
        let d0 = Day::from_ymd(2024, 1, 1).unwrap();
        Chart {
            title: String::new(),
            x_axis: XAxis::Dates((0..values.len() as i64).map(|k| d0.add_days(k)).collect()),
            panels: vec![panel(vec![series(
                "a",
                SeriesRole::Item,
                Mark::Line,
                values,
            )])],
        }
    }

    /// A chart of n periods, each with a positive and a negative bar, and
    /// their net total, drawn with this mark.
    fn net_chart(mark: Mark, n: usize) -> Chart {
        Chart {
            title: String::new(),
            x_axis: XAxis::Periods(vec![
                (crate::dates::DateSpan::default(), "p".to_string());
                n
            ]),
            panels: vec![panel(vec![
                series("a", SeriesRole::Item, Mark::Bar, vec![3; n]),
                series("b", SeriesRole::Item, Mark::Bar, vec![-1; n]),
                series("net", SeriesRole::Total, mark, vec![2; n]),
            ])],
        }
    }

    #[cfg_attr(test, test)]
    pub(crate) fn long_line_tooltips() {
        let hits = chart_as_svg(true, &line_chart((1..=1000).collect()))
            .matches("class=\"hit\"")
            .count();
        assert!((150..=220).contains(&hits), "tooltip targets: {hits}");
    }

    #[cfg_attr(test, test)]
    pub(crate) fn line_by_date_tooltips_where_it_changes() {
        let svg = chart_as_svg(true, &line_chart(vec![1, 1, 1, 2, 2, 2]));
        assert_eq!(svg.matches("class=\"hit\"").count(), 2);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn net_total_marks() {
        let marks = |n| {
            chart_as_svg(true, &net_chart(Mark::Marker, n))
                .matches("class=\"mark total\"")
                .count()
        };
        // one per period, and one in the legend; none when the periods are too narrow
        assert_eq!(marks(12), 13);
        assert_eq!(marks(100), 0);
        let svg = chart_as_svg(true, &net_chart(Mark::Line, 1));
        assert_eq!(svg.matches("class=\"mark total\"").count(), 2);
        assert!(!svg.contains("line total"));
    }

    #[cfg_attr(test, test)]
    pub(crate) fn one_point_line_is_a_dash() {
        let svg = chart_as_svg(true, &line_chart(vec![5]));
        let points = svg.split("points=\"").nth(1).unwrap();
        let points = &points[..points.find('"').unwrap()];
        assert_eq!(points.split_whitespace().count(), 2);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn flat_runs() {
        assert_eq!(
            drop_flat(vec![
                (0.0, 1.0),
                (1.0, 1.0),
                (1.0, 1.0),
                (2.0, 1.0),
                (2.0, 2.0),
                (3.0, 2.0)
            ]),
            vec![(0.0, 1.0), (2.0, 1.0), (2.0, 2.0), (3.0, 2.0)]
        );
    }
}
