//! Reading journals (Hledger.Read and its submodules): the input options,
//! and the parsers for amounts and journal files.

pub mod amount;
pub mod common;
pub mod journal;

use crate::amount::CommodityStyles;
use crate::dates::{DateSpan, Day};

/// How transactions are balanced (BalancingOpts). The last five fields are
/// set from the journal when it is finalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancingOpts {
    /// Don't check balance assertions (`-I`).
    pub ignore_assertions: bool,
    /// May costs be inferred to balance transactions? (Not with `--strict`
    /// or `check balanced`.)
    pub infer_balancing_costs: bool,
    /// Commodity display styles given on the command line (`-c`).
    pub commodity_styles: Option<CommodityStyles>,
    /// The precision transactions must balance to: each entry's own
    /// (`entry`), or the display precision (`display`) (`--txn-tolerance`).
    pub txn_balancing_display: bool,
    /// Show tolerated imbalances as imbalance postings (`--infer-imbalance`).
    pub infer_imbalance: bool,
    pub imbalance_account: String,
    pub account_types:
        std::collections::BTreeMap<crate::account::AccountName, crate::account::AccountType>,
    pub lotful_commodities: std::collections::BTreeSet<crate::amount::Sym>,
    pub account_lots_tags: std::collections::BTreeMap<crate::account::AccountName, String>,
    pub lenient_lots: bool,
    pub verbose_balancing_tags: bool,
}

impl Default for BalancingOpts {
    /// defbalancingopts.
    fn default() -> BalancingOpts {
        BalancingOpts {
            ignore_assertions: false,
            infer_balancing_costs: true,
            commodity_styles: None,
            txn_balancing_display: false,
            infer_imbalance: false,
            imbalance_account: crate::account::DEFAULT_IMBALANCE_ACCOUNT.to_string(),
            account_types: Default::default(),
            lotful_commodities: Default::default(),
            account_lots_tags: Default::default(),
            lenient_lots: false,
            verbose_balancing_tags: false,
        }
    }
}

/// Options for reading journals (InputOpts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputOpts {
    /// A file format to read, unless a file says otherwise.
    pub format: Option<String>,
    /// A CSV rules file to use (`--rules`).
    pub rules_file: Option<String>,
    /// Account aliases from `--alias` options, as written.
    pub aliases: Vec<String>,
    /// Obfuscate the data (`--obfuscate`).
    pub anon: bool,
    /// Read only transactions newer than last time (`--new`).
    pub new: bool,
    /// Save the latest dates read, with `new`.
    pub new_save: bool,
    /// Use this field's value as the account name (`--pivot`).
    pub pivot: String,
    /// The span in which to generate forecast transactions (`--forecast`).
    pub forecast: Option<DateSpan>,
    /// Add visible tags to generated or modified data (`--verbose-tags`).
    pub verbose_tags: bool,
    /// The report's dates, from the query (used by forecasting).
    pub report_span: DateSpan,
    /// Apply auto posting rules (`--auto`).
    pub auto: bool,
    /// Infer equity postings from costs (`--infer-equity`).
    pub infer_equity: bool,
    /// Infer costs from equity postings (`--infer-costs`).
    pub infer_costs: bool,
    /// Skip lot tracking and its checks (`--ignore-lots`).
    pub ignore_lots: bool,
    pub balancing: BalancingOpts,
    /// Do the strict checks (`--strict`).
    pub strict: bool,
    /// Postpone checks, while several files are read.
    pub defer: bool,
    /// The import command is reading.
    pub importing: bool,
    /// `import --dry-run`.
    pub dry_run: bool,
    /// Today's date.
    pub today: Day,
    /// Pair timeclock entries the old way (`--old-timeclock`).
    pub old_timeclock: bool,
    /// The main journal file's directory.
    pub journal_dir: Option<String>,
}

impl Default for InputOpts {
    /// definputopts.
    fn default() -> InputOpts {
        InputOpts {
            format: None,
            rules_file: None,
            aliases: Vec::new(),
            anon: false,
            new: false,
            new_save: true,
            pivot: String::new(),
            forecast: None,
            verbose_tags: false,
            report_span: DateSpan::default(),
            auto: false,
            infer_equity: false,
            infer_costs: false,
            ignore_lots: false,
            balancing: BalancingOpts::default(),
            strict: false,
            defer: false,
            importing: false,
            dry_run: false,
            today: crate::dates::null_date(),
            old_timeclock: false,
            journal_dir: None,
        }
    }
}
