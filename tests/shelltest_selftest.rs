//! Unit tests of the functional-test harness (the test file parser and the
//! sh-subset interpreter), which live in the modules shared with tests/shelltest.rs.

mod harness;
mod sh;

#[test]
fn corpus_runner_entry_point() {
    // Referencing it also stops the runner, used only by the shelltest target,
    // from counting as dead code in this one.
    let _: fn() -> std::process::ExitCode = harness::main;
}
