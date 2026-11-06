//! The quick reference card, shown when no command is given (Commands/Quickref.hs).

use crate::cli::version::progname_and_version;

const CARD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/hledger/hledger/quickref.txt"
));

/// The card, with the program name and version right-aligned on the title line
/// so that its right edge is level with the card's widest line, and the
/// program's name in its command lines (showQuickref, styleQuickref without
/// colour).
pub fn render(progname: &str) -> String {
    let version = progname_and_version(progname);
    let width = CARD.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut lines = CARD.lines();
    let mut out = String::with_capacity(CARD.len() + 2 * version.len());
    if let Some(title) = lines.next() {
        let used = title.chars().count() + version.chars().count();
        out.push_str(title);
        out.extend(std::iter::repeat_n(' ', width.saturating_sub(used).max(1)));
        out.push_str(&version);
        out.push('\n');
    }
    let named = format!("{progname} ");
    for line in lines {
        out.push_str(&line.replace("hledger ", &named));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_right_aligned_with_the_widest_line() {
        let card = render("hledger");
        let title = card.lines().next().unwrap();
        assert!(title.starts_with("HLEDGER QUICK REFERENCE "));
        assert!(title.ends_with(&progname_and_version("hledger")));
        assert!(render("hledgers").contains("Usage:            hledgers [COMMAND]"));
        let widest = CARD.lines().map(|l| l.chars().count()).max().unwrap();
        assert_eq!(title.chars().count(), widest);
        assert_eq!(card.lines().count(), CARD.lines().count());
    }
}
