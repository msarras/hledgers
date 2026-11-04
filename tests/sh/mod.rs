//! A small interpreter for the subset of POSIX sh that the corpus's command lines
//! use, so that they run without a system shell or other external tools. Only
//! `hledger` runs as a separate program; everything else is built in.

mod builtins;
mod exec;
pub mod posix_re;
mod syntax;

use std::path::Path;

pub use exec::{Output, run};
use syntax::{Command, List, Part, Simple};

/// The kind of command line a test case has.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// `[VAR=VALUE ...] hledger ARGS`, with no shell operators.
    Plain,
    /// `./csvtest.sh ARGS` or `./ssvtest.sh ARGS`.
    Csvtest,
    /// A pipeline, list, subshell or redirection within the supported subset.
    Compound,
    /// Outside the supported subset, for the given reason.
    Unsupported(String),
}

impl Shape {
    pub fn label(&self) -> &'static str {
        match self {
            Shape::Plain => "plain",
            Shape::Csvtest => "csvtest",
            Shape::Compound => "pipeline/sequence",
            Shape::Unsupported(_) => "unsupported",
        }
    }
}

/// Classify a command line without running it. `dir` is where it would run,
/// for reading the scripts that `sh FILE` runs.
pub fn classify(cmdline: &str, dir: &Path) -> Shape {
    let list = match syntax::parse(cmdline).and_then(|l| check(&l, dir).map(|()| l)) {
        Ok(l) => l,
        Err(why) => return Shape::Unsupported(why),
    };
    let only = match &list.0[..] {
        [and_or] if and_or.rest.is_empty() => match &and_or.first.0[..] {
            [Command::Simple(s)] if s.redirs.is_empty() => Some(s),
            _ => None,
        },
        _ => None,
    };
    match only
        .and_then(|s| s.words.first())
        .and_then(|w| w.literal())
        .as_deref()
    {
        Some("hledger") => Shape::Plain,
        Some("./csvtest.sh" | "./ssvtest.sh") => Shape::Csvtest,
        _ => Shape::Compound,
    }
}

/// Check every command in a list is one this interpreter runs, with options it
/// supports, so that unsupported cases are known before anything runs.
fn check(list: &List, dir: &Path) -> Result<(), String> {
    for and_or in &list.0 {
        let pipelines = std::iter::once(&and_or.first).chain(and_or.rest.iter().map(|(_, p)| p));
        for cmd in pipelines.flat_map(|p| &p.0) {
            match cmd {
                Command::Subshell(inner, _) => check(inner, dir)?,
                Command::Simple(s) => check_simple(s, dir)?,
            }
        }
    }
    Ok(())
}

fn check_simple(cmd: &Simple, dir: &Path) -> Result<(), String> {
    let words = cmd.words.iter().chain(cmd.assigns.iter().map(|(_, w)| w));
    for part in words.flat_map(|w| &w.0) {
        if let Part::Subst { cmd, .. } = part {
            check_simple(cmd, dir)?;
        }
    }
    for w in &cmd.words {
        exec::check_glob(w, dir)?;
    }
    let Some(first) = cmd.words.first() else {
        return Ok(());
    };
    let name = first.literal().ok_or("a command name from an expansion")?;
    // Arguments with expansions are checked with placeholder values; options
    // are always literal in the corpus.
    let args: Vec<String> = cmd.words[1..]
        .iter()
        .map(|w| {
            w.0.iter()
                .map(|p| match p {
                    Part::Lit { text, .. } => text.as_str(),
                    _ => "x",
                })
                .collect()
        })
        .collect();
    match (name.as_str(), &args[..]) {
        ("hledger" | "./csvtest.sh" | "./ssvtest.sh" | "export", _) => Ok(()),
        ("cd", [] | [_]) => Ok(()),
        ("exit", []) => Ok(()),
        ("exit", [n]) if cmd.words[1].literal().is_none() || n.parse::<i32>().is_ok() => Ok(()),
        ("sh", [c, _]) if c == "-c" => {
            let script = cmd.words[2]
                .literal()
                .ok_or("sh -c with an expanded script")?;
            check(&syntax::parse(&script)?, dir)
        }
        ("sh", [file]) => {
            let script =
                std::fs::read_to_string(dir.join(file)).map_err(|e| format!("sh {file}: {e}"))?;
            check(&syntax::parse(&script)?, dir)
        }
        ("cd" | "exit" | "sh", _) => Err(format!("{name} arguments")),
        _ => builtins::validate(&name, &args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn shape(cmd: &str) -> Shape {
        classify(cmd, Path::new("."))
    }

    #[test]
    fn shapes() {
        assert_eq!(shape("hledger -f- print"), Shape::Plain);
        assert_eq!(
            shape("LANG=C hledger -f sample.journal bal 'a b'"),
            Shape::Plain
        );
        assert_eq!(shape("./csvtest.sh --today 2000-01-01"), Shape::Csvtest);
        assert_eq!(shape("hledger print 2>&1"), Shape::Compound);
        assert_eq!(shape("hledger print | grep -c x"), Shape::Compound);
        assert_eq!(shape("rm -f x; hledger -f x add; rm -f x"), Shape::Compound);
        assert_eq!(shape("hledger x | sed 's/a/b/; s/c/d/'"), Shape::Compound);
        assert_eq!(
            shape(r#"sh -c 'D=$(mktemp -d "${TMPDIR:-/tmp}/x.XXXXXX"); rm -rf "$D"'"#),
            Shape::Compound
        );
        for cmd in [
            "hledger x | ls",
            "for a in b; do hledger $a; done",
            "hledger x | grep -i y",
            "hledger x | sed 1d",
            "sh -c 'ls'",
            "$x y",
        ] {
            assert!(matches!(shape(cmd), Shape::Unsupported(_)), "{cmd}");
        }
    }

    #[test]
    fn globs_that_match_nothing_stay_literal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.j"), "").unwrap();
        assert_eq!(
            classify("hledger bal assets.*etrade status:*", tmp.path()),
            Shape::Plain
        );
        assert!(matches!(
            classify("hledger -f *.j", tmp.path()),
            Shape::Unsupported(_)
        ));
        assert!(matches!(
            classify("hledger -f x/*.j", tmp.path()),
            Shape::Unsupported(_)
        ));
        assert_eq!(stdout(tmp.path(), "echo q* 'a*' x?", ""), "q* a* x?\n");
    }
    fn run_in(dir: &Path, cmd: &str, stdin: &str) -> Output {
        let env = BTreeMap::from([
            ("PATH".to_string(), "/nonexistent".to_string()),
            ("HOME".to_string(), dir.to_string_lossy().into_owned()),
        ]);
        assert_ne!(classify(cmd, dir).label(), "unsupported", "{cmd}");
        run(cmd, stdin.as_bytes(), dir, env, 4242).unwrap()
    }

    fn stdout(dir: &Path, cmd: &str, stdin: &str) -> String {
        String::from_utf8(run_in(dir, cmd, stdin).stdout).unwrap()
    }

    #[test]
    fn evaluation() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        assert_eq!(stdout(d, "echo $$ \"$$\"x", ""), "4242 4242x\n");
        assert_eq!(
            stdout(
                d,
                "printf 'a\\nb\\n' >f; cat f; rm f; test -e f || echo gone",
                ""
            ),
            "a\nb\ngone\n"
        );
        assert_eq!(stdout(d, "false; s=$?; echo $s; exit $s", "").trim(), "1");
        assert_eq!(run_in(d, "false; s=$?; rm -f x; exit $s", "").status, 1);
        assert_eq!(
            stdout(d, "true && echo y || echo n; false && echo y || echo n", ""),
            "y\nn\n"
        );
        assert_eq!(
            stdout(d, "cat | sort -u | head -2", "b\na\nb\nc\n"),
            "a\nb\n"
        );
        assert_eq!(stdout(d, "printf 'x1\\ny\\n' | grep -c x", ""), "1\n");
        assert_eq!(run_in(d, "printf 'y\\n' | grep -c x", "").status, 1);
        assert_eq!(
            stdout(
                d,
                "mkdir -p s/t && cd s/t && pwd >p && cd ../.. && cat s/t/p",
                ""
            ),
            format!("{}/s/t\n", d.display())
        );
        assert_eq!(
            stdout(d, "(cd s; echo in >f); cat s/f; rm -rf s", ""),
            "in\n"
        );
        assert_eq!(
            stdout(
                d,
                r#"x=1; sh -c 'echo "<$x>"'; export x; sh -c 'echo "<$x>"'"#,
                ""
            ),
            "<>\n<1>\n"
        );
        let out = run_in(d, "nosuch=1 hledger x 2>&1 >/dev/null; echo $?", "");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "sh: 1: hledger: not found\n127\n"
        );
        assert_eq!(
            stdout(
                d,
                "t=$(mktemp -d \"${TMPDIR:-/tmp}/sh-test.XXXXXX\"); test -e $t && echo made; rm -rf $t; test -e $t || echo removed",
                ""
            ),
            "made\nremoved\n"
        );
        assert_eq!(
            stdout(
                d,
                "printf 'p\\n' >g; printf 'p\\n' | diff - g && cmp g g && echo same; rm g",
                ""
            ),
            "same\n"
        );
        assert_eq!(
            stdout(d, "printf 'a,b\\n' | tr ',' '\\n' | uniq", ""),
            "a\nb\n"
        );
        assert_eq!(
            stdout(d, "printf '1\\n2\\n3\\n' | tail -n +2", ""),
            "2\n3\n"
        );
    }
}
