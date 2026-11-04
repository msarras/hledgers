//! A/B benchmark of hledgers against the hledger 1.52.4 release binary.
//!
//! cargo run -p hledgers-bench -- --setup         download the baseline, generate journals
//! cargo run -p hledgers-bench -- --run [--runs N]  time both binaries, write bench/results.md
//! cargo run -p hledgers-bench -- --generate NUMTXNS NUMACCTS ACCTDEPTH [OPTIONS]
//!                                               print a journal, as tools/generatejournal.hs does
//!
//! Everything lives in target/bench-cache/. `--run` times target/release/hledgers,
//! so build that first with `cargo build --release`.

mod generate;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BASELINE_URL: &str =
    "https://github.com/simonmichael/hledger/releases/download/1.52.4/hledger-linux-x64.tar.gz";
const BASELINE_VERSION: &str = "hledger 1.52.4";

/// The generated journals: file stem and generatejournal.hs arguments.
const JOURNALS: &[(&str, &str)] = &[
    ("10ktxns-1kaccts", "10000 1000 10"),
    ("100ktxns-1kaccts", "100000 1000 10"),
];

/// Upstream's own benchmark journal, used as it is.
const UPSTREAM_JOURNAL: &str = "hledger/hledger/bench/10000x1000x10.journal";

/// Timed commands. `stats` reports its own run time, so its output is not compared.
const COMMANDS: &[(&str, bool)] = &[
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
        _ => Err("usage: hledgers-bench --setup | --run [--runs N] | --generate NUMTXNS NUMACCTS ACCTDEPTH [--chinese|--mixed] [--start=YYYY-MM-DD] [--days=N]".into()),
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

fn baseline() -> PathBuf {
    cache().join("hledger-1.52.4")
}

fn setup() -> Result<(), String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err("the baseline binary is only published for x86_64 Linux".into());
    }
    let cache = cache();
    fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let baseline = baseline();
    if !baseline.exists() {
        let tarball = cache.join("hledger-linux-x64.tar.gz");
        if !tarball.exists() {
            download(BASELINE_URL, &tarball)?;
        }
        extract_hledger(&tarball, &baseline)?;
    }
    let version = Command::new(&baseline)
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", baseline.display()))?;
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    println!("baseline: {} ({version})", baseline.display());
    if !version.starts_with(BASELINE_VERSION) {
        return Err(format!("expected {BASELINE_VERSION}, got {version:?}"));
    }
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
    println!(
        "journal: {} (upstream's)",
        workspace().join(UPSTREAM_JOURNAL).display()
    );
    Ok(())
}

/// Download to a temporary name, renamed when complete.
fn download(url: &str, to: &Path) -> Result<(), String> {
    println!("downloading {url}");
    let response = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let expected: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let partial = to.with_extension("part");
    let mut file = File::create(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let size = std::io::copy(&mut response.into_body().into_reader(), &mut file)
        .map_err(|e| format!("{url}: {e}"))?;
    if expected.is_some_and(|n| n != size) {
        return Err(format!(
            "{url}: got {size} bytes, expected {}",
            expected.unwrap()
        ));
    }
    fs::rename(&partial, to).map_err(|e| format!("{}: {e}", to.display()))?;
    println!("downloaded {size} bytes to {}", to.display());
    Ok(())
}

fn extract_hledger(tarball: &Path, to: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", tarball.display());
    let gz = flate2::read::GzDecoder::new(File::open(tarball).map_err(err)?);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().map_err(err)? {
        let mut entry = entry.map_err(err)?;
        if entry.path().map_err(err)?.as_os_str() == "hledger" {
            let partial = to.with_extension("part");
            entry.unpack(&partial).map_err(err)?;
            return fs::rename(&partial, to).map_err(|e| format!("{}: {e}", to.display()));
        }
    }
    Err(format!(
        "{}: no hledger executable in the archive",
        tarball.display()
    ))
}

struct Measurement {
    median: Duration,
    peak_kb: u64,
}

fn run(runs: usize) -> Result<(), String> {
    let baseline = baseline();
    if !baseline.exists() {
        return Err("run --setup first".into());
    }
    let hledgers = workspace().join("target/release/hledgers");
    if !hledgers.exists() {
        println!(
            "note: {} is missing (cargo build --release); its columns will be n/a",
            hledgers.display()
        );
    }
    let mut journals: Vec<(String, PathBuf)> = JOURNALS
        .iter()
        .map(|(stem, _)| (stem.to_string(), cache().join(format!("{stem}.journal"))))
        .collect();
    journals.push((
        "10000x1000x10 (upstream)".into(),
        workspace().join(UPSTREAM_JOURNAL),
    ));
    // An empty HOME and working directory, so no config file is read.
    let scratch = cache().join("run");
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

    let mut table = format!(
        "# hledgers vs hledger 1.52.4\n\n\
         Median wall time of {runs} runs, peak RSS (VmHWM, polled every 2 ms), and whether \
         stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.\n\n\
         | journal | command | 1.52.4 | hledgers | speedup | RSS 1.52.4 | RSS hledgers | same stdout |\n\
         |---|---|---:|---:|---:|---:|---:|---|\n"
    );
    for (name, journal) in &journals {
        for &(command, compare) in COMMANDS {
            let args: Vec<&str> = command.split_whitespace().collect();
            let a = measure(&baseline, &args, journal, &scratch, "a", runs);
            let b = measure(&hledgers, &args, journal, &scratch, "b", runs);
            let secs = |m: &Option<Measurement>| {
                m.as_ref()
                    .map_or("n/a".into(), |m| format!("{:.3} s", m.median.as_secs_f64()))
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

/// Run `bin -f journal args` `runs` times, stdout to `scratch/label.out`.
/// None if the binary is missing or any run fails.
fn measure(
    bin: &Path,
    args: &[&str],
    journal: &Path,
    scratch: &Path,
    label: &str,
    runs: usize,
) -> Option<Measurement> {
    let mut times = Vec::new();
    let mut peak_kb = 0;
    for _ in 0..runs {
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
        let (end, status) = loop {
            peak_kb = peak_kb.max(vm_hwm_kb(pid).unwrap_or(0));
            match rx.recv_timeout(Duration::from_millis(2)) {
                Ok(done) => break done,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        };
        if !status.ok()?.success() {
            return None;
        }
        times.push(end - start);
    }
    times.sort();
    Some(Measurement {
        median: times[times.len() / 2],
        peak_kb,
    })
}

fn vm_hwm_kb(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
