//! Drawing a chart as text, for terminals (Hledger.Write.Chart.Text):
//! horizontal bars, a row per x position, after an aligned value column;
//! several series as stacked segments, positive values growing right from
//! a zero axis and negative ones left. Lines are drawn as bars too.

use crate::account::AccountType;
use crate::amount::Amount;
use crate::amount::render::{AmountFormat, show_amount_b};
use crate::decimal::Decimal;
use crate::text::{display_width, fit_text};
use crate::write::chart::{
    BarLayout, Chart, Colour, Colouring, Mark, Panel, Series, SeriesRole, XAxis, chart_colouring,
    colour_hex, is_contra, is_hatched, main_account_type, scale_spans, series_colours, series_size,
    stable_slots_favouring_first, total_is_bar_end,
};

/// Options for drawing a text chart (TextChartOpts): the width to fill,
/// and whether to use unicode block characters (otherwise ASCII).
#[derive(Clone, Copy, Debug)]
pub struct TextChartOpts {
    pub width: usize,
    pub unicode: bool,
}

/// How a piece of text is shown (Style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    /// The colour of the Nth ordinary series in a panel.
    Series(usize),
    /// The colour of a main account type, in its Nth variant.
    Type(AccountType, usize),
    /// The colour of a series combining several items, of this main
    /// account type if any.
    Other(Option<AccountType>),
    /// The colour of a chart's only series, when it is not an account.
    Lone,
    Negative,
}

/// A line of styled pieces (StyledLine).
pub type StyledLine = Vec<(Style, String)>;

/// How to colour a text chart (ColourMode): not at all, with the
/// terminal's basic colours, or with exact 24-bit colours (their variants
/// for a dark background if true).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourMode {
    NoColour,
    BasicColour,
    RgbColour(bool),
}

/// Styled lines as text, with ANSI colours or without (styledLinesAsText).
pub fn styled_lines_as_text(mode: ColourMode, lines: &[StyledLine]) -> String {
    let sgr = |codes: &str, t: &str| format!("\x1b[{codes}m{t}\x1b[m");
    let mut out = String::new();
    for l in lines {
        for (st, t) in l {
            let piece = match (mode, st) {
                (ColourMode::NoColour, _) | (_, Style::Plain) => t.clone(),
                (_, Style::Negative) => sgr("31", t),
                (ColourMode::BasicColour, Style::Series(n)) => {
                    // avoiding red, which negative values use
                    const INDEX: [&str; 8] = ["34", "33", "35", "36", "32", "94", "93", "95"];
                    sgr(INDEX[n % INDEX.len()], t)
                }
                (ColourMode::BasicColour, Style::Type(at, n)) => {
                    let base = match at {
                        AccountType::Asset => 4,
                        AccountType::Liability => 3,
                        AccountType::Equity => 5,
                        AccountType::Revenue => 2,
                        AccountType::Expense => 1,
                        _ => 5,
                    };
                    let code = if n % 2 == 0 { 30 + base } else { 90 + base };
                    sgr(&code.to_string(), t)
                }
                (ColourMode::BasicColour, Style::Other(_)) => format!("\x1b[2m{t}\x1b[22m"),
                (ColourMode::BasicColour, Style::Lone) => sgr("36", t),
                (ColourMode::RgbColour(dark), st) => {
                    let colour = match st {
                        Style::Series(n) => Colour::Index(*n),
                        Style::Type(at, n) => Colour::Type(*at, *n),
                        Style::Other(mt) => Colour::Other(*mt),
                        _ => Colour::Lone,
                    };
                    let hex = colour_hex(dark, colour);
                    let c = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
                    format!("\x1b[38;2;{};{};{}m{t}\x1b[m", c(1), c(3), c(5))
                }
            };
            out.push_str(&piece);
        }
        out.push('\n');
    }
    out
}

/// A chart as styled lines of text (chartAsStyledLines): its title if any,
/// then each panel, with a heading naming its commodity when there are
/// several (and it has a symbol), and a legend when it has several series,
/// or an "other" one; "No data" for a chart without panels.
pub fn chart_as_styled_lines(opts: TextChartOpts, chart: &Chart) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    if !chart.title.is_empty() {
        lines.push(vec![(Style::Plain, chart.title.clone())]);
        lines.push(Vec::new());
    }
    if chart.panels.is_empty() {
        lines.push(vec![(Style::Plain, "No data".to_string())]);
    }
    let colouring = chart_colouring(chart);
    for (i, p) in chart.panels.iter().enumerate() {
        if i > 0 {
            lines.push(Vec::new());
        }
        lines.extend(panel_lines(
            opts,
            &chart.x_axis,
            colouring,
            chart.panels.len() > 1,
            p,
        ));
    }
    lines
}

/// A row of a text chart (Row): a label, a second label (eg its group's
/// name), an optional value, the bar's segments (style, fill character,
/// quantity), a value to mark over them, the totals of each side's series
/// in a diverging chart, and a note after the value.
struct Row {
    label: String,
    sub_label: String,
    value: Option<Decimal>,
    segments: Vec<(Style, char, Decimal)>,
    mark: Option<Decimal>,
    sides: (Option<Decimal>, Option<Decimal>),
    note: Option<String>,
}

fn width_of(t: &str) -> usize {
    display_width(t)
}

fn line_width(l: &StyledLine) -> usize {
    l.iter().map(|(_, t)| width_of(t)).sum()
}

fn fit(w: usize, t: &str) -> String {
    fit_text(Some(w as isize), None, false, false, t)
}

fn plain(t: &str) -> (Style, String) {
    (Style::Plain, t.to_string())
}

/// Haskell's round, half to even.
fn round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// (panelLines)
fn panel_lines(
    opts: TextChartOpts,
    xaxis: &XAxis,
    colouring: Colouring,
    show_heading: bool,
    p: &Panel,
) -> Vec<StyledLine> {
    let TextChartOpts { width, unicode } = opts;
    let unit = &p.unit;
    let show_value = |q: &Decimal| {
        let a = Amount {
            quantity: q.clone(),
            ..unit.clone()
        };
        show_amount_b(&AmountFormat::ONE_LINE_NO_COST, &a).s
    };
    let summary_line = (p.summary.iter())
        .map(|su| {
            format!(
                "{} {}",
                su.label,
                show_amount_b(&AmountFormat::ONE_LINE_NO_COST, &su.value).s
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    // the series drawn as bars: all but totals (shown as the value) and
    // markers (not drawn in text)
    let drawn: Vec<&Series> = (p.series.iter())
        .filter(|s| s.role != SeriesRole::Total && s.mark != Mark::Marker)
        .collect();
    let mtotal = p.series.iter().find(|s| s.role == SeriesRole::Total);
    let single = drawn.len() == 1;
    let fillchars: Vec<char> = if unicode {
        "█▓▒░".chars().collect()
    } else {
        "#=+:".chars().collect()
    };
    let fill_char = |i: usize| fillchars[i % fillchars.len()];
    let contrachar = if unicode { '╱' } else { '/' };
    let mut groups: Vec<usize> = Vec::new();
    for s in &drawn {
        if !groups.contains(&s.group) {
            groups.push(s.group);
        }
    }
    let group_name = |k: usize| p.group_names.get(k).cloned().unwrap_or_default();
    let group_sign = |k: usize| {
        (drawn.iter())
            .filter(|s| s.group == k)
            .map(|s| s.sign)
            .find(|s| *s != 0)
            .unwrap_or(1)
    };
    // with the grouped layout and several groups, each x position has a
    // row per group, and the total's own row after them
    let grouped = p.layout == BarLayout::Grouped && p.group_names.len() > 1;
    // otherwise a chart with usual signs and a total is diverging: each
    // row shows its sides' totals at the bar's ends, the total as a mark
    let diverging = !grouped && mtotal.is_some() && drawn.iter().any(|s| s.sign != 0);
    let marktotal = match mtotal {
        Some(t) if !diverging && !grouped && total_is_bar_end(&drawn, t) => None,
        t => t,
    };
    // each series' fill character, chosen by name among those drawn in the
    // same bar: accounts favouring the first characters, "other" series
    // (grey) the last; a series alone in its bar gets the first
    let mut fillslots: Vec<(usize, usize)> = Vec::new();
    let indexed: Vec<(usize, &Series)> = drawn.iter().copied().enumerate().collect();
    let bars: Vec<Vec<(usize, &Series)>> = if grouped {
        (groups.iter())
            .map(|g| {
                indexed
                    .iter()
                    .filter(|(_, s)| s.group == *g)
                    .copied()
                    .collect()
            })
            .collect()
    } else {
        vec![indexed.clone()]
    };
    for bar in &bars {
        if bar.len() == 1 {
            fillslots.push((bar[0].0, 0));
            continue;
        }
        let (items, others): (Vec<_>, Vec<_>) =
            bar.iter().partition(|(_, s)| s.role == SeriesRole::Item);
        let slots_for = |ss: &[(usize, &Series)]| {
            let named: Vec<(String, Decimal)> = (ss.iter())
                .map(|(_, s)| (s.label.full.clone(), series_size(s)))
                .collect();
            stable_slots_favouring_first(0.5, fillchars.len(), &named)
        };
        for ((i, _), k) in items.iter().zip(slots_for(&items)) {
            fillslots.push((*i, k));
        }
        for ((i, _), k) in others.iter().zip(slots_for(&others)) {
            fillslots.push((*i, fillchars.len() - 1 - k));
        }
    }
    let colour_style = |c: Colour| match c {
        Colour::Type(at, k) => Style::Type(at, k),
        Colour::Index(k) => Style::Series(k),
        Colour::Lone => Style::Lone,
        Colour::Neutral | Colour::Total => Style::Plain,
        Colour::Other(mt) => Style::Other(mt),
    };
    let styled: Vec<(Style, char, &Series)> = (series_colours(colouring, &drawn).into_iter())
        .zip(&drawn)
        .enumerate()
        .map(|(i, (c, s))| {
            let slot = (fillslots.iter())
                .find(|(j, _)| *j == i)
                .map_or(0, |(_, k)| *k);
            (colour_style(c), fill_char(slot), *s)
        })
        .collect();
    // in a chart of categories with a single series, each category's bar
    // has its own account type's colour, if known
    let category_style = |j: usize, st: Style| match (xaxis, single) {
        (XAxis::Categories(ls), true) => match ls.get(j).and_then(|l| l.typ) {
            Some(at) => Style::Type(main_account_type(at), 0),
            None => st,
        },
        _ => st,
    };
    let xlabels: Vec<String> = match xaxis {
        XAxis::Categories(ls) => (ls.iter())
            .map(|l| format!("{}{}", " ".repeat(2 * l.indent), l.short))
            .collect(),
        XAxis::Periods(ps) => ps.iter().map(|(_, l)| l.clone()).collect(),
        XAxis::Dates(ds) => ds.iter().map(|d| d.to_string()).collect(),
    };
    // contra amounts are drawn nearest the axis; they and shortfalls hatched
    let segments_at = |j: usize, sers: &[&(Style, char, &Series)]| {
        let mut segs: Vec<(bool, (Style, char, Decimal))> = Vec::new();
        for (st, c, s) in sers.iter().map(|x| **x) {
            if let Some(q) = s.value_at(j).filter(|q| !q.is_zero()) {
                let ch = if is_hatched(s, q) { contrachar } else { c };
                segs.push((is_contra(s, q), (category_style(j, st), ch, q.clone())));
            }
        }
        segs.sort_by_key(|(contra, _)| !contra);
        segs.into_iter().map(|(_, s)| s).collect::<Vec<_>>()
    };
    let signed_total = |sign: i32, j: usize, sers: Vec<&Series>| -> Option<Decimal> {
        let qs: Vec<&Decimal> = sers.iter().filter_map(|s| s.value_at(j)).collect();
        if qs.is_empty() {
            return None;
        }
        let sum = qs.into_iter().fold(Decimal::ZERO, |a, q| &a + q);
        Some(&Decimal::from_i64(i64::from(sign)) * &sum)
    };
    let side_total = |j: usize, sign: i32| {
        signed_total(
            sign,
            j,
            drawn.iter().filter(|s| s.sign == sign).copied().collect(),
        )
    };
    let group_total = |j: usize, k: usize| {
        signed_total(
            group_sign(k),
            j,
            drawn.iter().filter(|s| s.group == k).copied().collect(),
        )
    };
    let all_styled: Vec<&(Style, char, &Series)> = styled.iter().collect();
    let is_categories = matches!(xaxis, XAxis::Categories(_));
    let mut rows: Vec<Row> = Vec::new();
    for (j, lbl) in xlabels.iter().enumerate() {
        if grouped {
            for (i, k) in groups.iter().enumerate() {
                let sers: Vec<&(Style, char, &Series)> =
                    styled.iter().filter(|(_, _, s)| s.group == *k).collect();
                rows.push(Row {
                    label: if i == 0 { lbl.clone() } else { String::new() },
                    sub_label: group_name(*k),
                    value: group_total(j, *k),
                    segments: segments_at(j, &sers),
                    mark: None,
                    sides: (None, None),
                    note: None,
                });
            }
            if let Some(t) = marktotal {
                rows.push(Row {
                    label: String::new(),
                    sub_label: t.label.short.clone(),
                    value: t.value_at(j).cloned(),
                    segments: Vec::new(),
                    mark: t.value_at(j).cloned(),
                    sides: (None, None),
                    note: None,
                });
            }
        } else {
            // categories with no values at all are left out
            if is_categories && styled.iter().all(|(_, _, s)| s.value_at(j).is_none()) {
                continue;
            }
            let rowval = match (mtotal, styled.as_slice()) {
                (Some(t), _) => t.value_at(j).cloned(),
                (None, [(_, _, s)]) => s.value_at(j).cloned(),
                _ => None,
            };
            rows.push(Row {
                label: lbl.clone(),
                sub_label: String::new(),
                value: rowval.clone(),
                segments: segments_at(j, &all_styled),
                mark: if marktotal.is_some() { rowval } else { None },
                sides: (side_total(j, 1), side_total(j, -1)),
                note: p.value_labels.get(j).cloned().flatten(),
            });
        }
    }

    // column widths and scale
    let labelw = (width / 3).min(rows.iter().map(|r| width_of(&r.label)).max().unwrap_or(0));
    let sublabelw = (width / 4).min(
        rows.iter()
            .map(|r| width_of(&r.sub_label))
            .max()
            .unwrap_or(0),
    );
    let valuew = (rows.iter())
        .filter_map(|r| r.value.as_ref())
        .map(|q| width_of(&show_value(q)))
        .max()
        .unwrap_or(0);
    let hasvalues = rows.iter().any(|r| r.value.is_some());
    let notew = (rows.iter())
        .filter_map(|r| r.note.as_deref())
        .map(width_of)
        .max()
        .unwrap_or(0);
    let note_column = |t: &str| -> StyledLine {
        if notew > 0 {
            vec![
                plain("  "),
                plain(&fit_text(Some(notew as isize), None, false, false, t)),
            ]
        } else {
            Vec::new()
        }
    };
    let side_name = |sign: i32| {
        (groups.iter())
            .filter(|k| group_sign(**k) == sign)
            .map(|k| group_name(*k))
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (posname, negname) = (side_name(1), side_name(-1));
    let netname = match (hasvalues, mtotal) {
        (true, Some(t)) => t.label.short.clone(),
        _ => String::new(),
    };
    let side_width = |name: &str, side: fn(&Row) -> &Option<Decimal>| {
        if !diverging {
            return 0;
        }
        (rows.iter())
            .filter_map(|r| side(r).as_ref())
            .map(|q| width_of(&show_value(q)))
            .chain(std::iter::once(width_of(name)))
            .max()
            .unwrap_or(0)
    };
    let negsumw = side_width(&negname, |r| &r.sides.1);
    let possumw = side_width(&posname, |r| &r.sides.0);
    let barw = {
        let used = (if labelw > 0 { labelw + 2 } else { 0 })
            + (if sublabelw > 0 { sublabelw + 2 } else { 0 })
            + (if hasvalues { valuew + 2 } else { 0 })
            + (if notew > 0 { notew + 2 } else { 0 })
            + (if negsumw > 0 { negsumw + 1 } else { 0 })
            + (if possumw > 0 { possumw + 1 } else { 0 });
        (width as isize - used as isize).max(10) as usize
    };
    let zero = Decimal::ZERO;
    let pos_total = |r: &Row| {
        let s = (r.segments.iter())
            .filter(|(_, _, q)| *q > zero)
            .fold(Decimal::ZERO, |a, (_, _, q)| &a + q);
        let m = r
            .mark
            .as_ref()
            .map_or(Decimal::ZERO, |m| m.clone().max(Decimal::ZERO));
        s.max(m)
    };
    let neg_total = |r: &Row| {
        let s = (r.segments.iter())
            .filter(|(_, _, q)| q.is_negative())
            .fold(Decimal::ZERO, |a, (_, _, q)| &a + &q.abs());
        let m = r
            .mark
            .as_ref()
            .map_or(Decimal::ZERO, |m| m.clone().min(Decimal::ZERO).abs());
        s.max(m)
    };
    let maxpos = rows.iter().map(pos_total).max().unwrap_or(Decimal::ZERO);
    let maxneg = rows.iter().map(neg_total).max().unwrap_or(Decimal::ZERO);
    let hasaxis = maxneg > zero;
    // the amounts that fill each side: the largest there, or the parts of
    // a fixed scale
    let (posspan, negspan) = match &p.scale {
        Some(s) if *s > zero => scale_spans(s, &maxpos, &maxneg),
        _ => (maxpos.clone(), maxneg.clone()),
    };
    let area = if hasaxis { barw - 1 } else { barw };
    // cells per unit of value, enough for the values to fill the area (for
    // counts, at most one cell per unit, unless the scale is fixed)
    let scale: f64 = {
        let spans = &posspan + &negspan;
        if spans.is_zero() {
            0.0
        } else {
            let fillscale = area as f64 / spans.to_f64();
            if p.counts && p.scale.is_none() {
                fillscale.min(1.0)
            } else {
                fillscale
            }
        }
    };
    let leftw: usize = if hasaxis {
        round(scale * negspan.to_f64()).max(0) as usize
    } else {
        0
    };
    let rightw = area as isize - leftw as isize;
    let axischar = if unicode { "│" } else { "|" };
    let markchar = if unicode { "┃" } else { "!" };
    let clipped_right = |q: &Decimal| p.scale.is_some() && *q > posspan;
    let clipped_left = |q: &Decimal| p.scale.is_some() && *q > negspan;
    let clipright = if unicode { "▸" } else { ">" };
    let clipleft = if unicode { "◂" } else { "<" };
    let cells = |perunit: i64, q: &Decimal| -> i64 { round(q.to_f64() * scale * perunit as f64) };
    let value_style = |q: &Decimal| {
        if q.is_negative() {
            Style::Negative
        } else {
            Style::Plain
        }
    };
    // segments as whole cells, the cumulative boundaries rounded so the
    // total length is kept, within a maximum width; if their total is
    // clipped, the outermost cell is replaced by a clip mark
    let segment_pieces = |maxw: isize,
                          clipped: &dyn Fn(&Decimal) -> bool,
                          clip: &dyn Fn(&str) -> String,
                          segs: &[(Style, char, Decimal)]|
     -> StyledLine {
        let mut cum = Decimal::ZERO;
        let mut prev: isize = 0;
        let mut pieces: StyledLine = Vec::new();
        for (st, c, q) in segs {
            cum = &cum + q;
            let bound = (cells(1, &cum) as isize).min(maxw);
            let w = bound - prev;
            prev = bound;
            if w > 0 {
                pieces.push((*st, c.to_string().repeat(w as usize)));
            }
        }
        if clipped(&cum)
            && let Some(last) = pieces.last_mut()
        {
            last.1 = clip(&last.1);
        }
        pieces
    };
    let positive_bar = |r: &Row| -> StyledLine {
        let segs: Vec<(Style, char, Decimal)> = r
            .segments
            .iter()
            .filter(|(_, _, q)| *q > zero)
            .cloned()
            .collect();
        match segs.as_slice() {
            [(st, c, q)] if unicode && *c == '█' && r.segments.len() == 1 => {
                if clipped_right(q) {
                    let n = (rightw - 1).max(0) as usize;
                    vec![(*st, format!("{}{clipright}", "█".repeat(n)))]
                } else {
                    let e = (rightw * 8).min(cells(8, q) as isize).max(0) as usize;
                    const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
                    vec![(*st, format!("{}{}", "█".repeat(e / 8), PARTIAL[e % 8]))]
                }
            }
            _ => segment_pieces(
                rightw,
                &clipped_right,
                &|t: &str| {
                    let mut cs: Vec<char> = t.chars().collect();
                    cs.pop();
                    cs.into_iter().collect::<String>() + clipright
                },
                &segs,
            ),
        }
    };
    let negative_bar = |r: &Row| -> StyledLine {
        let segs: Vec<(Style, char, Decimal)> = (r.segments.iter())
            .filter(|(_, _, q)| q.is_negative())
            .map(|(st, c, q)| (*st, *c, q.abs()))
            .collect();
        let pieces = segment_pieces(
            leftw as isize,
            &clipped_left,
            &|t: &str| format!("{clipleft}{}", t.chars().skip(1).collect::<String>()),
            &segs,
        );
        let used: usize = pieces.iter().map(|(_, t)| width_of(t)).sum();
        let mut line = vec![plain(&" ".repeat(leftw.saturating_sub(used)))];
        line.extend(pieces.into_iter().rev());
        line
    };
    // draw a mark over the bar at a value's position (on the axis if zero)
    let overlay_mark = |q: &Decimal, line: StyledLine| -> StyledLine {
        let col: isize = if *q > zero {
            leftw as isize + isize::from(hasaxis) + (cells(1, q) as isize).min(rightw).max(1) - 1
        } else if q.is_negative() {
            leftw as isize - (cells(1, &q.abs()) as isize).min(leftw as isize).max(1)
        } else if hasaxis {
            leftw as isize
        } else {
            0
        };
        overlay_at(col, plain(markchar), line)
    };
    let pad_to = |w: usize, mut l: StyledLine| -> StyledLine {
        let used = line_width(&l);
        l.push(plain(&" ".repeat(w.saturating_sub(used))));
        l
    };
    let row_line = |r: &Row| -> StyledLine {
        let mut line: StyledLine = Vec::new();
        if labelw > 0 {
            line.push(plain("  "));
            line.push(plain(&fit_text(
                Some(labelw as isize),
                Some(labelw as isize),
                true,
                true,
                &r.label,
            )));
        }
        if sublabelw > 0 {
            line.push(plain("  "));
            line.push(plain(&fit_text(
                Some(sublabelw as isize),
                Some(sublabelw as isize),
                true,
                true,
                &r.sub_label,
            )));
        }
        if hasvalues {
            line.push(plain("  "));
            let st = r.value.as_ref().map_or(Style::Plain, value_style);
            let v = r.value.as_ref().map_or(String::new(), show_value);
            line.push((st, fit(valuew, &v)));
        }
        line.extend(note_column(r.note.as_deref().unwrap_or("")));
        line.push(plain("  "));
        if negsumw > 0 {
            let st = r.sides.1.as_ref().map_or(Style::Plain, value_style);
            let v = r.sides.1.as_ref().map_or(String::new(), show_value);
            line.push((st, fit(negsumw, &v)));
            line.push(plain(" "));
        }
        let mut bars: StyledLine = Vec::new();
        if hasaxis {
            bars.extend(negative_bar(r));
            bars.push(plain(axischar));
        }
        bars.extend(positive_bar(r));
        if let Some(m) = &r.mark {
            bars = overlay_mark(m, bars);
        }
        if possumw > 0 {
            line.extend(pad_to(barw, bars));
            line.push(plain(" "));
            let st = r.sides.0.as_ref().map_or(Style::Plain, value_style);
            line.push((st, r.sides.0.as_ref().map_or(String::new(), show_value)));
        } else {
            line.extend(bars);
        }
        line.remove(0);
        line
    };
    let rowlines: Vec<StyledLine> = rows.iter().map(row_line).collect();
    // the styles of the segments actually drawn (a series too small to show
    // at this scale has none)
    let mut drawnstyles: Vec<Style> = Vec::new();
    for l in &rowlines {
        for (st, t) in l {
            if *st != Style::Plain && !t.trim().is_empty() && !drawnstyles.contains(st) {
                drawnstyles.push(*st);
            }
        }
    }
    let mut out: Vec<StyledLine> = Vec::new();
    if show_heading && !unit.commodity.is_empty() {
        out.push(vec![plain(&unit.commodity)]);
    }
    // a legend for the series, omitting any not drawn
    if drawn.len() > 1 || drawn.iter().any(|s| s.role == SeriesRole::Other) {
        let mut items: Vec<StyledLine> = Vec::new();
        for (st, c, s) in &styled {
            if drawnstyles.contains(st) {
                let ch = if s.role == SeriesRole::Shortfall {
                    contrachar
                } else {
                    *c
                };
                items.push(vec![
                    (*st, ch.to_string()),
                    plain(&format!(" {}", s.label.full)),
                ]);
            }
        }
        if let Some(t) = marktotal {
            items.push(vec![plain(markchar), plain(&format!(" {}", t.label.full))]);
        }
        let any_contra = (drawn.iter()).any(|s| s.values.iter().flatten().any(|q| is_contra(s, q)));
        if any_contra {
            items.push(vec![plain(&contrachar.to_string()), plain(" contra")]);
        }
        // items left to right, two spaces apart, a new line when the next
        // won't fit
        let mut legend: Vec<StyledLine> = Vec::new();
        for item in items {
            match legend.last_mut() {
                Some(l) if line_width(l) + 2 + line_width(&item) <= width => {
                    l.push(plain("  "));
                    l.extend(item);
                }
                _ => legend.push(item),
            }
        }
        out.extend(legend);
        out.push(Vec::new());
    }
    if diverging && !(posname.is_empty() && negname.is_empty() && netname.is_empty()) {
        let mut header: StyledLine = Vec::new();
        if labelw > 0 {
            header.push(plain("  "));
            header.push(plain(&" ".repeat(labelw)));
        }
        if hasvalues {
            header.push(plain("  "));
            header.push(plain(&fit(valuew, &netname)));
        }
        header.extend(note_column(""));
        header.push(plain("  "));
        if negsumw > 0 {
            header.push(plain(&fit(negsumw, &negname)));
            header.push(plain(" "));
        }
        if possumw > 0 {
            header.push(plain(&" ".repeat(barw)));
            header.push(plain(" "));
            header.push(plain(&posname));
        }
        header.remove(0);
        out.push(trim_end(header));
    }
    out.extend(rowlines.into_iter().map(trim_end));
    if !p.summary.is_empty() {
        out.push(Vec::new());
        out.push(vec![plain(&summary_line)]);
    }
    out
}

/// The line with the character at a column (of single-width characters)
/// replaced by this piece, padded with spaces if it is shorter
/// (overlayAt).
fn overlay_at(i: isize, new: (Style, String), line: StyledLine) -> StyledLine {
    let mut out = Vec::new();
    let mut col: isize = 0;
    let mut rest = line.into_iter();
    while let Some((st, t)) = rest.next() {
        let n = t.chars().count() as isize;
        if i < col + n {
            if i > col {
                out.push((st, t.chars().take((i - col) as usize).collect()));
            }
            out.push(new);
            // at a column before the start (a negative value beside an
            // axis with no room on its left), the text is kept whole
            if i - col + 1 < n {
                out.push((st, t.chars().skip((i - col + 1).max(0) as usize).collect()));
            }
            out.extend(rest);
            return out;
        }
        out.push((st, t));
        col += n;
    }
    out.push(plain(&" ".repeat((i - col).max(0) as usize)));
    out.push(new);
    out
}

/// The line without trailing spaces (trimEnd).
fn trim_end(mut line: StyledLine) -> StyledLine {
    while let Some((_, t)) = line.last() {
        if t.trim_end().is_empty() {
            line.pop();
        } else {
            break;
        }
    }
    if let Some((_, t)) = line.last_mut() {
        *t = t.trim_end().to_string();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::chart::ChartLabel;

    fn texts(lines: Vec<StyledLine>) -> Vec<String> {
        lines
            .into_iter()
            .map(|l| l.into_iter().map(|(_, t)| t).collect())
            .collect()
    }

    #[test]
    fn overlaying_before_the_start_keeps_the_text() {
        let line = vec![plain("│"), plain("ab")];
        let at = |i| texts(vec![overlay_at(i, plain("┃"), line.clone())]).remove(0);
        assert_eq!(at(-1), "┃│ab");
        assert_eq!(at(0), "┃ab");
        assert_eq!(at(2), "│a┃");
        assert_eq!(at(5), "│ab  ┃");
    }

    // tests_Hledger_Write_Chart_Text.
    #[test]
    fn trimming() {
        assert_eq!(trim_end(vec![plain("a "), plain("  ")]), vec![plain("a")]);
        assert!(trim_end(Vec::new()).is_empty());
    }

    fn chart(values: Vec<i64>) -> Chart {
        let lbl = |t: &str| ChartLabel {
            account: true,
            ..ChartLabel::plain(t)
        };
        Chart {
            title: String::new(),
            x_axis: XAxis::Categories(vec![lbl("a"), lbl("b")]),
            panels: vec![Panel {
                unit: Amount::null(),
                series: vec![Series {
                    label: lbl("s"),
                    role: SeriesRole::Item,
                    mark: Mark::Bar,
                    values: values
                        .into_iter()
                        .map(|v| Some(Decimal::from_i64(v)))
                        .collect(),
                    group: 0,
                    sign: 0,
                }],
                group_names: Vec::new(),
                scale: None,
                layout: BarLayout::Diverging,
                counts: false,
                summary: Vec::new(),
                value_labels: Vec::new(),
            }],
        }
    }

    #[test]
    fn single_series_scaled_to_the_width() {
        let opts = TextChartOpts {
            width: 20,
            unicode: true,
        };
        assert_eq!(
            texts(chart_as_styled_lines(opts, &chart(vec![1, 2]))),
            ["a  1  ███████", "b  2  ██████████████"]
        );
    }

    #[test]
    fn mixed_signs_grow_from_an_axis() {
        let opts = TextChartOpts {
            width: 21,
            unicode: false,
        };
        assert_eq!(
            texts(chart_as_styled_lines(opts, &chart(vec![-1, 2]))),
            ["a  -1  ####|", "b   2      |#########"]
        );
    }
}
