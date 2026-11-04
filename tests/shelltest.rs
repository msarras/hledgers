//! Runs hledger's functional test corpus against hledgers. See harness/run.rs.

// The shared modules' unit tests compile here too (cargo sets cfg(test) even with
// `harness = false`) but only run in shelltest_selftest, whose build reports any
// code that is really unused.
#![allow(dead_code, unused_imports)]

mod harness;
mod sh;

fn main() -> std::process::ExitCode {
    harness::main()
}
