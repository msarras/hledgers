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

Milestone 6 of 10, first part: the journal reader and writer, valuation,
cost conversion, the listing commands, the balance reports and the
register reports, with all their output formats, are complete. `hledgers`
reads journal files with every directive, include globs, amount
expressions, costs, balance assertions and assignments, auto postings and
periodic transactions; it balances and checks entries as hledger 1.99 does
and prints the same error messages; it converts amounts to cost and values
them with declared, inferred and commodity alias prices (`-B`, `-V`, `-X`,
`--value`, `--infer-market-prices`), and pivots account names (`--pivot`).
`print`, `check`, `files`, `prices`, `rewrite`, `accounts`, `payees`,
`codes`, `notes`, `descriptions`, `tags`, `commodities`, `stats`,
`transactions`, `balance`, `balancesheet`, `balancesheetequity`,
`incomestatement`, `cashflow`, `register`, `aregister`, `activity`, `close`,
`diff` and `roi` work, with the general input, report and query options;
long flags may be abbreviated to a unique prefix. `balance` shows
single-column lists and trees (with `--format` line formats) and
multi-period tables: changes, cumulative or historical balances, value
changes, gains and counts, with totals, averages, percentages, sorting,
`--invert`, `--transpose`, the wide, tall, bare, barewide and tidy layouts,
plain or `--pretty`; with `--budget`, it compares the actual changes with
the goals of the periodic transactions. The four compound commands show
their sections (assets and liabilities, revenues and expenses, ...)
normally positive, each with its totals, and the net totals. `register`
lists postings with their running total or average (`--historical`,
`--cumulative`, `--average`), or with a report interval, summary postings
per account and period, related postings, sorted, depth-clipped, laid out
to `--width`; `aregister` lists an account's transactions with its running
balance; `activity` charts the number of transactions or postings per
period; `close` writes closing, opening, retain earnings, assertion and
assignment transactions; `diff` compares an account's transactions in two
files; `roi` calculates an investment's internal and time-weighted rates
of return. `print`, `balance`, the compound commands, `register` and
`aregister` write txt, csv, tsv, json, html and fods (`-O`, or `-o FILE` by
its extension), html and fods with links to hledger-web's register
(`--base-url`); `balance`, the compound commands, `register`, `aregister`
and `activity` draw charts (`--chart[=KIND,SCALE]`, `-O svg`) as text,
svg, html, or their data as csv, tsv and json. Of hledger's 2420
functional test cases 1568 pass byte-identically, all of them gated.
Lots and holdings, translations, CSV, timeclock and timedot, and the
remaining commands are the later milestones. Register widths come from
`--width` or are 80 columns: the terminal's width is not yet queried.

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
