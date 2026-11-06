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

Milestone 9 of 10: the journal reader and writer, valuation, cost
conversion, lot tracking, the listing commands, the balance reports, the
register reports and holdings, with all their output formats, the CSV,
timeclock and timedot readers, the import command, and the command-line
interface (help, config files, command aliases, add-ons, translations,
`run` and `repl`, `add`, `get`, `setup`) are complete. `hledgers` reads journal files with every directive,
include globs, amount expressions, costs, balance assertions and
assignments, auto postings and periodic transactions; it balances and
checks entries as hledger 1.99 does and prints the same error messages; it
converts amounts to cost and values them with declared, inferred and
commodity alias prices (`-B`, `-V`, `-X`, `--value`,
`--infer-market-prices`), and pivots account names (`--pivot`). It tracks
investment lots: cost bases written as `{...}` annotations or lot
subaccounts, or inferred from costs; acquisitions,
disposals (FIFO, LIFO, HIFO, average cost, specific lots, and their
all-accounts variants, set by `lots:` tags), transfers with their fees,
short positions, and lots carried through closing balances; it adds lot
subaccounts and realised gain postings, and checks them (`--lots` shows the
lot detail, `--ignore-lots` skips it). `print`, `check`, `files`, `prices`,
`rewrite`, `accounts`, `payees`, `codes`, `notes`, `descriptions`, `tags`,
`commodities`, `stats`, `transactions`, `balance`, `balancesheet`,
`balancesheetequity`, `incomestatement`, `cashflow`, `register`,
`aregister`, `activity`, `close`, `diff`, `roi`, `holdings`, `run`,
`repl`, `add`, `get` and `setup` work, with
the general input, report and query options; long flags may be abbreviated
to a unique prefix. `balance` shows single-column lists and trees (with
`--format` line formats) and multi-period tables: changes, cumulative or
historical balances, value changes, gains and counts, with totals,
averages, percentages, sorting, `--invert`, `--transpose`, the wide, tall,
bare, barewide and tidy layouts, plain or `--pretty`; with `--budget`, it
compares the actual changes with the goals of the periodic transactions.
The four compound commands show their sections (assets and liabilities,
revenues and expenses, ...) normally positive, each with its totals, and
the net totals. `register` lists postings with their running total or
average (`--historical`, `--cumulative`, `--average`), or with a report
interval, summary postings per account and period, related postings,
sorted, depth-clipped, laid out to `--width`; `aregister` lists an
account's transactions with its running balance; `activity` charts the
number of transactions or postings per period; `close` writes closing,
opening, retain earnings, assertion and assignment transactions, with
`--lots` per lot; `diff` compares an account's transactions in two files;
`roi` calculates an investment's internal and time-weighted rates of
return; `holdings` shows the lots held, per account or lot, with their
cost, value, weight, unrealised and realised gains and rate of return, in
a list or tree, or their totals at each period's end. `print`, `balance`,
the compound commands, `register`, `aregister` and `holdings` write txt,
csv, tsv, json, html and fods (`-O`, or `-o FILE` by its extension), html
and fods with links to hledger-web's register (`--base-url`); `print`
also writes Beancount, Ledger and SQL (`-O beancount|ledger|sql`; with
`--export`, the whole journal, Beancount's with its options and open,
commodity and price directives); `balance`,
the compound commands, `register`, `aregister`, `activity` and `holdings`
draw charts (`--chart[=KIND,SCALE]`, `-O svg`) as text, svg, html, or
their data as csv, tsv and json. It reads CSV, SSV and TSV files (by
extension, `csv:`/`ssv:`/`tsv:` prefix, or from standard input with
`--rules-file`) converted by their rules files, with every rule: fields
lists, field assignments with templates, `%{...}` expressions and their
functions, if blocks and ifs tables with field and record matchers, skip,
end and merge rules, date formats and time zones, decimal marks,
currencies, amounts and amount expressions, balance assertions, postings 1
to 99, separators, encodings, and included rules files; rules files read
as data through their source rule (a file pattern, a data cleaning or
generating command); and CSV and rules files included by journals. CSV
rules also have this repo's `let` and `split` (below). It reads timeclock
files (clock-in and clock-out sessions, split at midnight, open ones
ending now; `--old-timeclock`) and timedot files (dots, numbers with
units, letters), directly or included. `import` appends the new
transactions of data files, or of the rules files in `rules/`, to the
journal, remembering how far each was read in `.latest` files (as `print
--new` does), with `--dry-run`, `--catchup`, `--strict`, `--get` and the
`archive` rule. `--obfuscate` hides descriptions, codes and account names
behind hashes, as hledger's does.

The command line is processed as hledger's is: flags before the command
are moved after it, commands are found by name, alias or unique prefix,
`@FILE` arguments are expanded, and errors read as cmdargs' do. Each
command's `-h`/`--help` and `hledger -h` show the same help, laid out as
cmdargs lays it out, with the program's name as invoked. The `help`
command shows the quick reference, the commands list (with installed
add-ons and config file aliases), a command's usage, the embedded hledger,
hledger-ui and hledger-web manuals at a topic (in info, man, a pager or as
text), topic lists, and tldr examples; `--info`, `--man`, `--webman` and
`--examples` work too. Config files (`hledger.conf` in the current
directory or above, `~/.hledger.conf`, the XDG one, or `--conf`; `-n` for
none) add general and per-command arguments and command aliases, shell
aliases only from a trusted file. `hledger-*` executables in PATH run as
add-on commands. `--lang` (or `auto`, following the locale) translates
report titles, headings, month names and chart labels with hledger's
eleven built-in catalogs or the user's own, and `--write-lang` starts a
translation. Today's date and CSV time zones use the local time zone;
register and text charts use the terminal's width; long help and reports
go through a pager on a terminal; `--view` opens the output in a viewer;
`--test` runs the 131 unit tests and doctests ported from hledger's
(whose own `--test` runs 326). `run` runs commands one after
another, from its command line, files or standard input, reading each
journal once, and can collect their outputs into one html page; `repl`
does so at a prompt (with line editing on a terminal), reloading changed
files and aliases. `add` asks for transactions, with defaults from the
command line or a similar past entry, completion on a terminal, and
checks against the whole journal, and appends them. `get` runs the
journal's `data/getdata` and `prices/getprices` helpers, merging the
prices into per-commodity files. `setup` checks the installation, the
terminal and the default journal, contacting hledger.org over HTTPS (pure
Rust TLS: rustls with a RustCrypto provider, TLS 1.3 only). Of hledger's
2420 functional test cases 2416 pass byte-identically, all of them gated,
with this repo's 27. The other four (`journal/fastpath.test` 6, 9, 11 and
13) check the statistics line of hledger's fast-path journal parser, which
hledgers does not have by design; they are listed as such and not run.

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
  shell features the harness does not support, with the reason, and the cases
  that can not pass by design (reason `by-design: ...`), which are not run and
  are counted apart.
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

hledgers extends CSV rules with named values and an exact split, so that one
amount in a bank's CSV can become several postings without editing the CSV.
Rules files that use neither behave exactly as hledger's.

**`let NAME VALUE`** (or `let NAME: VALUE`) names a value, which is a field
template like any other: `let principal %payment - %interest`. A let can be
assigned wherever a field can: at top level, indented in an `if` block (where
it overrides earlier assignments for the records the block matches, and its
`\1` refers to the block's match groups), and in an `ifs` table, as a
`let:NAME` column. The last assignment in effect wins, as for fields.
`%{NAME}` gives a let's value, and so does a bare `NAME` in a template
function's arguments; a let's value can use other lets and hledger fields
(`let memo paid at %{description}`), and its own name gives its previous
value. A name is a lowercase letter, then lowercase letters, digits and
underscores, and may not be a hledger field's name (`amount1`, `skip`...) or a
template function's (`split`...). A let can be used before it is declared. A
value that is an amount expression is substituted in parentheses, so with
`let sum %a + %b`, `amount1 %{sum} * 2` doubles the sum.

**`%{split(VALUE, WEIGHTS, I)}`** gives part `I` (from 1) of the amount
`VALUE` split in proportion to `WEIGHTS`: a number of equal parts (`3`), or
positive numbers separated by colons (`"30:70"`, `"0.3:0.7"`). `VALUE` is
read as a CSV amount is, with the `decimal-mark` rule and the `currency`
rule's commodity: an amount expression if it has an operator, otherwise with
its sign made canonical. The split is exact: the amount, rounded to the
decimal places it is written with, is shared out in whole units of its last
place; each part is rounded down, and the units left over go one each to the
parts that lost most in rounding, the later part first among equal losses.
So the parts always add up to the amount: 100.00 split in 3 is 33.33, 33.33
and 33.34, 333.33 split "30:70" is 100.00 and 233.33. A negative amount's
parts are negative. A part is written with the amount's commodity, without
digit group marks, with the same decimal places, so reading it as an amount
again changes nothing. Bad weights, a part number out of range, and a first
argument that is not an amount are errors: when the rules are read if they
are written there, otherwise for the record.

A mortgage payment, of which the bank's CSV gives the interest, shared
between two owners, the principal equally and the interest 30:70:

    date,description,payment,interest
    2024-01-01,mortgage,1000.00,333.33

    skip 1
    fields date, description, payment, interest
    let interest %interest
    let principal %payment - %interest
    account1 assets:checking
    amount1 -%payment
    account2 liabilities:mortgage:alice
    amount2 %{split(principal, 2, 1)}
    account3 liabilities:mortgage:bob
    amount3 %{split(principal, 2, 2)}
    account4 expenses:interest:alice
    amount4 %{split(interest, "30:70", 1)}
    account5 expenses:interest:bob
    amount5 %{split(interest, "30:70", 2)}

    $ hledgers -f mortgage.csv print
    2024-01-01 mortgage
        assets:checking                            -1000.00
        liabilities:mortgage:alice                   333.33
        liabilities:mortgage:bob                     333.34
        expenses:interest:alice                      100.00
        expenses:interest:bob                        233.33

`tests/rules/let-split.test` has more examples. Limits: a split part has no
cost (an `@` cost of `VALUE` is dropped); weights use `.` as their decimal
mark; and since `split` is a template function, hledger's error for an
unknown function, which lists them all, lists it too.

## Licence

GPL-3.0-or-later, the same as hledger, from which this is derived
(hledger is copyright Simon Michael and contributors). See `LICENSE`.
