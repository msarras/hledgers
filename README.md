# hledgers

hledgers is a Rust port of [hledger](https://hledger.org), the plain text
accounting tool: the command-line program and its library, with the same
commands, flags, output formats and error messages, checked against hledger's
own functional test corpus. It aims to be substantially faster. hledger-ui and
hledger-web are not part of it.

The reference is hledger 1.99.6 (development version), pinned as the `hledger/`
git submodule at commit fb09198fc. That tree supplies the test corpus, the
embedded documents and the benchmark journals; it is never built.

## Status

Milestone 0 of 10: the workspace, the test harness and the benchmark are in
place. `hledgers --version` prints `hledger 1.99.6 (hledgers 0.1.0)` and
`hledgers` with no command prints the quick reference card; every command
still exits with a "not implemented yet" error.

## Build

Requires Rust 1.97 or later.

    git clone --recursive URL hledgers    # or, in an existing clone: git submodule update --init
    cd hledgers
    cargo build --release                 # target/release/hledgers

The build stops with a hint if the submodule has not been checked out.

## Test

    cargo test

This runs the unit tests and then the functional tests: every case in
`hledger/hledger/test/**/*.test` (shelltestrunner format) plus this repo's
`tests/rules/*.test`, run against the hledgers binary, which the tests see as
`hledger`. No shell or external tools are used: the harness interprets the
small subset of sh that the test command lines use, and runs only `hledger` as
a separate process.

- `tests/shelltest-gate.txt` lists the test files (or `FILE:N-M` case ranges)
  that must pass; it grows with each milestone. Other cases are run and counted
  but do not fail the build.
- `tests/shelltest-unsupported.txt` lists the cases whose command lines use
  shell features the harness does not support, with the reason.
- `HLEDGERS_TEST_FILTER=GLOB` runs only matching test files, eg `'cli/*.test'`.
- `HLEDGERS_TEST_VERBOSE=1` shows the details of every failing case, not just
  gated ones.

## Benchmark

    cargo run -p hledgers-bench -- --setup              # once
    cargo build --release
    cargo run --release -p hledgers-bench -- --run [--runs N]

`--setup` finds the Haskell hledger to compare against (`HLEDGERS_BENCH_BASELINE`,
else `target/bench-cache/hledger`, else `hledger` on `PATH`) and generates the
10k- and 100k-transaction benchmark journals into `target/bench-cache/`.
`--run` times both programs on those journals and on
`hledger/hledger/bench/10000x1000x10.journal`, reporting the median wall time,
peak memory and whether their output is identical, in `bench/results.md`.

The best baseline is hledger built from the same commit as the submodule, so
the two programs are compared feature for feature. With GHC and cabal
installed (for example through ghcup), from a clone of the submodule's commit
with a `cabal.project` that lists only `hledger-lib` and `hledger`:

    cabal install exe:hledger --installdir=$HOME/.local/bin --install-method=copy

Linking needs `libgmp-dev`. The bench crate has no dependencies.

## `let` and `split` in CSV rules

CSV rules files will be able to derive several postings from one amount, with
named formulas (`let NAME EXPR`, read back as `%{NAME}`) and an exact
allocation function (`%{split(VALUE, WEIGHTS, I)}`). This lands with
milestone 7, together with its documentation here.

## Licence

GPL-3.0-or-later, the same as hledger, from which this is derived
(hledger is copyright Simon Michael and contributors). See `LICENSE`.
