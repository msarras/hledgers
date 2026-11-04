//! Discovery, execution, gating and reporting.
//!
//! Every `.test` file under hledger/hledger/test (except `_*` files and the
//! ledger-compat reference suites, which upstream also skips) and tests/rules is
//! parsed, and each case classified by the shape of its command line. Supported
//! cases run against the hledgers binary (or HLEDGERS_TEST_BIN), reached as `hledger`
//! through PATH, in the
//! test file's directory, with HOME a fresh directory per file and XDG_* unset.
//! Files run in parallel; cases within a file in order.
//!
//! The run fails if a parse fails, a case matched by tests/shelltest-gate.txt
//! fails, an unsupported case is missing from tests/shelltest-unsupported.txt, or
//! that list or the gate list has an entry that matches nothing.
//! HLEDGERS_TEST_FILTER=GLOB selects files; HLEDGERS_TEST_VERBOSE=1 shows every
//! failure's details, not just gated ones; HLEDGERS_TEST_BIN=PATH runs another binary.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use regex::Regex;

use super::format::{self, Case, Matcher};
use crate::sh::{self, Shape};

/// The corpus files excluded upstream too: reference suites from Ledger.
const EXCLUDED_DIRS: &[&str] = &[
    "ledger-compat/ledger-baseline",
    "ledger-compat/ledger-regress",
    "ledger-compat/ledger-extra",
];

struct TestFile {
    /// Relative to hledger/hledger/test, or `rules/NAME.test` for this repo's.
    id: String,
    path: PathBuf,
    /// The parsed cases with their shapes, or the parse error.
    cases: Result<Vec<(Case, Shape)>, String>,
}

enum Outcome {
    Pass,
    Fail(String),
    Unsupported(String),
}

pub fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = root
        .join("hledger/hledger/test")
        .canonicalize()
        .expect("hledger/ submodule is missing: run `git submodule update --init`");
    let selected = std::env::var("HLEDGERS_TEST_FILTER").ok().map(|g| glob(&g));
    let verbose = std::env::var("HLEDGERS_TEST_VERBOSE").is_ok_and(|v| v == "1");

    let mut paths = Vec::new();
    find_tests(&corpus, &corpus, &mut paths);
    let corpus_files = paths.len();
    let rules = root.join("tests/rules");
    if let Ok(entries) = std::fs::read_dir(&rules) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".test") {
                paths.push((format!("rules/{name}"), e.path()));
            }
        }
    }
    let repo_files = paths.len() - corpus_files;
    paths.sort();
    let files: Vec<TestFile> = paths
        .into_iter()
        .filter(|(id, _)| selected.as_ref().is_none_or(|re| re.is_match(id)))
        .map(|(id, path)| {
            let dir = path.parent().unwrap().to_path_buf();
            let cases = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| format::parse(&text))
                .map(|cases| {
                    cases
                        .into_iter()
                        .map(|c| {
                            let shape = sh::classify(&c.command, &dir);
                            (c, shape)
                        })
                        .collect()
                });
            TestFile { id, path, cases }
        })
        .collect();

    let gate: Vec<GateEntry> = read_list(&root.join("tests/shelltest-gate.txt"))
        .into_iter()
        .map(|(entry, _)| GateEntry::new(entry))
        .collect();
    let listed: BTreeMap<String, String> = read_list(&root.join("tests/shelltest-unsupported.txt"))
        .into_iter()
        .collect();

    let bin = tempfile::tempdir().expect("temporary directory");
    // HLEDGERS_TEST_BIN points the corpus at another binary, such as a Haskell
    // hledger built from the submodule's commit, to check the harness itself.
    let target = std::env::var_os("HLEDGERS_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hledgers")));
    std::os::unix::fs::symlink(&target, bin.path().join("hledger"))
        .expect("symlink to the binary under test");
    let base_env = base_env(bin.path());

    let results: Vec<Vec<Outcome>> = files.par_iter().map(|f| run_file(f, &base_env)).collect();

    // Report.
    let mut out = String::new();
    let mut problems = Vec::new();
    let total: usize = files
        .iter()
        .filter_map(|f| f.cases.as_ref().ok())
        .map(Vec::len)
        .sum();
    let _ = writeln!(
        out,
        "\nshelltest: {corpus_files} corpus files and {repo_files} repo files found, {} selected, {total} cases",
        files.len()
    );
    let mut shapes: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, shape) in files.iter().filter_map(|f| f.cases.as_ref().ok()).flatten() {
        *shapes.entry(shape.label()).or_default() += 1;
    }
    let _ = writeln!(
        out,
        "cases by shape: {}",
        ["plain", "csvtest", "pipeline/sequence", "unsupported"]
            .iter()
            .map(|s| format!("{s} {}", shapes.get(s).unwrap_or(&0)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let (mut passed, mut failed, mut unsupported, mut gated, mut gated_passed) = (0, 0, 0, 0, 0);
    let mut seen_unsupported = BTreeSet::new();
    let mut gate_used = vec![false; gate.len()];
    let mut table = String::new();
    for (f, outcomes) in files.iter().zip(&results) {
        let cases = match &f.cases {
            Ok(cases) => cases,
            Err(e) => {
                problems.push(format!("{}: parse error: {e}", f.id));
                continue;
            }
        };
        let (mut p, mut x, mut u) = (0, 0, 0);
        for (n, ((case, _), outcome)) in (1..).zip(cases.iter().zip(outcomes)) {
            let id = format!("{}:{n}", f.id);
            let mut in_gate = false;
            for (used, entry) in gate_used.iter_mut().zip(&gate) {
                let hit = entry.matches(&f.id, n);
                *used |= hit;
                in_gate |= hit;
            }
            gated += usize::from(in_gate);
            match outcome {
                Outcome::Pass => {
                    p += 1;
                    gated_passed += usize::from(in_gate);
                }
                Outcome::Fail(details) => {
                    x += 1;
                    if in_gate {
                        problems.push(format!("gated case {id} failed"));
                    }
                    if in_gate || verbose {
                        let _ = writeln!(
                            out,
                            "FAIL {id} (line {}): {}\n{details}",
                            case.line, case.command
                        );
                    }
                }
                Outcome::Unsupported(why) => {
                    u += 1;
                    seen_unsupported.insert(id.clone());
                    let mark = if listed.contains_key(&id) {
                        ""
                    } else {
                        " [not listed]"
                    };
                    let _ = writeln!(out, "UNSUPPORTED {id}: {} ({why}){mark}", case.command);
                    if !listed.contains_key(&id) {
                        problems.push(format!(
                            "unsupported case {id} is not in tests/shelltest-unsupported.txt"
                        ));
                    }
                }
            }
        }
        let _ = writeln!(table, "  {:<44} {p:>5} {x:>5} {u:>5}", f.id);
        (passed, failed, unsupported) = (passed + p, failed + x, unsupported + u);
    }
    let _ = writeln!(
        out,
        "  {:<44} {:>5} {:>5} {:>5}",
        "file", "pass", "fail", "unsup"
    );
    out.push_str(&table);
    let _ = writeln!(
        out,
        "totals: {passed} passed, {failed} failed, {unsupported} unsupported \
         ({} of them listed); gated cases: {gated_passed} of {gated} passed",
        seen_unsupported
            .iter()
            .filter(|id| listed.contains_key(*id))
            .count()
    );

    // Stale list entries, checked only for the files that ran.
    let ran: BTreeSet<&str> = files.iter().map(|f| f.id.as_str()).collect();
    for (id, reason) in &listed {
        if reason.is_empty() {
            problems.push(format!(
                "tests/shelltest-unsupported.txt gives no reason for {id}"
            ));
        }
        let file = id.rsplit_once(':').map_or(id.as_str(), |(f, _)| f);
        if (ran.contains(file) || selected.is_none()) && !seen_unsupported.contains(id) {
            problems.push(format!(
                "tests/shelltest-unsupported.txt lists {id}, which is not an unsupported case"
            ));
        }
    }
    if selected.is_none() {
        for (entry, used) in gate.iter().zip(gate_used) {
            if !used {
                problems.push(format!(
                    "tests/shelltest-gate.txt entry {} matches no case",
                    entry.text
                ));
            }
        }
    }
    print!("{out}");
    if problems.is_empty() {
        println!("shelltest: ok\n");
        ExitCode::SUCCESS
    } else {
        println!("shelltest: FAILED");
        for p in &problems {
            println!("  {p}");
        }
        println!();
        ExitCode::FAILURE
    }
}

/// Collect `.test` files under `dir`, skipping `_`-prefixed names and the
/// excluded suites, as (path relative to `root`, path).
fn find_tests(root: &Path, dir: &Path, found: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if e.file_name().to_string_lossy().starts_with('_') || EXCLUDED_DIRS.contains(&rel.as_str())
        {
            continue;
        }
        if path.is_dir() {
            find_tests(root, &path, found);
        } else if rel.ends_with(".test") {
            found.push((rel, path));
        }
    }
}

/// The environment every case starts from: this process's, without the
/// variables that would let the user's setup leak in.
fn base_env(bin: &Path) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    for k in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "COLUMNS",
        "NO_COLOR",
    ] {
        env.remove(k);
    }
    env.insert("LC_COLLATE".into(), "C".into());
    env.insert("TERM".into(), "dumb".into());
    let path = env.get("PATH").map_or(String::new(), |p| format!(":{p}"));
    env.insert("PATH".into(), format!("{}{path}", bin.display()));
    env
}

fn run_file(f: &TestFile, base_env: &BTreeMap<String, String>) -> Vec<Outcome> {
    static PIDS: AtomicU64 = AtomicU64::new(0);
    let Ok(cases) = &f.cases else {
        return Vec::new();
    };
    let dir = f.path.parent().unwrap();
    let home = tempfile::tempdir().expect("temporary HOME");
    let mut env = base_env.clone();
    env.insert("HOME".into(), home.path().to_string_lossy().into_owned());
    env.insert("PWD".into(), dir.to_string_lossy().into_owned());
    cases
        .iter()
        .map(|(case, shape)| {
            if let Shape::Unsupported(why) = shape {
                return Outcome::Unsupported(why.clone());
            }
            // $$ must be unique across files running at once in the same directory.
            let pid =
                u64::from(std::process::id()) * 100_000 + PIDS.fetch_add(1, Ordering::Relaxed);
            let stdin = case.stdin.as_deref().unwrap_or("");
            match sh::run(&case.command, stdin.as_bytes(), dir, env.clone(), pid) {
                Ok(output) => compare(case, &output),
                Err(why) => Outcome::Fail(format!("unsupported at run time: {why}")),
            }
        })
        .collect()
}

fn compare(case: &Case, out: &sh::Output) -> Outcome {
    let mut details = String::new();
    let streams = [
        ("stdout", &case.stdout, &out.stdout),
        ("stderr", &case.stderr, &out.stderr),
    ];
    for (name, matcher, actual) in streams {
        let Some(m) = matcher else { continue };
        let actual = String::from_utf8_lossy(actual);
        match m.matches(&actual) {
            Ok(true) => {}
            Ok(false) => describe(&mut details, name, m, &actual),
            Err(e) => {
                let _ = writeln!(details, "{name}: {e}");
            }
        }
    }
    let status = out.status.to_string();
    match case.status.matches(&status) {
        Ok(true) => {}
        Ok(false) => describe(&mut details, "exit status", &case.status, &status),
        Err(e) => {
            let _ = writeln!(details, "exit status: {e}");
        }
    }
    if details.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(details)
    }
}

fn describe(out: &mut String, name: &str, m: &Matcher, actual: &str) {
    match m {
        Matcher::Lines(expected) => {
            let diff = similar::TextDiff::from_lines(expected.as_str(), actual);
            let _ = write!(
                out,
                "{name} differs:\n{}",
                diff.unified_diff().header("expected", "actual")
            );
        }
        Matcher::Regex(re) => {
            let _ = writeln!(out, "{name} does not match /{re}/; it was:\n{actual}");
        }
        Matcher::NotRegex(re) => {
            let _ = writeln!(out, "{name} matches !/{re}/; it was:\n{actual}");
        }
        Matcher::Numeric(n) => {
            let _ = writeln!(out, "{name} is {actual}, expected {n}");
        }
        Matcher::NotNumeric(n) => {
            let _ = writeln!(out, "{name} is {actual}, expected anything but {n}");
        }
    }
}

/// A line of tests/shelltest-gate.txt: a glob over test file ids, optionally
/// followed by `:` and case number ranges like `1-5,7`.
struct GateEntry {
    text: String,
    files: Regex,
    cases: Option<Vec<(usize, usize)>>,
}

impl GateEntry {
    fn new(text: String) -> GateEntry {
        let (pattern, cases) = match text.rsplit_once(':') {
            Some((p, ranges)) => (p, Some(parse_ranges(ranges))),
            None => (text.as_str(), None),
        };
        GateEntry {
            files: glob(pattern),
            cases,
            text,
        }
    }

    fn matches(&self, id: &str, n: usize) -> bool {
        self.files.is_match(id)
            && self
                .cases
                .as_ref()
                .is_none_or(|rs| rs.iter().any(|&(a, b)| (a..=b).contains(&n)))
    }
}

/// Non-comment lines of a list file, as (first word, rest of line).
fn read_list(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once(char::is_whitespace) {
            Some((first, rest)) => (first.to_string(), rest.trim().to_string()),
            None => (l.to_string(), String::new()),
        })
        .collect()
}

/// Case number ranges: `1-5,7`.
fn parse_ranges(s: &str) -> Vec<(usize, usize)> {
    s.split(',')
        .map(|r| {
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            let n = |x: &str| {
                x.trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("bad case range {s}"))
            };
            (n(a), n(b))
        })
        .collect()
}

/// A glob over test ids: `*` and `?` stay within a path segment, `**` does not.
fn glob(pattern: &str) -> Regex {
    let mut re = String::from("^");
    let mut cs = pattern.chars().peekable();
    while let Some(c) = cs.next() {
        match c {
            '*' if cs.next_if_eq(&'*').is_some() => re.push_str(".*"),
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).unwrap()
}
