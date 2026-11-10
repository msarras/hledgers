//! The setup command (Hledger.Cli.Commands.Setup): checks of the
//! installation, the terminal and the default journal, with hints.

use std::io::{IsTerminal, Write};

use crate::account::AccountType;
use crate::cli::args::{General, Hidden, Mode};
use crate::cli::conf::{self, Conf};
use crate::cli::opts::CliOpts;
use crate::cli::version::{PACKAGE_VERSION, progname_and_version};

pub static MODE: Mode = Mode {
    name: "setup",
    general: General::Group3,
    hidden: Hidden::None,
    args: None,
    doc: crate::command_doc!("Setup"),
    ..Mode::DEFAULT
};

/// Where the current hledger release is announced
/// (latestHledgerVersionUrlStr).
const LATEST_VERSION_URL: &str = "https://hledger.org/install.html";

/// Check and show the status of the hledger installation, the terminal and
/// the default journal (setup). Contacts hledger.org.
pub fn setup(opts: &CliOpts) -> Result<(), String> {
    println!("Checking your setup (and contacting hledger.org to find out the current release):");
    let latest = latest_hledger_version(LATEST_VERSION_URL);
    let conf = setup_hledger(opts, &latest)?;
    setup_terminal(opts, &conf);
    setup_journal(opts, &conf);
    Ok(())
}

/// A check's result: yes, no or unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ynu {
    Y,
    N,
    U,
}
use crate::platform::find_executables;
use Ynu::{N, U, Y};

/// A check's description, padded to a fixed width (pdesc).
fn pdesc(s: &str) {
    print!("  {s:<40}");
    let _ = std::io::stdout().flush();
}

/// A check's result as pass or fail, and a message (p). Colour is not
/// shown, as elsewhere in this port.
fn p(ok: Ynu, msg: &str) {
    let s = match ok {
        Y => "yes",
        N => " no",
        U => "  ?",
    };
    println!(" {s}  {msg}");
}

/// A check's result as information, and a message (i).
fn i(ok: Ynu, msg: &str) {
    p(ok, msg);
}

/// The last of the config file's general arguments satisfying a predicate
/// (conflookup).
fn conf_lookup(conf: &Option<Result<Conf, String>>, pred: impl Fn(&str) -> bool) -> Option<String> {
    match conf {
        Some(Ok(c)) => c.lookup("general").into_iter().rev().find(|a| pred(a)),
        _ => None,
    }
}

/// A version number's parts, if it is one (toVersion).
fn to_version(s: &str) -> Option<Vec<u64>> {
    s.split('.').map(|p| p.parse().ok()).collect()
}

/// Check the hledger installation (setupHledger); give the config file, if
/// one was found, or its error.
fn setup_hledger(
    opts: &CliOpts,
    latest: &Result<String, String>,
) -> Result<Option<Result<Conf, String>>, String> {
    println!("hledger");
    let os = match std::env::consts::OS {
        "macos" => "macos".to_string(),
        o => o.to_string(),
    };
    let arch = std::env::consts::ARCH;
    let osversion = if os == "linux" {
        std::process::Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| crate::text::strip(&String::from_utf8_lossy(&o.stdout)).to_string())
    } else {
        None
    };
    pdesc("is running on");
    let osdesc = match osversion {
        Some(v) => format!("{os} {v}"),
        None => os,
    };
    println!("      {osdesc} on {arch}");

    pdesc("is built with a supported compiler/RTS");
    p(Y, &format!("rustc {}", env!("HLEDGERS_RUSTC_VERSION")));

    pdesc("is a native binary for this machine ?");
    p(Y, arch);

    let progversion = progname_and_version(&opts.progname);
    let installed = to_version(PACKAGE_VERSION).unwrap_or_default();
    pdesc("is a released version ?");
    if installed.last().is_some_and(|v| *v < 98) {
        p(Y, &progversion);
    } else {
        i(N, &progversion);
    }

    pdesc("is up to date ? checking latest...");
    match latest {
        Err(e) => p(U, &format!("couldn't read {LATEST_VERSION_URL} {e}")),
        Ok(v) => match to_version(v) {
            None => p(U, "couldn't parse latest version number"),
            Some(lv) => p(
                if installed >= lv { Y } else { N },
                &format!("latest is {v}, {PACKAGE_VERSION} is installed"),
            ),
        },
    }

    pdesc("is installed in PATH (this version) ?");
    let failmsg = " Some of this info may not apply to that hledger version. Continuing anyway..";
    match find_executables("hledger").first() {
        None => p(N, failmsg),
        Some(exe) => match try_hledger_args(&[&["--version", "--no-conf"], &["--version"]]) {
            Err(e) => p(U, &format!("hledger --version failed: {e}")),
            Ok(out) => {
                let out = out.trim_end();
                let mut words = out.split_whitespace();
                let parsed = words.next().is_some_and(|w| w.starts_with("hledger"))
                    && words
                        .next()
                        .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()));
                if !parsed {
                    p(U, &format!("couldn't parse hledger --version: {out}"));
                    let _ = std::io::stdout().flush();
                    std::process::exit(1);
                }
                if out != progversion {
                    p(
                        N,
                        &format!(
                            "\n A different hledger version was found in PATH: {out}\n at: {exe}\n{failmsg}"
                        ),
                    );
                } else {
                    p(Y, exe);
                }
            }
        },
    }

    pdesc("has a system text encoding configured ?");
    let encoding = crate::write::fods::locale_encoding_name();
    if encoding.eq_ignore_ascii_case("ascii") {
        p(
            N,
            &format!("{encoding}, please configure an encoding for non-ascii data"),
        );
    } else {
        p(Y, &format!("{encoding}, data files must use this encoding"));
    }

    pdesc("has a user config file ?");
    let user = conf::user_conf_files().into_iter().next();
    let local = conf::conf_files()
        .into_iter()
        .next()
        .filter(|f| !conf::user_conf_files().contains(f));
    match &user {
        Some(f) => i(
            Y,
            &format!("{f}{}", if local.is_some() { " (overridden)" } else { "" }),
        ),
        None => i(N, ""),
    }
    pdesc("has a local config file ?");
    match &local {
        Some(f) => i(Y, f),
        None => i(N, ""),
    }
    if user.is_none() && local.is_none() {
        return Ok(None);
    }
    pdesc("the config file is readable ?");
    Ok(Some(match conf::get_conf(&Default::default(), false) {
        Err(e) => {
            p(N, &e);
            Err(e)
        }
        Ok((c, _)) => {
            p(Y, "");
            Ok(c)
        }
    }))
}

/// Check the terminal (setupTerminal): its type, colour, the pager and
/// less's options, box-drawing characters.
fn setup_terminal(opts: &CliOpts, conf: &Option<Result<Conf, String>>) {
    println!("terminal");
    let env = |v: &str| std::env::var(v).ok();
    pdesc("the TERM variable is defined ?");
    let term = env("TERM");
    let dumb = term.as_deref().is_some_and(|t| t.to_lowercase() == "dumb");
    match &term {
        None => i(N, "terminal type is unknown"),
        Some(v) => i(
            Y,
            &format!(
                "{v}{}",
                if dumb {
                    " (color and styles will not be used)"
                } else {
                    ""
                }
            ),
        ),
    }

    pdesc("the terminal supports ANSI color ?");
    let is_terminal = std::io::stdout().is_terminal();
    // hSupportsANSIColor: a terminal that is not dumb, or Emacs's shell
    let supports_color =
        (is_terminal && !dumb) || (std::env::var_os("INSIDE_EMACS").is_some() && dumb);
    let truecolor = env("COLORTERM").is_some_and(|c| c == "truecolor" || c == "24bit");
    if !supports_color {
        i(
            N,
            if is_terminal {
                ""
            } else {
                "stdout is not a terminal"
            },
        );
    } else if truecolor {
        i(Y, "24-bit color (COLORTERM is set)");
    } else if term.as_deref().unwrap_or("").contains("256color") {
        i(Y, "256 colors");
    } else {
        i(Y, "16 colors");
    }

    pdesc("the NO_COLOR variable is defined ?");
    let no_color = std::env::var_os("NO_COLOR").is_some();
    i(if no_color { Y } else { N }, "");

    // the value after a flag's = or space, as y/yes/always, n/no/never or auto (parseYNA)
    let yna = |a: &str| {
        let v = a.rsplit(['=', ' ']).next().unwrap_or(a).to_lowercase();
        match v.as_str() {
            "y" | "yes" | "always" => Some(true),
            "n" | "no" | "never" => Some(false),
            _ => None,
        }
    };
    pdesc("--color is configured by config file ?");
    let conf_color = match conf_lookup(conf, |a| {
        a.starts_with("--color") || a.starts_with("--colour")
    }) {
        None => {
            i(N, "");
            None
        }
        Some(a) => {
            i(Y, &a);
            yna(&a)
        }
    };

    pdesc("hledger will use color by default ?");
    let color_option = crate::console::color_option(&opts.input.prog_args);
    let use_color = conf_color.unwrap_or_else(|| crate::console::use_color_on_stdout(color_option));
    if use_color {
        p(Y, "");
    } else {
        let option_no = color_option
            .map(str::to_lowercase)
            .is_some_and(|c| matches!(c.as_str(), "n" | "no" | "never"));
        i(
            N,
            if conf_color == Some(false) {
                "disabled by config file"
            } else if option_no {
                "disabled by --color"
            } else if no_color {
                "disabled by NO_COLOR"
            } else if dumb {
                "disabled by TERM=dumb"
            } else if !is_terminal {
                "stdout is not a terminal"
            } else if !supports_color {
                "the terminal does not support ANSI color"
            } else {
                ""
            },
        );
    }

    pdesc("the PAGER variable is defined ?");
    match env("PAGER") {
        None => i(N, ""),
        Some(v) => i(Y, &v),
    }

    pdesc("--pager is configured by config file ?");
    let conf_pager = match conf_lookup(conf, |a| a.starts_with("--pager")) {
        None => {
            i(N, "");
            None
        }
        Some(a) => {
            i(Y, &a);
            yna(&a)
        }
    };

    pdesc("hledger will use a pager when needed ?");
    // findPager
    let pager = (env("PAGER").into_iter())
        .chain(["less".to_string(), "more".to_string()])
        .find_map(|p| find_executables(&p).into_iter().next());
    match pager {
        None => i(N, "no pager was found"),
        Some(_) if conf_pager == Some(false) => i(N, "disabled in config file"),
        Some(pager) => {
            p(Y, &pager);
            let base = std::path::Path::new(&pager)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if base == "more" {
                pdesc("the MORE variable is defined ?");
                match env("MORE") {
                    None => i(N, ""),
                    Some(v) => i(Y, &v),
                }
            }
            if base == "less" {
                let (hledger_less, less) = (env("HLEDGER_LESS"), env("LESS"));
                pdesc("the LESS variable is defined ?");
                match &less {
                    None => i(N, ""),
                    Some(v) => i(
                        Y,
                        &format!(
                            "{v}{}",
                            if hledger_less.is_some() {
                                " (overridden)"
                            } else {
                                ""
                            }
                        ),
                    ),
                }
                pdesc("the HLEDGER_LESS variable is defined ?");
                match &hledger_less {
                    None => i(N, ""),
                    Some(v) => i(Y, v),
                }
                let color = crate::console::use_color_on_stdout(color_option);
                let value =
                    crate::cli::io::less_var_value(hledger_less.as_deref(), less.as_deref(), color);
                if hledger_less.is_none() {
                    pdesc("adjusting LESS var for consistent UX ?");
                    i(Y, &value);
                }
                pdesc("less is working, with these options ?");
                if crate::cli::io::less_is_working(&value) {
                    p(Y, "");
                } else {
                    p(
                        N,
                        "less --version shows a problem, check LESS/HLEDGER_LESS settings",
                    );
                }
            }
        }
    }

    pdesc("box-drawing chars are used by default ?");
    if conf_lookup(conf, |a| a == "--pretty").is_some() {
        p(Y, "");
    } else {
        i(N, "you can add --pretty to enable them");
    }
}

/// Check the default journal (setupJournal): that it can be read and
/// appended to, whether it includes files, declares its commodities and
/// accounts, has accounts of each basic type, and which checks the config
/// file enables.
fn setup_journal(opts: &CliOpts, conf: &Option<Result<Conf, String>>) {
    println!("journal");
    pdesc("the LEDGER_FILE variable is defined ?");
    match std::env::var("LEDGER_FILE").ok().as_deref() {
        Some("") => i(N, "defined, but with a null value"),
        Some(f) => i(Y, f),
        None => i(N, ""),
    }

    pdesc("a default journal file is readable ?");
    let trim = |s: &str| s.strip_prefix("Error: ").unwrap_or(s).to_string();
    let path = opts
        .possible_journal_file_paths()
        .map(|ps| ps.into_iter().next().unwrap_or_default());
    let f = match path {
        Err(e) => return p(N, &trim(&e)),
        Ok(f) => f,
    };
    let iopts = crate::read::InputOpts::default();
    let j = match crate::read::read_journal_file(&iopts, &f) {
        Err(e) => {
            if crate::read::file_exists(&f) {
                p(N, &format!("{f}:\n{}", trim(&e)));
            } else {
                p(N, &format!("{f} (not found)"));
            }
            return journal_filesystem_can_append(&f);
        }
        Ok(j) => j,
    };
    p(Y, &f);
    journal_filesystem_can_append(&f);

    pdesc("it includes additional files ?");
    if j.files.len() > 1 {
        i(Y, &format!("{} files", j.files.len() - 1));
    } else {
        i(N, "");
    }

    pdesc("all commodities are declared ?");
    let undeclared = crate::read::checks::journal_undeclared_commodities(&j);
    if undeclared.is_empty() {
        p(Y, &format!("{} commodities", j.commodities().len()));
    } else {
        p(N, &format!("{} undeclared commodities", undeclared.len()));
    }

    use AccountType::*;
    let basic = [Asset, Liability, Equity, Revenue, Expense, Cash];
    let detected: Vec<AccountType> = j.account_types.values().copied().collect();
    let missing_types: Vec<AccountType> = (basic.iter().copied())
        .filter(|t| !detected.iter().any(|d| d.is_subtype_of(*t)))
        .collect();
    let missing = |ts: &[AccountType]| ts.iter().any(|t| missing_types.contains(t));
    let mut affected = Vec::new();
    if missing(&[Asset, Liability]) {
        affected.push("bs");
    }
    if missing(&[Equity]) && !missing(&[Asset, Liability]) {
        affected.push("bse");
    }
    if missing(&[Revenue, Expense]) {
        affected.push("is");
    }
    if missing(&[Cash]) {
        affected.push("cf");
    }
    let mut used: Vec<&str> = j.postings().map(|p| &*p.p.account).collect();
    used.sort();
    used.dedup();
    let mut declared: Vec<&str> = j.declared_accounts.iter().map(|(a, _)| &**a).collect();
    declared.sort();
    declared.dedup();
    let mut names: Vec<&str> = (declared.iter().copied())
        .chain(crate::account::expand_account_names(used.iter().copied()))
        .collect();
    names.sort();
    names.dedup();
    let untyped = names
        .iter()
        .filter(|a| !j.account_types.contains_key(**a))
        .count();
    let undeclared_accts = used.iter().filter(|a| !declared.contains(a)).count();

    pdesc("all accounts are declared ?");
    if undeclared_accts == 0 {
        p(Y, &format!("{} accounts", names.len()));
    } else {
        i(N, &format!("{undeclared_accts} undeclared accounts"));
    }

    pdesc("all accounts have types ?");
    if untyped == 0 {
        p(Y, "");
    } else {
        i(N, &format!("{untyped} accounts without types"));
    }

    pdesc("accounts of all basic types exist ?");
    let shown = |ts: &[AccountType]| ts.iter().map(|t| format!("{t:?}")).collect::<String>();
    if missing_types.is_empty() {
        p(Y, &format!("{} accounts detected", shown(&basic)));
    } else {
        let verb = if affected.len() == 1 {
            " needs "
        } else {
            " need "
        };
        i(
            N,
            &format!(
                "{}{verb}{} accounts",
                affected.join("/"),
                shown(&missing_types)
            ),
        );
    }

    let has = |flags: &[&str]| conf_lookup(conf, |a| flags.contains(&a)).is_some();
    let strict = has(&["-s", "--strict"]);
    pdesc("balance assertions checked by default ?");
    let ignore_assertions = has(&["-I", "--ignore-assertions"]);
    if ignore_assertions && !strict {
        i(N, "you can add -s to check them");
    } else if !strict {
        i(Y, "you can add -I to ignore them");
    } else {
        i(Y, "and can't be ignored, because strict checks are enabled");
    }
    pdesc("lot movements checked by default ?");
    let ignore_lots = has(&["-I", "--ignore-lots"]);
    if ignore_lots && !strict {
        i(N, "you can add -s to check them");
    } else if !strict {
        i(Y, "you can add -I to ignore them");
    } else {
        i(Y, "and can't be ignored, because strict checks are enabled");
    }
    pdesc("strict checks checked by default ?");
    if strict {
        i(Y, "commodities and accounts must be declared");
    } else {
        i(N, "you can add -s to check them");
    }
}

/// Check that the journal's directory honours appending, as add and import
/// need (journalFilesystemCanAppend).
fn journal_filesystem_can_append(f: &str) {
    pdesc("its filesystem supports appending ?");
    match super::import::filesystem_can_append(&crate::read::rules::take_directory(f)) {
        Ok(true) => p(Y, ""),
        Ok(false) => p(N, "add/import won't work; use a reliable filesystem"),
        Err(e) => i(U, &format!("unknown; {e}")),
    }
}

/// The output of the first of these sets of arguments that the hledger in
/// PATH runs successfully, or the error output of the last
/// (tryHledgerArgs).
fn try_hledger_args(argss: &[&[&str]]) -> Result<String, String> {
    let mut last = Err("tryHledgerArgs: no arguments provided".to_string());
    for args in argss {
        last = match std::process::Command::new("hledger")
            .args(*args)
            .stdin(std::process::Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => {
                return Ok(String::from_utf8_lossy(&o.stdout).into_owned());
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => Err(e.to_string()),
        };
    }
    last
}

/// The current hledger release: the first number on the first line of
/// this page containing "current hledger release", fetched over HTTPS with
/// a 10 second timeout (getLatestHledgerVersionFromHledgerOrg).
fn latest_hledger_version(url: &str) -> Result<String, String> {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls_pure::provider()))
        .build();
    let agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| "(HTTP failure)".to_string())?;
    let body = response
        .body_mut()
        .read_to_vec()
        .map_err(|_| "(HTTP failure)".to_string())?;
    let text = String::from_utf8(body).map_err(|e| {
        let b = e.as_bytes()[e.utf8_error().valid_up_to()];
        format!(
            "Cannot decode byte '\\x{b:02x}': Data.Text.Internal.Encoding: Invalid UTF-8 stream"
        )
    })?;
    let line = text
        .lines()
        .find(|l| l.contains("current hledger release"))
        .unwrap_or("");
    let version: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if version.is_empty() {
        Err("couldn't parse version".to_string())
    } else {
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Serve one HTTP response on a local port, giving the page's URL.
    fn serve_once(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://{addr}/install.html")
    }

    #[test]
    fn latest_version_from_a_local_server() {
        let page = "<p>x</p>\n<p>The current stable hledger release is <strong>1.52.4</strong>. <!-- for setup command: \"current hledger release\" --></p>\n";
        assert_eq!(
            latest_hledger_version(&serve_once("200 OK", page)),
            Ok("1.52.4".to_string())
        );
        assert_eq!(
            latest_hledger_version(&serve_once("200 OK", "no version here\n")),
            Err("couldn't parse version".to_string())
        );
        assert_eq!(
            latest_hledger_version(&serve_once("404 Not Found", "")),
            Err("(HTTP failure)".to_string())
        );
    }

    #[test]
    fn versions() {
        assert_eq!(to_version("1.52.4"), Some(vec![1, 52, 4]));
        assert_eq!(to_version("1.x"), None);
        assert!(to_version("1.99").unwrap() >= to_version("1.52.4").unwrap());
    }
}
