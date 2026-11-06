//! The pinned hledger submodule supplies embedded documents (the quick reference card,
//! command help) and the functional test corpus, so a checkout without it cannot build.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // the compiler's version, which setup shows
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_default();
    println!("cargo::rustc-env=HLEDGERS_RUSTC_VERSION={version}");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !root.join("hledger/hledger/test").is_dir() {
        eprintln!("hledger/ submodule is missing: run `git submodule update --init`");
        std::process::exit(1);
    }
}
