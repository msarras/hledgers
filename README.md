# hledgers

hledgers is a Rust port of [hledger](https://hledger.org) (without the UI/web
components), the plain text accounting tool: the command-line program and its
library, with the same commands, flags, output formats and error messages,
checked against hledger's own functional test corpus. It aims to be
substantially faster with extended import parsing functionality. As such, the
philosophy behind hledgers deviates from hledger in a subtle but important way,
informed by my own experiences using ledger-cli originally and then moving to
hledger years later for its powerful reporting functionality.

The problem is that I've always struggled with the strict parsing framework of
hledger's import into journal entries because it meant that I had to lean on
automated transactions for any arithmetic I wanted to apply. The hard data I saw
in my journal wasn't the exact representation of my accounts. It had to be
ingested with `--auto` which meant I had two distinct states depending on the
CLI invocation. Maybe that's crucial to some, but for me it's an increased
cognitive load. It also meant that identifying discrepancies was difficult for
me because I also had to keep track of automated transaction rules and maintain
a growing list of exceptions, all of which are specified in a journal file
separate from a `.rules` file. I guess I could have maintained auxiliary scripts
for parsing but then I would need to just that: maintain them.  Why not just
build on the already simple and powerful framework of `import` to do everything
I need? hledger also starts getting slow for journals spanning multiple years
which meant that I also had to reconcile automated transactions at year-end and
start a fresh journal for the new year to keep things relatively fast. It meant
that I'd have to go digging through readmes to remember how to do it, and also
tinker with it to make sure everything checked out. All of these friction points
pushed me to flesh out an hledger derivative that aligns with my own way of
thinking. Namely:

1. The data I see in my plain text journal file should be complete and explicit
   so I'm never surprised, which means
2. parsing structured files like CSV and inserting their lines as journal
   entries should be complete and self-contained, which means
3. all of my parsing and arithmetic rules are together in their own
   separate file and one `import` takes care of it all, which also means
4. I don't have to spend time reconciling transactions and their automated
   arithmetic at year-end just to start a new journal file to keep runtimes low
   and,
5. I wouldn't have to maintain any auxiliary scripts or perform manual
   processing of structured files prior to parsing, and finally,
6. it should all be essentially instantaneous to run my personal finance
   reports regardless of how long or short the period is (monthly or 15+ years)

I know some people swear by automated transactions and may be perfectly happy
with the way hledger currently works, that's fine. hledgers still does all of
those things exactly the same way.

The reference is hledger 1.99.6 (development version), pinned as the `hledger/`
git submodule at commit fb09198fc. That tree supplies the test corpus, the
embedded documents and the benchmark journals; it is never built.

## Status

The port is complete: the journal reader and writer, valuation, cost
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
rules also have this repo's `let`, `split` and `pair` (below). It reads timeclock
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
hledgers is several times faster than hledger on journals of every size
(see Benchmark).

Known differences, on inputs hledger's tests do not cover: the `encoding`
rule refuses the DOS code pages (cp437, cp737, cp775, cp850, cp852, cp855,
cp857, cp860 to cp865 and cp869) and JIS X 0201 and 0208, which no pure-Rust
decoder provides, with an error saying so; `--debug` accepts its levels but
prints few of hledger's debug messages and none of its value dumps; a
regular expression whose alternatives overlap (`a|ab`) takes the
leftmost-first match where hledger takes the longest; and colour-only
details (the heading gradient of `help`, charts coloured for the terminal's
background) are not reproduced. `--version` prints hledger's version and
then hledgers' own.

## Download

Each release on the repository's Releases page carries archives built by
`.github/workflows/release.yml`, named `hledgers-vX.Y.Z-TARGET.tar.gz`
(`.zip` on Windows), for Linux x86_64 and arm64 (glibc 2.28 or later: Debian
10, Ubuntu 18.10, RHEL 8 or newer), macOS on Apple silicon and Intel, and
Windows x86_64, plus a `SHA256SUMS` file. Unpack the archive and put
`hledgers` (`hledgers.exe` on Windows) on your PATH.

On Windows, shell command lines (`source` rules, `!` in `run` and `repl`)
run through `cmd /C`, the home directory is `%USERPROFILE%` when `HOME` is
unset, and colour needs a terminal that understands ANSI escapes, such as
Windows Terminal.

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
  that must pass: every case that passes, so that none can silently fail
  again. Other cases are run and counted but do not fail the build.
- `tests/shelltest-unsupported.txt` lists the cases the harness does not run,
  with the reason: a command line outside its sh subset, or (reason
  `by-design: ...`) a case that can not pass by design, counted apart. At
  present it holds only the four by-design cases.
- `HLEDGERS_TEST_FILTER=GLOB` runs only matching test files, eg `'cli/*.test'`.
- `HLEDGERS_TEST_VERBOSE=1` lists every passing case and shows the details of
  every failing case, not just gated ones.
- `HLEDGERS_TEST_BIN=PATH` runs the corpus against another binary, such as a
  hledger built from the submodule's commit, to check the harness itself.

## Release

Set `version` in Cargo.toml and run `cargo build`, which records it in
Cargo.lock: the release build is `--locked` and stops if the two disagree.
Commit both files, then tag the commit and push the tag:

    git tag v0.2.0
    git push origin v0.2.0

The release workflow runs the CI checks, builds the binary for each
platform, runs it on `hledger/examples/sample.journal`, and creates the
GitHub release with the archives, `SHA256SUMS` and generated release notes.
Started by hand from the Actions tab (Run workflow), it does all of that but
publishes nothing, which checks the platforms before a release. The CI
checks are `cargo fmt --check`, `cargo clippy` and `cargo test` on Linux;
the `ci` workflow also runs them on every branch push and pull request. The Linux
arm64 build uses GitHub's `ubuntu-24.04-arm` runner, which is free for
public repositories only.

## Benchmark

    cargo run -p hledgers-bench -- --setup              # once
    cargo build --release
    cargo run --release -p hledgers-bench -- --run [--runs N]

`--setup` finds the Haskell hledger to compare against (`HLEDGERS_BENCH_BASELINE`,
else `target/bench-cache/hledger`, else `hledger` on `PATH`) and generates the
benchmark journals into `target/bench-cache/`: household journals of 0, 10,
100, 1000 and 100,000 transactions (`--household N` prints one: declared and
typed accounts, a salary, card spending, bills, savings, a brokerage account
trading lotful stocks at cost, dividends, spending in euros, weekly market
prices; one year per 10,000 transactions), and generatejournal's 10k- and
100k-transaction journals (`--generate NUMTXNS NUMACCTS ACCTDEPTH` prints
one). `--run` times both programs, their runs
alternating, with `balance`, `register`, `is`, `bs` and `roi -V` on the
household journals, and with `stats`, `balance`, `print`, `register`,
`bal -M`, `reg -w 120` and `print -O json` on the others and on
`hledger/hledger/bench/10000x1000x10.journal`, reporting the median wall time,
peak memory and whether their output is identical, in `bench/results.md`.

The best baseline is hledger built from the same commit as the submodule, so
the two programs are compared feature for feature. With GHC and cabal
installed (for example through ghcup), from a clone of the submodule's commit
with a `cabal.project` that lists only `hledger-lib` and `hledger`:

    cabal install exe:hledger --installdir=$HOME/.local/bin --install-method=copy

Linking needs `libgmp-dev`. The bench crate has no dependencies.

### Results

The daily commands on the household journals, hledger 1.99 / hledgers
(speedup), median wall time of 100 alternating runs of each program (5 at
100,000 transactions), from `bench/results.md`; `roi` is `roi --inv
assets:investments --pnl income:investments -V`, which both programs refuse
on the empty journal:

| transactions | `balance` | `register` | `is` | `bs` | `roi` |
|---|---:|---:|---:|---:|---:|
| 0 | 31.0 ms / 6.6 ms (4.7x) | 30.2 ms / 6.5 ms (4.7x) | 31.5 ms / 6.8 ms (4.6x) | 31.3 ms / 6.8 ms (4.6x) | n/a |
| 100 | 56.2 ms / 11.4 ms (4.9x) | 78.8 ms / 14.9 ms (5.3x) | 56.1 ms / 11.5 ms (4.9x) | 57.4 ms / 11.8 ms (4.9x) | 55.8 ms / 11.7 ms (4.8x) |
| 1,000 | 170.2 ms / 39.9 ms (4.3x) | 527.0 ms / 86.2 ms (6.1x) | 168.5 ms / 39.3 ms (4.3x) | 168.5 ms / 41.2 ms (4.1x) | 205.6 ms / 40.5 ms (5.1x) |
| 100,000 | 7.599 s / 1.855 s (4.1x) | 21.559 s / 3.859 s (5.6x) | 7.842 s / 2.099 s (3.7x) | 7.281 s / 2.201 s (3.3x) | 71.338 s / 2.082 s (34.3x) |

A run on an empty journal takes hledgers about 6 ms and hledger 29 ms; of
those 6 ms, about 2 ms is what starting any program takes on this machine
(`/bin/true` takes 2.0 ms), and much of the rest is the kernel loading the
17 MB binary and mapping its pages in.

On the 100k-transaction journal (`generatejournal 100000 1000 10`: a
thousand accounts nested up to ten deep, 26 commodities, costs on two
entries in three, and 100,000 price directives), median of five
alternating runs:

| command | hledger 1.99 | hledgers | speedup | peak RSS hledger | peak RSS hledgers |
|---|---:|---:|---:|---:|---:|
| `stats` | 7.094 s | 1.069 s | 6.6x | 563 MB | 291 MB |
| `balance` | 6.834 s | 1.161 s | 5.9x | 567 MB | 291 MB |
| `print` | 10.429 s | 1.453 s | 7.2x | 566 MB | 291 MB |
| `register` | 37.673 s | 6.980 s | 5.4x | 642 MB | 1231 MB |
| `bal -M` | 116.935 s | 24.401 s | 4.8x | 3687 MB | 3557 MB |
| `reg -w 120` | 44.826 s | 8.193 s | 5.5x | 642 MB | 1452 MB |
| `print -O json` | 36.863 s | 10.280 s | 3.6x | 1424 MB | 1696 MB |

The machine is a Xen virtual machine with 16 cores of an Intel Xeon
E5-2690 v2 (3.0 GHz) and 16 GB of memory, running Debian with Linux 6.12;
hledger 1.99 is built with GHC from the submodule's commit. Its timings
vary from run to run, at times by a factor of two, with the host's load.

hledgers does work over independent items on all cores: a large file's
transactions after its directives are parsed in parts, transactions are
balanced and styled, market prices inferred, and `print`, `register`,
`aregister` and the balance report render their lines, each in parallel,
the results in order. Work on fewer than 5000 transactions, postings or
lines stays on one thread, unless the threads were started already, as
starting them costs more than a small journal's whole run. This is the
`parallel` feature, on by default; `cargo build --release
--no-default-features` builds hledgers to use one core, with the same
output.

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

Conditions need not nest, and `if` blocks can not contain other `if` blocks.
Every block applies in turn and the last assignment wins, so a `let` carries
a choice from one block to the next: many inner conditions set only a let,
and the few outer conditions lay out the postings around it, each written
once. Here a `card` column says whether a purchase was joint or personal; a
joint one is split half to the expense and a quarter to each friend:

    skip 1
    fields date, description, amount, card
    account1 assets:checking
    amount1 %amount

    let category expenses:unknown
    if,let:category
    DINNER|RESTAURANT,expenses:dining
    SUPERMARKET|GROCER,expenses:groceries
    FUEL|SHELL,expenses:car:fuel

    if %card joint
        account2 %{category}
        amount2 -%{split(%amount, "2:1:1", 1)}
        account3 assets:receivable:alice
        amount3 -%{split(%amount, "2:1:1", 2)}
        account4 assets:receivable:bob
        amount4 -%{split(%amount, "2:1:1", 3)}
    if %card personal
        account2 %{category}

The row `2026-11-05,DINNER WITH FRIENDS,-90.01,joint` gives dining 45.01 and
22.50 to each receivable; a personal supermarket row gives one groceries
posting. The inner conditions could as well be `if` blocks, each setting the
let. When one case needs both conditions, hledger's `&` matcher lines AND
them within one block.

The choice can also go the other way. A matcher names a let as it names a
CSV field: `%NAME REGEX` tests the CSV field `NAME` if there is one,
otherwise the let `NAME` as assigned above the block, by the top-level `let`
lines and the blocks and tables before it (above its first assignment a let
is empty, and `!%NAME REGEX` negates the test). So a table can name each
account number once, and later blocks branch on the name:

    if,let:account
    %account_number 1234,joint
    %account_number 5678,personal

    if %account joint
    & %type Internal Transfer
      skip

`tests/rules/let-split.test` has more examples. Limits: a split part has no
cost (an `@` cost of `VALUE` is dropped); weights use `.` as their decimal
mark; and since `split` is a template function, hledger's error for an
unknown function, which lists them all, lists it too.

## `pair` in CSV rules

Some banks export every account into one file, and a transfer between two
of them appears twice: a record for the account the money left and one for
the account it reached, with the same date and time. hledger converts
records one at a time (its `merge` rule joins only the records that follow
one), so the two halves can only become two transactions. hledgers adds a
`pair` rule that joins them wherever they are in the file:

    if MATCHER
      pair KEY

A record the block matches is the first row of its group. Its partner is
the first other record in the file, before or after it, that has no `pair`
rule of its own, is not paired already, and for which `KEY`, an ordinary
field template, renders to the same text. The partner's fields are then the
record's row 2 fields, `%FIELD_2` or `%N_2`, as after `merge`, and the
partner makes no transaction of its own. A record with no partner converts
on its own, with its row 2 fields empty. The block's other assignments
apply whether or not a partner was found, so assignments meant only for a
pair go in a block that matches a partner field, as `if %side_2 Destination`
does below, and `!%side_2 .` matches the records left unpaired. Skipped
records never pair, and pairing follows merging, so a merged group
can be a partner, its rows becoming rows 2 and 3. An error in a paired
transaction cites the first record's line and shows both rows.

Here two of the bank's accounts, 1234 and 5678, are in one export. The
transfer at 09:24:50 becomes one transaction; the Destination at 08:14:21
has no Source in the file, so it came from an account outside the export:

    date,time,account_number,type,side,amount
    2026-09-30,09:24:50,5678,Internal Transfer,Source,-1000.00
    2026-09-30,08:14:21,1234,Internal Transfer,Destination,1000.00
    2026-09-30,09:24:50,1234,Internal Transfer,Destination,1000.00

    skip 1
    fields date, time, account_number, type, side, amount
    if,let:account
    %account_number 1234,joint
    %account_number 5678,personal

    account1 assets:bank:%{account}
    amount1 %amount

    # A transfer between our own accounts is two records with the same date
    # and time: the Source becomes posting 1 and its Destination posting 2.
    if %side Source
      pair %date %time

    # Only a Source that found its Destination has row 2 fields.
    if %side_2 Destination
      description transfer to %{to}
      account2 assets:bank:%{to}
      amount2 %amount_2
    if,let:to
    %account_number_2 1234,joint
    %account_number_2 5678,personal

    # A Destination without a Source in the file came from an account that
    # is not in the export.
    if %side Destination
      description contribution from partner
      account2 equity:contributions:partner

    $ hledgers -f bank.csv print
    2026-09-30 transfer to joint
        assets:bank:personal                       -1000.00
        assets:bank:joint                           1000.00

    2026-09-30 contribution from partner
        assets:bank:joint                           1000.00
        equity:contributions:partner               -1000.00

`tests/rules/pair.test` has more examples. Limits: the key is compared as
text, so a field in it must be written the same way in both records; and a
match group (`\1`) in the key is empty on the partner's side, whose record
the block did not match.

## Licence

GPL-3.0-or-later, the same as hledger, from which this is derived
(hledger is copyright Simon Michael and contributors). See `LICENSE`.
