//! The quick reference card, shown when no command is given (Commands/Quickref.hs).

use crate::cli::version::PROGNAME_AND_VERSION;

const CARD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/hledger/hledger/quickref.txt"
));

/// The card, with the program name and version right-aligned on the title line
/// so that its right edge is level with the card's widest line.
pub fn render() -> String {
    let width = CARD.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut lines = CARD.lines();
    let mut out = String::with_capacity(CARD.len() + 2 * PROGNAME_AND_VERSION.len());
    if let Some(title) = lines.next() {
        let used = title.chars().count() + PROGNAME_AND_VERSION.chars().count();
        out.push_str(title);
        out.extend(std::iter::repeat_n(' ', width.saturating_sub(used).max(1)));
        out.push_str(PROGNAME_AND_VERSION);
        out.push('\n');
    }
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_right_aligned_with_the_widest_line() {
        let card = render();
        let title = card.lines().next().unwrap();
        assert!(title.starts_with("HLEDGER QUICK REFERENCE "));
        assert!(title.ends_with(PROGNAME_AND_VERSION));
        let widest = CARD.lines().map(|l| l.chars().count()).max().unwrap();
        assert_eq!(title.chars().count(), widest);
        assert_eq!(card.lines().count(), CARD.lines().count());
    }
}
