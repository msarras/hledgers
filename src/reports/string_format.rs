//! Line formats for report items, as `--format` gives them
//! (Hledger.Data.StringFormat): literal text and report item fields with a
//! minimum and maximum width, such as `%-20(account)` or `%.10(total)`.
//! The single-column balance report renders them.

use crate::text::is_print;

/// A line format: its components, and how values of several lines are
/// placed: on one line separated by commas, top-aligned, or
/// bottom-aligned (StringFormat).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StringFormat {
    OneLine(Vec<Component>),
    TopAligned(Vec<Component>),
    BottomAligned(Vec<Component>),
}

/// Literal text, or a report item field, left- or right-justified, with a
/// minimum and maximum width (StringFormatComponent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Component {
    Literal(String),
    Field {
        left: bool,
        min: Option<usize>,
        max: Option<usize>,
        field: Field,
    },
}

/// The report item fields a format can show (ReportItemField). Not every
/// report has every one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Account,
    DefaultDate,
    Description,
    /// A balance or running total, always right-justified.
    Total,
    /// The item's indentation, as that many spaces times the minimum width.
    DepthSpacer,
    /// The item's nth field.
    FieldNo(usize),
}

impl StringFormat {
    /// The components.
    pub fn components(&self) -> &[Component] {
        match self {
            StringFormat::OneLine(cs) | StringFormat::TopAligned(cs) => cs,
            StringFormat::BottomAligned(cs) => cs,
        }
    }
}

impl Default for StringFormat {
    /// The balance report's line format,
    /// `%20(total)  %2(depth_spacer)%-(account)` (defaultBalanceLineFormat).
    fn default() -> StringFormat {
        StringFormat::BottomAligned(vec![
            Component::Field {
                left: false,
                min: Some(20),
                max: None,
                field: Field::Total,
            },
            Component::Literal("  ".to_string()),
            Component::Field {
                left: true,
                min: Some(2),
                max: None,
                field: Field::DepthSpacer,
            },
            Component::Field {
                left: true,
                min: None,
                max: None,
                field: Field::Account,
            },
        ])
    }
}

/// A line format parsed: an optional `%^`, `%_` or `%,` choosing top, bottom
/// or one-line alignment (bottom by default), then literal text (with `%%`
/// for `%`) and fields `%[-][MIN][.MAX](NAME)`, NAME being account,
/// depth_spacer, date (which shows the description, as upstream's parser
/// has it), description, total or a field number; a field's minimum width
/// is 0 if not given (parseStringFormat).
pub fn parse_string_format(s: &str) -> Result<StringFormat, String> {
    let (make, mut rest): (fn(Vec<Component>) -> StringFormat, &str) = match s
        .strip_prefix('%')
        .and_then(|r| r.chars().next().map(|c| (c, r)))
    {
        Some(('^', r)) => (StringFormat::TopAligned, &r[1..]),
        Some(('_', r)) => (StringFormat::BottomAligned, &r[1..]),
        Some((',', r)) => (StringFormat::OneLine, &r[1..]),
        _ => (StringFormat::BottomAligned, s),
    };
    let mut components = Vec::new();
    while !rest.is_empty() {
        let offset = s.len() - rest.len();
        let error = |at: usize, what: &str| {
            format!(
                "(unknown):1:{}: unexpected {what}",
                s[..offset + at].chars().count() + 1
            )
        };
        if let Some(r) = rest.strip_prefix("%%") {
            push_literal(&mut components, '%');
            rest = r;
        } else if let Some(r) = rest.strip_prefix('%') {
            let left = r.starts_with('-');
            let r = if left { &r[1..] } else { r };
            let (min, r) = digits(r);
            let (max, r) = match r.strip_prefix('.') {
                Some(r2) => match digits(r2) {
                    (Some(n), r3) => (Some(n), r3),
                    (None, r3) => return Err(error(rest.len() - r3.len(), &shown(r3))),
                },
                None => (None, r),
            };
            let Some(r) = r.strip_prefix('(') else {
                return Err(error(rest.len() - r.len(), &shown(r)));
            };
            let names = [
                ("account", Field::Account),
                ("depth_spacer", Field::DepthSpacer),
                ("date", Field::Description),
                ("description", Field::Description),
                ("total", Field::Total),
            ];
            let (field, r) = match names.iter().find(|(n, _)| r.starts_with(n)) {
                Some((n, f)) => (*f, &r[n.len()..]),
                None => match digits(r) {
                    (Some(n), r2) => (Field::FieldNo(n), r2),
                    (None, _) => return Err(error(rest.len() - r.len(), &shown(r))),
                },
            };
            let Some(r) = r.strip_prefix(')') else {
                return Err(error(rest.len() - r.len(), &shown(r)));
            };
            components.push(Component::Field {
                left,
                min: Some(min.unwrap_or(0)),
                max,
                field,
            });
            rest = r;
        } else {
            let c = rest.chars().next().expect("not empty");
            if !is_print(c) {
                return Err(error(0, &shown(rest)));
            }
            push_literal(&mut components, c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Ok(make(components))
}

/// Add a character to the literal text at the end, or start one.
fn push_literal(components: &mut Vec<Component>, c: char) {
    match components.last_mut() {
        Some(Component::Literal(s)) => s.push(c),
        _ => components.push(Component::Literal(c.to_string())),
    }
}

/// The number at the start of the text, if any, and the rest.
fn digits(s: &str) -> (Option<usize>, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    match s[..end].parse() {
        Ok(n) => (Some(n), &s[end..]),
        // a number too big to be a width is as wide as can be
        Err(_) if end > 0 => (Some(usize::MAX), &s[end..]),
        Err(_) => (None, s),
    }
}

/// What a parse error says was found: the next character, or the end.
fn shown(rest: &str) -> String {
    rest.chars()
        .next()
        .map_or_else(|| "end of input".to_string(), |c| format!("{c:?}"))
}

pub(crate) mod tests {
    use super::*;
    use crate::text::format_text;

    fn field(left: bool, min: Option<usize>, max: Option<usize>, field: Field) -> Component {
        Component::Field {
            left,
            min,
            max,
            field,
        }
    }

    // tests_StringFormat "formatStringHelper".
    #[cfg_attr(test, test)]
    pub(crate) fn formatting_fields() {
        let t = |left, min, max, s: &str| format_text(left, min, max, s);
        assert_eq!(t(false, None, None, " "), " ");
        assert_eq!(t(false, None, None, "description"), "description");
        assert_eq!(
            t(false, Some(20), None, "description"),
            "         description"
        );
        assert_eq!(t(false, None, Some(20), "description"), "description");
        assert_eq!(t(true, None, Some(20), "description"), "description");
        assert_eq!(
            t(true, Some(20), None, "description"),
            "description         "
        );
        assert_eq!(
            t(true, Some(20), Some(20), "description"),
            "description         "
        );
        assert_eq!(t(true, None, Some(3), "description"), "des");
    }

    // tests_StringFormat "parseStringFormat".
    #[cfg_attr(test, test)]
    pub(crate) fn parsing_formats() {
        use Field::*;
        use StringFormat::BottomAligned as B;
        let gives = |s: &str, f: StringFormat| assert_eq!(parse_string_format(s), Ok(f), "{s}");
        gives("", B(vec![]));
        gives("D", B(vec![Component::Literal("D".to_string())]));
        gives("%(date)", B(vec![field(false, Some(0), None, Description)]));
        gives("%(total)", B(vec![field(false, Some(0), None, Total)]));
        gives(
            "Hello %(date)!",
            B(vec![
                Component::Literal("Hello ".to_string()),
                field(false, Some(0), None, Description),
                Component::Literal("!".to_string()),
            ]),
        );
        gives("%-(date)", B(vec![field(true, Some(0), None, Description)]));
        gives(
            "%20(date)",
            B(vec![field(false, Some(20), None, Description)]),
        );
        gives(
            "%.10(date)",
            B(vec![field(false, Some(0), Some(10), Description)]),
        );
        gives(
            "%20.10(date)",
            B(vec![field(false, Some(20), Some(10), Description)]),
        );
        gives(
            "%20(account) %.10(total)",
            B(vec![
                field(false, Some(20), None, Account),
                Component::Literal(" ".to_string()),
                field(false, Some(0), Some(10), Total),
            ]),
        );
        assert!(parse_string_format("\n").is_err());
        // and the alignment prefixes, literal percent signs, field numbers
        gives(
            "%,%%%3(1)",
            StringFormat::OneLine(vec![
                Component::Literal("%".to_string()),
                field(false, Some(3), None, FieldNo(1)),
            ]),
        );
        assert!(parse_string_format("%(foo)").is_err());
        assert!(parse_string_format("%").is_err());
        assert_eq!(StringFormat::default().components().len(), 4);
    }
}
