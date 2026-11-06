//! The built-in self-tests that --test runs, with output as tasty's
//! console reporter shows it (runUnitTests).

use std::time::{Duration, Instant};

use crate::i18n::SelfTest as Test;

/// The self-tests: each one's group path below `hledger`, its name, and
/// the test.
fn registry() -> Vec<(&'static [&'static str], &'static str, Test)> {
    const I18N: &[&str] = &["Hledger", "Utils", "I18n"];
    (crate::i18n::SELF_TESTS.iter())
        .map(|(name, t)| (I18N, *name, *t))
        .collect()
}

/// Run the self-tests whose full name (`hledger.GROUP...NAME`) contains
/// the pattern, all of them if it is empty, print the tree of results and
/// a summary, and return the exit status.
pub fn run_unit_tests(pattern: &str) -> i32 {
    let start = Instant::now();
    let tests: Vec<_> = registry()
        .into_iter()
        .filter(|(path, name, _)| {
            let full = format!("hledger.{}.{name}", path.join("."));
            full.contains(pattern)
        })
        .collect();
    let results: Vec<(Result<(), String>, Duration)> = (tests.iter())
        .map(|(_, _, t)| {
            let t0 = Instant::now();
            (t(), t0.elapsed())
        })
        .collect();
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
