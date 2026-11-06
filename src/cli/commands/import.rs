//! The import command (Hledger.Cli.Commands.Import): read the new
//! transactions from data files (or the rules files in `rules/` beside the
//! journal), append them to the journal file, and remember how far each
//! file was imported in its `.latest` file.

use std::io::Write;

use super::print::{LAYOUT_FLAG, layout_from_raw_opts};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::not_yet;
use crate::cli::opts::CliOpts;
use crate::model::{Journal, Transaction};
use crate::read::latest::save_latest_dates_for_files;
use crate::read::rules::{path_join, take_directory};
use crate::read::{BalancingOpts, InputOpts};
use crate::write::journal::show_transaction;

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "import",
    aliases: &[],
    flags: &[
        Flag::new(
            &["get"],
            NONE,
            "",
            "fetch new data first by running the get command",
        )
        .short(&['g']),
        Flag::new(
            &["catchup"],
            NONE,
            "",
            "just mark all transactions as already imported",
        ),
        Flag::new(
            &["dry-run"],
            NONE,
            "",
            "just show the transactions to be imported",
        ),
        LAYOUT_FLAG,
    ],
    args: Some("[-f JOURNALFILE] [DATAFILES]"),
    doc: crate::command_doc!("Import"),
    ..Mode::DEFAULT
};

/// Run the import command (importcmd). Messages go to standard error; a
/// dry run shows the transactions on standard output.
pub fn import(opts: &CliOpts, j: Journal) -> Result<(), String> {
    let raw = &opts.raw;
    if raw.bool("get") {
        return Err(not_yet("import --get", "8"));
    }
    let journal_file = j
        .files
        .first()
        .map_or("(unknown)", |(f, _)| f.as_str())
        .to_string();
    let catchup = raw.bool("catchup");
    let dry_run = raw.bool("dry-run");
    // a bad --layout is an error only once a transaction is shown, as upstream's
    let layout = layout_from_raw_opts(opts);
    // amounts styled as -c says, or else as the journal's are
    let mut styles = opts
        .input
        .balancing
        .commodity_styles
        .clone()
        .unwrap_or_default();
    for (c, s) in j.commodity_styles() {
        styles.entry(c).or_insert(s);
    }
    let iopts = InputOpts {
        new: true,
        new_save: false,
        strict: false,
        importing: true,
        dry_run,
        balancing: BalancingOpts {
            commodity_styles: Some(styles),
            ..BalancingOpts::default()
        },
        journal_dir: Some(take_directory(&journal_file)),
        ..opts.input.clone()
    };
    let args: Vec<String> = raw.args().map(str::to_string).collect();
    let files = if args.is_empty() {
        discover_rules_files(&path_join(&take_directory(&journal_file), "rules"))
    } else {
        args
    };
    if files.is_empty() {
        return Err("please specify one or more data files to import from,\n\
                    or add .rules files in a rules/ directory next to the journal."
            .to_string());
    }
    let names: Vec<String> = files
        .iter()
        .map(|f| quote_if_needed(f.rsplit('/').next().unwrap_or(f)))
        .collect();
    let names = names.join(", ");
    let (newj, latest) = crate::read::read_journal_files_and_latest_dates(&iopts, &files)?;
    let mut new = newj.txns;
    new.sort_by_key(|t| t.date);
    let n = new.len();
    if n == 0 {
        eprintln!("no new transactions found in {names}");
        return Ok(());
    }
    if catchup {
        if dry_run {
            eprint!("would skip {n} new transactions (dry run)\n\n");
        } else {
            eprint!("marked {names} as caught up, skipping {n} transactions\n\n");
            save_latest_dates_for_files(&latest)?;
        }
        return Ok(());
    }
    if dry_run {
        eprint!("would import {n} new transactions from {names}:\n\n");
        let layout = layout.clone()?;
        let out: String = new
            .iter()
            .map(|t| show_transaction(t, layout, false))
            .collect();
        let mut stdout = std::io::stdout().lock();
        let _ = stdout
            .write_all(out.as_bytes())
            .and_then(|()| stdout.flush());
        if opts.input.strict {
            strict_checks(j, &new)?;
        }
        return Ok(());
    }
    if opts.input.strict {
        strict_checks(j, &new)?;
    }
    for t in &new {
        append_to_journal_file_or_stdout(&journal_file, || {
            Ok(show_transaction(t, layout.clone()?, false))
        })?;
    }
    eprintln!("imported {n} new transactions from {names} to {journal_file}");
    save_latest_dates_for_files(&latest)
}

/// The strict checks of the journal with the new transactions added, each
/// before the ones already there, as upstream adds them; a failure is shown
/// as `fail` shows it.
fn strict_checks(mut j: Journal, new: &[Transaction]) -> Result<(), String> {
    let old = std::mem::take(&mut j.txns);
    j.txns = new.iter().rev().cloned().chain(old).collect();
    crate::read::checks::journal_strict_checks(&j).map_err(|e| format!("user error ({e})"))
}

/// The `.rules` files in a directory, in order of name, except hidden ones
/// and those whose name starts with `_`; none if there is no such
/// directory (discoverRulesFiles).
fn discover_rules_files(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rules") && !n.starts_with('.') && !n.starts_with('_'))
        .collect();
    names.sort();
    names.into_iter().map(|n| path_join(dir, &n)).collect()
}

/// The text in double quotes, its double quotes backslash-escaped, if it
/// contains a quote, white space, `<` or `>` (quoteIfNeeded).
fn quote_if_needed(s: &str) -> String {
    if s.contains(['\'', '"', ' ', '\t', '\n', '\r', '<', '>']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Append a transaction's text to the journal file, after a blank line and
/// ending with exactly one newline; or with `-`, write it to standard
/// output (appendToJournalFileOrStdout). The file is created if need be,
/// and the filesystem is first checked to append safely.
pub fn append_to_journal_file_or_stdout(
    f: &str,
    text: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    let text = || text().map(|t| format!("\n{}\n", t.trim_end_matches('\n')));
    if f == "-" {
        let text = text()?;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush());
        return Ok(());
    }
    ensure_journal_file_exists(f)?;
    let dir = take_directory(f);
    if !filesystem_can_append(&dir)? {
        return Err(format!(
            "Cannot safely append to journal; aborting to avoid data loss.\n\
             The filesystem at {} does not honor O_APPEND.\n\
             This can happen eg with FAT/exFAT and Android shared-storage / FUSE mounts.\n\
             Please move the journal file to a reliable filesystem and try again.",
            crate::text::show_string(&dir)
        ));
    }
    let text = text()?;
    let io_error = |e: std::io::Error| format!("{f}: openFile: {}", crate::read::io_error_text(&e));
    std::fs::OpenOptions::new()
        .append(true)
        .open(f)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(io_error)
}

/// Create the journal file if it does not exist, with its directory, and a
/// comment saying so (ensureJournalFileExists).
fn ensure_journal_file_exists(f: &str) -> Result<(), String> {
    if crate::read::file_exists(f) {
        return Ok(());
    }
    eprintln!(
        "Creating hledger journal file {}",
        crate::text::show_string(f)
    );
    let dir = take_directory(f);
    let io_error = |p: &str, e: std::io::Error| format!("{p}: {}", crate::read::io_error_text(&e));
    if !dir.is_empty() && dir != "." {
        std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    }
    let content = format!(
        "; journal created {} by hledger\n",
        crate::dates::current_day()
    );
    std::fs::write(f, content).map_err(|e| io_error(f, e))
}

/// Whether appending to a file in this directory adds to its end, as it
/// does on most filesystems, tried with a temporary file
/// (ensureFilesystemCanAppend).
fn filesystem_can_append(dir: &str) -> Result<bool, String> {
    let path =
        std::path::Path::new(dir).join(format!(".hledger-append-test-{}", std::process::id()));
    let tried = (|| -> std::io::Result<bool> {
        std::fs::write(&path, "can this filesystem\n")?;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"append ?\n")?;
        Ok(std::fs::read(&path)? == b"can this filesystem\nappend ?\n")
    })();
    let _ = std::fs::remove_file(&path);
    tried.map_err(|e| {
        format!(
            "{}: openTempFile: {}",
            path.display(),
            crate::read::io_error_text(&e)
        )
    })
}
