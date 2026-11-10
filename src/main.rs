#![forbid(unsafe_code)]

fn main() {
    std::process::exit(hledgers::cli::run(std::env::args_os()));
}
