//! The get command (Hledger.Cli.Commands.Get): new data fetched by the
//! journal's helper scripts, `data/getdata` for transactions and
//! `prices/getprices` for market prices, each skipped with a notice when
//! missing.

use std::collections::BTreeMap;

use crate::amount::Sym;
use crate::amount::currency::to_currency_code;
use crate::cli::args::{Arity, Flag, General, Mode};
use crate::cli::opts::CliOpts;
use crate::dates::Day;
use crate::model::{Journal, PriceDirective};
use crate::read::rules::{path_join, take_directory};

pub static MODE: Mode = Mode {
    name: "get",
    flags: &[
        Flag::new(
            &["transactions"],
            Arity::None,
            "",
            "fetch transactions only",
        ),
        Flag::new(&["prices"], Arity::None, "", "fetch prices only"),
        Flag::new(
            &["dry-run"],
            Arity::None,
            "",
            "just print the commands that would be run",
        ),
    ],
    general: General::Groups2,
    args: None,
    doc: crate::command_doc!("Get"),
    ..Mode::DEFAULT
};

/// Run the get command (getcmd): both phases, or with --transactions or
/// --prices, the ones selected.
pub fn get(opts: &CliOpts, j: &Journal) -> Result<(), String> {
    let (txns, prices) = (opts.raw.bool("transactions"), opts.raw.bool("prices"));
    if txns || !prices {
        fetch_data(opts, j)?;
    }
    if prices || !txns {
        fetch_prices(opts, j)?;
    }
    Ok(())
}

/// Show a helper's command line on standard error (logCommand).
fn log_command(dry_run: bool, cmdline: &str) {
    let what = if dry_run { "would run" } else { "running" };
    eprintln!("{what}: {cmdline}");
}

/// The journal's directory.
fn journal_dir(j: &Journal) -> String {
    take_directory(j.files.first().map_or("(unknown)", |(f, _)| f))
}

fn create_dir(d: &str) -> Result<(), String> {
    std::fs::create_dir_all(d).map_err(|e| format!("{d}: {}", crate::read::io_error_text(&e)))
}

/// Run a helper script with these arguments, in this directory if given,
/// with empty input: its exit code, standard output and standard error,
/// or the error starting it as GHC shows it (readCreateProcessWithExitCode).
fn run_script(
    script: &str,
    args: &[String],
    dir: Option<&str>,
) -> Result<(i32, String, String), String> {
    let mut cmd = std::process::Command::new(script);
    cmd.args(args).stdin(std::process::Stdio::null());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().map_err(|e| {
        format!(
            "{script}: readCreateProcessWithExitCode: posix_spawnp: {}",
            crate::read::io_error_text(&e)
        )
    })?;
    Ok((
        crate::cli::exit_code(out.status),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Fetch transactions (fetchData): run `data/getdata` in the journal's data
/// directory, made if need be, its output going to standard error.
fn fetch_data(opts: &CliOpts, j: &Journal) -> Result<(), String> {
    let dry_run = opts.raw.bool("dry-run");
    let data_dir = path_join(&journal_dir(j), "data");
    let script = path_join(&data_dir, "getdata");
    create_dir(&data_dir)?;
    if !crate::read::file_exists(&script) {
        eprintln!("no {script} script, skipping data fetch");
        return Ok(());
    }
    log_command(dry_run, &format!("cd {data_dir} && getdata"));
    if dry_run {
        return Ok(());
    }
    let warn = |m: &str| crate::console::warn(&opts.input.prog_args, m);
    match run_script(&script, &[], Some(&data_dir)) {
        Err(e) => warn(&format!("getdata failed: {e}")),
        Ok((code, out, err)) => {
            eprint!("{out}{err}");
            if code != 0 {
                warn(&format!("getdata exited {code}"));
            }
        }
    }
    Ok(())
}

/// Fetch market prices (fetchPrices): for each commodity used, other than
/// the base currency, run `prices/getprices BASE CODE START TODAY` from the
/// commodity's first posting date, merging what it prints into
/// `prices/CODE.prices`.
fn fetch_prices(opts: &CliOpts, j: &Journal) -> Result<(), String> {
    let dry_run = opts.raw.bool("dry-run");
    let jdir = journal_dir(j);
    let prices_dir = path_join(&jdir, "prices");
    let script = path_join(&prices_dir, "getprices");
    create_dir(&prices_dir)?;
    if !crate::read::file_exists(&script) {
        eprintln!("no {script} script, skipping prices fetch");
        return Ok(());
    }
    let today = crate::dates::current_day();
    let base = j.base_currency_code().to_string();
    // each currency code's first posting date (commodityDateSpansByCode)
    let mut starts: BTreeMap<String, Day> = BTreeMap::new();
    for p in j.postings() {
        for a in p.p.amount.amounts_raw() {
            let d = starts
                .entry(to_currency_code(&a.commodity).to_string())
                .or_insert(p.date());
            *d = (*d).min(p.date());
        }
    }
    let mut codes: Vec<String> = Vec::new();
    for c in j.commodities_used() {
        let code = to_currency_code(&c).to_string();
        if code != base && !codes.contains(&code) {
            codes.push(code);
        }
    }
    let warn = |m: &str| crate::console::warn(&opts.input.prog_args, m);
    for code in codes {
        let Some(start) = starts.get(&code) else {
            continue;
        };
        let outfile = path_join(&prices_dir, &format!("{code}.prices"));
        let args = vec![
            base.clone(),
            code.clone(),
            start.to_string(),
            today.to_string(),
        ];
        log_command(dry_run, &format!("getprices {} >{outfile}", args.join(" ")));
        if dry_run {
            continue;
        }
        let out = match run_script(&script, &args, None) {
            Err(e) => {
                warn(&format!("getprices failed for {code}: {e}"));
                continue;
            }
            Ok((code_, out, err)) => {
                eprint!("{err}");
                if code_ != 0 {
                    warn(&format!("getprices exited {code_} for {code}"));
                    continue;
                }
                out
            }
        };
        if !out.is_empty() {
            eprintln!("{}", merge_and_write_prices(&outfile, &out)?);
        }
    }
    Ok(())
}

/// The P directives in journal text (parsePrices).
fn parse_prices(text: &str) -> Result<Vec<PriceDirective>, String> {
    let iopts = crate::read::InputOpts::default();
    let (j, st) = crate::read::journal::parse_journal(&iopts, "-", text)?;
    let j = crate::read::finalise::journal_finalise(&iopts, "-", text, j, st)?;
    Ok(j.price_directives)
}

/// What two price directives are the same price for (priceKey).
fn price_key(pd: &PriceDirective) -> (Day, Sym, Sym) {
    (pd.date, pd.commodity.clone(), pd.amount.commodity.clone())
}

/// Merge new prices into a prices file, made if need be: the file's prices
/// win, new ones are added, all sorted; a file holding anything but P
/// directives is not touched. The outcome as reported (mergeAndWritePrices,
/// reportOutcome), or an error writing the file.
fn merge_and_write_prices(outfile: &str, new: &str) -> Result<String, String> {
    let existing = if crate::read::file_exists(outfile) {
        let text = std::fs::read_to_string(outfile).unwrap_or_default();
        let pure = text.lines().all(|l| {
            let l = crate::text::strip(l);
            l.is_empty() || l.starts_with('P')
        });
        if !pure {
            return Ok(format!(
                "Refusing to overwrite {outfile}: it contains content other than P directives, which would be lost."
            ));
        }
        match parse_prices(&text) {
            Ok(ps) => ps,
            Err(e) => return Ok(format!("{outfile}: could not read existing prices: {e}")),
        }
    } else {
        Vec::new()
    };
    let new = match parse_prices(new) {
        Ok(ps) => ps,
        Err(e) => {
            return Ok(format!(
                "{outfile}: could not read existing prices: could not parse new prices: {e}"
            ));
        }
    };
    let keys: std::collections::BTreeSet<_> = existing.iter().map(price_key).collect();
    let added: Vec<PriceDirective> = new
        .into_iter()
        .filter(|p| !keys.contains(&price_key(p)))
        .collect();
    if added.is_empty() {
        return Ok(format!("{outfile}: no new prices"));
    }
    let (n_existing, n_added) = (existing.len(), added.len());
    let mut merged = existing;
    merged.extend(added);
    merged.sort_by_key(price_key);
    let text: String = (merged.iter())
        .map(|pd| crate::model::render::show_price_directive(pd) + "\n")
        .collect();
    create_dir(&take_directory(outfile))?;
    std::fs::write(outfile, text)
        .map_err(|e| format!("{outfile}: withFile: {}", crate::read::io_error_text(&e)))?;
    Ok(format!(
        "wrote {outfile} (existing {n_existing}, added {n_added})"
    ))
}
