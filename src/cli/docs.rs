//! hledger's embedded documentation and the viewers that show it
//! (Hledger.Cli.DocFiles): the manuals of hledger, hledger-ui and
//! hledger-web as plain text, man and info source, their headings as help
//! topics, the tldr example pages, and each builtin command's help text.

use crate::text::hs_lines;
use std::borrow::Cow;
use std::process::Command;

/// A manual's file in the reference source's embeddedfiles directory.
macro_rules! embedded {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/hledger/hledger/embeddedfiles/",
            $name
        ))
    };
}

/// The text file of a builtin command's help, embedded from the reference
/// source.
#[macro_export]
macro_rules! command_doc {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/hledger/hledger/Hledger/Cli/Commands/",
            $name,
            ".txt"
        ))
    };
}

/// The manuals: each tool's name, and its manual as man source, plain text
/// and info source (manuals).
const MANUALS: [(&str, [&str; 3]); 3] = [
    (
        "hledger",
        [
            embedded!("hledger.1"),
            embedded!("hledger.txt"),
            embedded!("hledger.info"),
        ],
    ),
    (
        "hledger-ui",
        [
            embedded!("hledger-ui.1"),
            embedded!("hledger-ui.txt"),
            embedded!("hledger-ui.info"),
        ],
    ),
    (
        "hledger-web",
        [
            embedded!("hledger-web.1"),
            embedded!("hledger-web.txt"),
            embedded!("hledger-web.info"),
        ],
    ),
];

/// The hledger pages of the tldr-pages project (tldrs).
const TLDRS: [(&str, &str); 11] = [
    ("hledger-accounts", embedded!("hledger-accounts.md")),
    ("hledger-add", embedded!("hledger-add.md")),
    ("hledger-aregister", embedded!("hledger-aregister.md")),
    ("hledger-balance", embedded!("hledger-balance.md")),
    ("hledger-balancesheet", embedded!("hledger-balancesheet.md")),
    ("hledger-import", embedded!("hledger-import.md")),
    (
        "hledger-incomestatement",
        embedded!("hledger-incomestatement.md"),
    ),
    ("hledger-print", embedded!("hledger-print.md")),
    ("hledger-ui", embedded!("hledger-ui.md")),
    ("hledger-web", embedded!("hledger-web.md")),
    ("hledger", embedded!("hledger.md")),
];

/// A tool's manual in one format (0 man, 1 text, 2 info), or a not found
/// message (manualMan, manualTxt, manualInfo).
fn manual(tool: &str, format: usize) -> Cow<'static, str> {
    match MANUALS.iter().find(|(t, _)| *t == tool) {
        Some((_, m)) => Cow::Borrowed(m[format]),
        None => {
            let what = ["man page", "text manual", "info manual"][format];
            Cow::Owned(format!("No {what} found for tool: {tool}"))
        }
    }
}

/// A tool's manual title: the first word of its running header, without the
/// section number, as in HLEDGER(1) (manualTitle).
pub fn manual_title(tool: &str) -> Option<String> {
    let txt = manual(tool, 1);
    let first = txt.lines().find(|l| !l.is_empty())?;
    let w = first.split_whitespace().next()?;
    w.contains('(')
        .then(|| w[..w.find('(').unwrap_or(w.len())].to_string())
}

/// The headings of a tool's manual, each with its level (manualHeadings):
/// the title is level 1, an all-caps column-0 heading level 2 (except CSV),
/// another column-0 heading level 3, one indented three spaces level 4.
/// Lines with a run of two spaces are not headings, nor are NAME, SYNOPSIS
/// and DESCRIPTION.
fn manual_headings(tool: &str) -> Vec<(String, usize)> {
    let heading_text = |s: &str| {
        let s = s.trim_end_matches(' ');
        (!s.is_empty() && !s.contains("  ")).then(|| s.to_string())
    };
    let all_caps =
        |s: &str| s.chars().any(char::is_uppercase) && !s.chars().any(char::is_lowercase);
    let txt = manual(tool, 1);
    let sections = hs_lines(&txt).filter_map(|l| {
        let rest = l.trim_start_matches(' ');
        let (h, level) = match l.len() - rest.len() {
            0 => {
                let h = heading_text(rest)?;
                let level = if all_caps(&h) && h != "CSV" { 2 } else { 3 };
                (h, level)
            }
            3 => (heading_text(rest)?, 4),
            _ => return None,
        };
        let lower = h.to_lowercase();
        (!["name", "synopsis", "description"].contains(&lower.as_str())).then_some((h, level))
    });
    manual_title(tool)
        .map(|t| (t, 1))
        .into_iter()
        .chain(sections)
        .collect()
}

/// Every help topic: its unique name, the tool whose manual it is in, the
/// heading and its level. A hledger-ui or hledger-web heading also found in
/// another manual is named with a -ui or -web suffix (manualTopics).
fn manual_topics() -> Vec<(String, &'static str, String, usize)> {
    let raw: Vec<(&str, (String, usize))> = MANUALS
        .iter()
        .flat_map(|(tool, _)| manual_headings(tool).into_iter().map(move |h| (*tool, h)))
        .collect();
    let shared = |h: &str| {
        let lower = h.to_lowercase();
        let mut tools: Vec<&str> = raw
            .iter()
            .filter(|(_, (h2, _))| h2.to_lowercase() == lower)
            .map(|(t, _)| *t)
            .collect();
        tools.dedup();
        tools.len() > 1
    };
    raw.iter()
        .map(|(tool, (h, level))| {
            let name = match *tool {
                "hledger-ui" if shared(h) => format!("{h}-ui"),
                "hledger-web" if shared(h) => format!("{h}-web"),
                _ => h.clone(),
            };
            (name, *tool, h.clone(), *level)
        })
        .collect()
}

/// What a topic resolves to (TopicResolution).
pub enum TopicResolution {
    NotFound,
    /// The tool whose manual has it, and the heading.
    Found(&'static str, String),
    /// The names of the matching topics, with their levels.
    Ambiguous(Vec<(String, usize)>),
}

/// The topic, trimmed of spaces and lower-cased, and those of these topics
/// whose names it matches in this way, without repeats.
fn topics_where(
    topics: &[(String, &'static str, String, usize)],
    topic: &str,
    matches: impl Fn(&str, &str) -> bool,
) -> Vec<(String, &'static str, String, usize)> {
    let t = topic.trim_matches(' ').to_lowercase();
    let mut found: Vec<_> = Vec::new();
    for tp in topics {
        if matches(&tp.0.to_lowercase(), &t) && !found.contains(tp) {
            found.push(tp.clone());
        }
    }
    found
}

/// Resolve a topic against every manual's topics, ignoring case: an exact
/// name, else a unique prefix, else a unique infix; listing every infix
/// match when ambiguous (resolveManualTopic).
pub fn resolve_manual_topic(topic: &str) -> TopicResolution {
    let topics = manual_topics();
    let exacts = topics_where(&topics, topic, |n, t| n == t);
    let prefixes = topics_where(&topics, topic, |n, t| n.starts_with(t));
    let infixes = topics_where(&topics, topic, |n, t| n.contains(t));
    let name_level = |tps: Vec<(String, &str, String, usize)>| {
        TopicResolution::Ambiguous(tps.into_iter().map(|(n, _, _, l)| (n, l)).collect())
    };
    let found = |(_, tool, h, _): &(String, &'static str, String, usize)| {
        TopicResolution::Found(tool, h.clone())
    };
    match (exacts.as_slice(), prefixes.as_slice(), infixes.as_slice()) {
        ([tp], _, _) | ([], [tp], _) | ([], [], [tp]) => found(tp),
        ([], [], []) => TopicResolution::NotFound,
        _ => name_level(infixes),
    }
}

/// The names and levels of the topics containing this one, ignoring case;
/// all of them for the empty topic (manualTopicsMatching).
pub fn manual_topics_matching(topic: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for (n, _, _, l) in topics_where(&manual_topics(), topic, |n, t| n.contains(t)) {
        if !out.contains(&(n.clone(), l)) {
            out.push((n, l));
        }
    }
    out
}

/// A tool's manual as plain text, for output that is not interactive: the
/// whole manual if a topic is requested, otherwise just the introduction, up
/// to the first top-level heading after DESCRIPTION, and a note on reading
/// the rest (printHelpForTopic).
pub fn print_help_for_topic(tool: &str, topic: Option<&str>) -> Result<(), String> {
    let manual = manual(tool, 1);
    let lines: Vec<&str> = hs_lines(&manual).collect();
    let is_top_heading = |l: &str| {
        l.chars().next().is_some_and(char::is_uppercase) && !l.chars().any(char::is_lowercase)
    };
    let intro = (topic.is_none())
        .then(|| {
            let desc = lines.iter().position(|l| *l == "DESCRIPTION")?;
            let end = desc + 1 + lines[desc + 1..].iter().position(|l| is_top_heading(l))?;
            Some(
                lines[..end]
                    .iter()
                    .map(|l| format!("{l}\n"))
                    .collect::<String>(),
            )
        })
        .flatten();
    match intro {
        Some(text) => super::io::print_stdout(&format!("{text}{HELP_TRUNCATION_NOTE}\n")),
        None => super::io::print_stdout(&manual),
    }
    Ok(())
}

const HELP_TRUNCATION_NOTE: &str =
    "(This is just the introduction. To read the full manual, run `hledger help`
in a more capable terminal, or `hledger help -p` (pager), `-m` (man) or `-i`
(info), or see https://hledger.org.)";

/// Write a tool's manual in one format to a temporary file, run a shell
/// command on it, and remove it (withSystemTempFile and callCommand).
fn with_manual_file(
    tool: &str,
    format: usize,
    ext: &str,
    cmd: impl Fn(&str) -> String,
) -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("hledger-{tool}{}.{ext}", std::process::id()));
    let path_str = path.to_string_lossy().into_owned();
    std::fs::write(&path, (manual(tool, format) + "\n").as_bytes())
        .map_err(|e| format!("{path_str}: {e}"))?;
    let result = call_command(&cmd(&single_quote_if_needed(&path_str)), &[]);
    let _ = std::fs::remove_file(&path);
    result
}

/// Run a shell command, with these variables added to its environment,
/// failing if it does (callCommand).
fn call_command(cmd: &str, env: &[(&str, &str)]) -> Result<(), String> {
    let status = crate::platform::shell_command(cmd)
        .envs(env.iter().copied())
        .status()
        .map_err(|e| format!("callCommand: {cmd}: {e}"))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(n) => Err(format!("callCommand: {cmd} (exit {n}): failed")),
        None => Err(format!("callCommand: {cmd}: terminated by a signal")),
    }
}

/// Show a tool's info manual with `info`, at the topic if any
/// (runInfoForTopic).
pub fn run_info_for_topic(tool: &str, topic: Option<&str>) -> Result<(), String> {
    with_manual_file(tool, 2, "info", |f| {
        let node = topic.map(|t| format!(" -n '{t}'")).unwrap_or_default();
        format!("info -f {f}{node}")
    })
}

/// less with vertical white space squashed, searching without case, and
/// the $ metacharacter reachable as \$.
const LESS: &str = "less -s -i --use-backslash";

/// Show a tool's text manual with $PAGER or less, or with less scrolled to
/// the topic if any (runPagerForTopic).
pub fn run_pager_for_topic(tool: &str, topic: Option<&str>) -> Result<(), String> {
    let env_pager = std::env::var("PAGER").unwrap_or_else(|_| LESS.to_string());
    with_manual_file(tool, 1, "txt", |f| match topic {
        None => format!("{env_pager}  {f}"),
        Some(t) => format!("{LESS} -p'^(   )?{t}\\$' {f}"),
    })
}

/// Show a tool's man page with `man`, scrolled to the topic with less if any
/// (runManForTopic).
pub fn run_man_for_topic(tool: &str, topic: Option<&str>) -> Result<(), String> {
    with_manual_file(tool, 0, "1", |f| {
        let pager = topic
            .map(|t| format!("-P \"{LESS} -p'^(   )?{t}\\\\$'\""))
            .unwrap_or_default();
        format!("man {pager} {f}")
    })
}

/// Show one of the tldr pages with a `tldr` client, or else as it is, after
/// a warning (runTldrForPage).
pub fn run_tldr_for_page(name: &str) -> Result<(), String> {
    let Some((_, page)) = TLDRS.iter().find(|(n, _)| *n == name) else {
        return Err(format!("sorry, there are no examples for {name} yet"));
    };
    let fallback = || {
        eprintln!("Warning: could not run tldr --render, using fallback viewer instead.\n");
        super::io::print_stdout(&format!("{page}\n"));
        Ok(())
    };
    if !likely_executables_in_path().iter().any(|e| e == "tldr") {
        return fallback();
    }
    let path = std::env::temp_dir().join(format!("{name}{}.md", std::process::id()));
    let path_str = path.to_string_lossy().into_owned();
    if std::fs::write(&path, format!("{page}\n")).is_err() {
        return fallback();
    }
    let result = call_command(
        &format!("tldr --render {}", single_quote_if_needed(&path_str)),
        &[("TLDR_AUTO_UPDATE_DISABLED", "1")],
    );
    let _ = std::fs::remove_file(&path);
    result.or_else(|_| fallback())
}

/// The URL of a tool's manual on hledger.org for this version (the dev
/// manuals), at a heading's anchor if any (webManualUrl).
pub fn web_manual_url(tool: &str, heading: Option<&str>) -> String {
    let anchor = heading
        .map(|h| {
            let a: String = h
                .chars()
                .map(|c| if c == ' ' { '-' } else { c })
                .flat_map(char::to_lowercase)
                .collect();
            format!("#{a}")
        })
        .unwrap_or_default();
    format!("https://hledger.org/dev/{tool}.html{anchor}")
}

/// Open a web browser on a URL with the first launcher that succeeds, or
/// say what was tried and print the URL (openBrowserOn).
pub fn open_browser_on(url: &str) {
    if let Err(tried) = launch(url) {
        super::io::print_stdout(&format!(
            "Could not start a web browser (tried: {})\nPlease open your browser and visit {url}\n",
            tried.join(", ")
        ));
    }
}

/// Open a URL or file with the first of the platform's launchers that
/// succeeds, or say which were tried (launch).
pub fn launch(url: &str) -> Result<(), Vec<&'static str>> {
    const LAUNCHERS: [&str; 4] = [
        "xdg-open",
        "sensible-browser",
        "gnome-www-browser",
        "firefox",
    ];
    for l in LAUNCHERS {
        let ok = Command::new(l)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }
    Err(LAUNCHERS.to_vec())
}

/// The names of the files in the directories of PATH, sorted and without
/// repeats (likelyExecutablesInPath).
pub fn likely_executables_in_path() -> Vec<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut names: Vec<String> = path
        .split(':')
        .filter_map(|d| std::fs::read_dir(if d.is_empty() { "." } else { d }).ok())
        .flat_map(|entries| entries.flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The string in single quotes if it has quotes or white space
/// (singleQuoteIfNeeded, shellQuoteIfNeeded off Windows).
fn single_quote_if_needed(s: &str) -> String {
    if s.contains(['\'', '"', ' ', '\t', '\n', '\r']) {
        format!("'{s}'")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_titles_and_headings() {
        assert_eq!(manual_title("hledger").as_deref(), Some("HLEDGER"));
        assert_eq!(manual_title("hledger-ui").as_deref(), Some("HLEDGER-UI"));
        let hs = manual_headings("hledger");
        assert_eq!(hs[0], ("HLEDGER".to_string(), 1));
        assert!(hs.contains(&("PART 1: USER INTERFACE".to_string(), 2)));
        assert!(hs.contains(&("CSV".to_string(), 3)));
        assert!(hs.contains(&("Text encoding".to_string(), 4)));
        assert!(!hs.iter().any(|(h, _)| h == "DESCRIPTION"));
    }

    #[test]
    fn topics_resolve_exactly_by_prefix_or_by_infix() {
        let found = |t: &str| match resolve_manual_topic(t) {
            TopicResolution::Found(tool, h) => Some((tool, h)),
            _ => None,
        };
        assert_eq!(
            found("text enc"),
            Some(("hledger", "Text encoding".to_string()))
        );
        assert_eq!(
            found("OPTIONS-ui"),
            Some(("hledger-ui", "OPTIONS".to_string()))
        );
        assert!(matches!(
            resolve_manual_topic("nosuchtopic"),
            TopicResolution::NotFound
        ));
        assert!(matches!(
            resolve_manual_topic("Text"),
            TopicResolution::Ambiguous(_)
        ));
        assert_eq!(
            manual_topics_matching("text"),
            [
                ("Text encoding".to_string(), 4),
                ("Text output".to_string(), 4)
            ]
        );
    }

    #[test]
    fn web_manual_urls() {
        assert_eq!(
            web_manual_url("hledger", Some("Time periods")),
            "https://hledger.org/dev/hledger.html#time-periods"
        );
    }
}
