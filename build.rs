//! The pinned hledger submodule supplies embedded documents (the quick reference card,
//! command help) and the functional test corpus, so a checkout without it cannot build.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !root.join("hledger/hledger/test").is_dir() {
        eprintln!("hledger/ submodule is missing: run `git submodule update --init`");
        std::process::exit(1);
    }
}
