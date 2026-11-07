//! A/B benchmark of hledgers against a Haskell hledger binary.
//!
//! cargo run -p hledgers-bench -- --setup         find the baseline, generate journals
//! cargo run -p hledgers-bench -- --run [--runs N]  time both binaries, write bench/results.md
//! cargo run -p hledgers-bench -- --generate NUMTXNS NUMACCTS ACCTDEPTH [OPTIONS]
//!                                               print a journal, as tools/generatejournal.hs does
//! cargo run -p hledgers-bench -- --household NUMTXNS  print a household journal (realistic.rs)
//!
//! The baseline is HLEDGERS_BENCH_BASELINE if set, else target/bench-cache/hledger,
//! else `hledger` on PATH (the README explains building the submodule's commit).
//! Journals live in target/bench-cache/. `--run` times target/release/hledgers,
//! so build that first with `cargo build --release`.

mod generate;
mod realistic;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The generated journals: file stem and generatejournal.hs arguments.
const JOURNALS: &[(&str, &str)] = &[
    ("10ktxns-1kaccts", "10000 1000 10"),
    ("100ktxns-1kaccts", "100000 1000 10"),
];

/// Household journals (realistic.rs) of these sizes, for the daily commands.
const HOUSEHOLD: &[usize] = &[0, 10, 100, 1000, 100_000];

/// The daily commands, timed on the household journals.
const DAILY: Commands = &[
    ("balance", true),
    ("register", true),
    ("is", true),
    ("bs", true),
    (
        "roi --inv assets:investments --pnl income:investments -V",
        true,
    ),
];

/// Upstream's own benchmark journal, used as it is.
const UPSTREAM_JOURNAL: &str = "hledger/hledger/bench/10000x1000x10.journal";

/// Commands to time, and whether to compare their output.
type Commands = &'static [(&'static str, bool)];

/// Timed commands. `stats` reports its own run time, so its output is not compared.
const COMMANDS: Commands = &[
    ("stats", false),
    ("balance", true),
    ("print", true),
    ("register", true),
    ("bal -M", true),
    ("reg -w 120", true),
    ("print -O json", true),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--setup") => setup(),
        Some("--run") => {
            let runs = match &args[1..] {
                [] => Ok(5),
                [flag, n] if flag == "--runs" => n.parse().map_err(|_| format!("bad run count {n}")),
                _ => Err("usage: --run [--runs N]".into()),
            };
            runs.and_then(run)
        }
        Some("--generate") => generate::Options::parse(&args[1..]).and_then(|o| {
            std::io::stdout()
                .write_all(generate::journal(&o).as_bytes())
                .map_err(|e| e.to_string())
        }),
        Some("--household") => match &args[1..] {
            [n] => n
                .parse()
                .map_err(|_| format!("not a number: {n}"))
                .and_then(|n| {
                    std::io::stdout()
                        .write_all(realistic::journal(n).as_bytes())
                        .map_err(|e| e.to_string())
                }),
            _ => Err("usage: --household NUMTXNS".into()),
        },
        _ => Err("usage: hledgers-bench --setup | --run [--runs N] | --generate NUMTXNS NUMACCTS ACCTDEPTH [--chinese|--mixed] [--start=YYYY-MM-DD] [--days=N] | --household NUMTXNS".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hledgers-bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn cache() -> PathBuf {
    workspace().join("target/bench-cache")
}

/// The Haskell hledger to compare against: HLEDGERS_BENCH_BASELINE, else
/// target/bench-cache/hledger, else `hledger` on PATH.
fn baseline() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("HLEDGERS_BENCH_BASELINE") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "HLEDGERS_BENCH_BASELINE={}: not a file",
                p.display()
            ))
        };
    }
    let cached = cache().join("hledger");
    if cached.is_file() {
        return Ok(cached);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(found) = std::env::split_paths(&path)
        .map(|dir| dir.join("hledger"))
        .find(|p| p.is_file())
    {
        return Ok(found);
    }
    Err(
        "no hledger binary found: put one on PATH (the README explains building the \
         submodule's commit with cabal), copy it to target/bench-cache/hledger, or set \
         HLEDGERS_BENCH_BASELINE=/path/to/hledger"
            .into(),
    )
}

fn baseline_version(bin: &Path) -> Result<String, String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn setup() -> Result<(), String> {
    let cache = cache();
    fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let baseline = baseline()?;
    println!(
        "baseline: {} ({})",
        baseline.display(),
        baseline_version(&baseline)?
    );
    for (stem, args) in JOURNALS {
        let args: Vec<String> = args.split_whitespace().map(String::from).collect();
        let path = cache.join(format!("{stem}.journal"));
        let text = generate::journal(&generate::Options::parse(&args)?);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!(
            "journal: {} (generatejournal {})",
            path.display(),
            args.join(" ")
        );
    }
    for &n in HOUSEHOLD {
        let path = cache.join(format!("{}.journal", household_stem(n)));
        fs::write(&path, realistic::journal(n)).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("journal: {} (household, {n} transactions)", path.display());
    }
    println!(
        "journal: {} (upstream's)",
        workspace().join(UPSTREAM_JOURNAL).display()
    );
    Ok(())
}

/// `0txns-household`, `100txns-household`, `1ktxns-household`, ...
fn household_stem(n: usize) -> String {
    if n >= 1000 && n.is_multiple_of(1000) {
        format!("{}ktxns-household", n / 1000)
    } else {
        format!("{n}txns-household")
    }
}

struct Measurement {
    median: Duration,
    peak_kb: u64,
}

fn run(runs: usize) -> Result<(), String> {
    let baseline = baseline()?;
    let version = baseline_version(&baseline)?;
    let hledgers = workspace().join("target/release/hledgers");
    if !hledgers.exists() {
        println!(
            "note: {} is missing (cargo build --release); its columns will be n/a",
            hledgers.display()
        );
    }
    // (name, path, commands, runs): the small household journals take
    // milliseconds, so they get more runs.
    let mut journals: Vec<(String, PathBuf, Commands, usize)> = HOUSEHOLD
        .iter()
        .map(|&n| {
            let stem = household_stem(n);
            let path = cache().join(format!("{stem}.journal"));
            (stem, path, DAILY, if n <= 1000 { runs * 20 } else { runs })
        })
        .collect();
    journals.extend(JOURNALS.iter().map(|(stem, _)| {
        let path = cache().join(format!("{stem}.journal"));
        (stem.to_string(), path, COMMANDS, runs)
    }));
    journals.push((
        "10000x1000x10 (upstream)".into(),
        workspace().join(UPSTREAM_JOURNAL),
        COMMANDS,
        runs,
    ));
    // An empty HOME and working directory, so no config file is read.
    let scratch = cache().join("run");
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

    let mut table = format!(
        "# hledgers vs {version}\n\n\
         Baseline: `{}`. Median wall time of {runs} runs ({} for household journals of up \
         to 1000 transactions), the two binaries' runs alternating; peak RSS (VmHWM, polled \
         every 2 ms); and whether stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.\n\n\
         | journal | command | baseline | hledgers | speedup | RSS baseline | RSS hledgers | same stdout |\n\
         |---|---|---:|---:|---:|---:|---:|---|\n",
        baseline.display(),
        runs * 20
    );
    for (name, journal, commands, runs) in &journals {
        for &(command, compare) in *commands {
            let args: Vec<&str> = command.split_whitespace().collect();
            let [a, b] = measure([&baseline, &hledgers], &args, journal, &scratch, *runs);
            let secs = |m: &Option<Measurement>| {
                m.as_ref()
                    .map_or("n/a".into(), |m| match m.median.as_secs_f64() {
                        s if s < 1.0 => format!("{:.1} ms", s * 1000.0),
                        s => format!("{s:.3} s"),
                    })
            };
            let mb = |m: &Option<Measurement>| {
                m.as_ref().map_or("n/a".into(), |m| {
                    format!("{:.0} MB", m.peak_kb as f64 / 1024.0)
                })
            };
            let speedup = match (&a, &b) {
                (Some(a), Some(b)) => {
                    format!("{:.1}x", a.median.as_secs_f64() / b.median.as_secs_f64())
                }
                _ => "n/a".into(),
            };
            let same = match (&a, &b) {
                _ if !compare => "-".to_string(),
                (Some(_), Some(_)) => {
                    let read = |s: &str| fs::read(scratch.join(format!("{s}.out"))).ok();
                    if read("a") == read("b") { "yes" } else { "no" }.to_string()
                }
                _ => "n/a".into(),
            };
            let row = format!(
                "| {name} | {command} | {} | {} | {speedup} | {} | {} | {same} |\n",
                secs(&a),
                secs(&b),
                mb(&a),
                mb(&b)
            );
            print!("{row}");
            table.push_str(&row);
        }
    }
    let results = Path::new(env!("CARGO_MANIFEST_DIR")).join("results.md");
    fs::write(&results, table).map_err(|e| format!("{}: {e}", results.display()))?;
    println!("wrote {}", results.display());
    Ok(())
}

/// Run each binary's `-f journal args` `runs` times, alternating which goes
/// first, as this machine's speed drifts; stdout to `scratch/a.out` and
/// `scratch/b.out`. None for a binary that is missing or fails.
fn measure(
    bins: [&Path; 2],
    args: &[&str],
    journal: &Path,
    scratch: &Path,
    runs: usize,
) -> [Option<Measurement>; 2] {
    let mut times: [Option<Vec<Duration>>; 2] = [Some(Vec::new()), Some(Vec::new())];
    let mut peak_kb = [0; 2];
    for run in 0..runs {
        let order = if run % 2 == 0 { [0, 1] } else { [1, 0] };
        for i in order {
            let label = ["a", "b"][i];
            let timed = times[i]
                .as_ref()
                .and_then(|_| run_once(bins[i], args, journal, scratch, label));
            match (timed, times[i].as_mut()) {
                (Some((t, kb)), Some(ts)) => {
                    ts.push(t);
                    peak_kb[i] = peak_kb[i].max(kb);
                }
                _ => times[i] = None,
            }
        }
    }
    [0, 1].map(|i| {
        let mut ts = times[i].take()?;
        ts.sort();
        Some(Measurement {
            median: ts[ts.len() / 2],
            peak_kb: peak_kb[i],
        })
    })
}

/// One run of `bin -f journal args`, stdout to `scratch/label.out`: its
/// wall time and peak resident size, or None if it fails.
fn run_once(
    bin: &Path,
    args: &[&str],
    journal: &Path,
    scratch: &Path,
    label: &str,
) -> Option<(Duration, u64)> {
    let stdout = File::create(scratch.join(format!("{label}.out"))).ok()?;
    let start = Instant::now();
    let mut child = Command::new(bin)
        .args(args)
        .arg("-f")
        .arg(journal)
        .current_dir(scratch)
        .env("HOME", scratch)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("LEDGER_FILE")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send((Instant::now(), status));
    });
    // The peak resident size is only readable while the process is alive,
    // so poll it until the waiter reports the exit.
    let mut peak_kb = 0;
    let (end, status) = loop {
        peak_kb = peak_kb.max(vm_hwm_kb(pid).unwrap_or(0));
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(done) => break done,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    };
    status.ok()?.success().then_some((end - start, peak_kb))
}

fn vm_hwm_kb(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
