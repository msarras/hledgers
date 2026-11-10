//! The add command (Hledger.Cli.Commands.Add): transactions entered at
//! prompts, with defaults from the command line or a similar past
//! transaction, checked against the whole journal, and appended to its
//! file.

use std::io::{BufRead, IsTerminal, Write};

use crate::account::{account_name_posting_type, expand_account_names};
use crate::amount::render::{AmountFormat, show_mixed_amount_b};
use crate::amount::{MixedAmount, Precision, Rounding, styles_with_rounding};
use crate::cli::args::{Arity, Flag, General, Hidden, Mode};
use crate::cli::commands::print::{LAYOUT_FLAG, journal_similar_transaction, layout_from_raw_opts};
use crate::cli::opts::CliOpts;
use crate::dates::{Day, fix_smart_date, smartdate};
use crate::model::render::{PostingLayout, show_account_name, show_transaction};
use crate::model::{BalanceAssertion, Journal, Posting, Status, Tag, Transaction};
use crate::parse::{Cursor, Item, ParseError, Parser};
use crate::read::amount::{AmountCtx, AmountKind, amountp, balance_assertionp};
use crate::read::common::{accountnamep, codep, posting_commentp};
use crate::text::strip;

pub static MODE: Mode = Mode {
    name: "add",
    flags: &[
        Flag::new(
            &["no-new-accounts"],
            Arity::None,
            "",
            "don't allow creating new accounts",
        ),
        LAYOUT_FLAG,
    ],
    general: General::Group2,
    hidden: Hidden::Conf,
    args: Some("[-f JOURNALFILE] [DATE [DESCRIPTION [ACCOUNT1 [ETC..]]]]]"),
    doc: crate::command_doc!("Add"),
    ..Mode::DEFAULT
};

const HELP: &str = "Any command line arguments will be used as defaults.
Use tab key to complete, readline keys to edit, enter to accept defaults.
An optional (CODE) may follow transaction dates.
An optional ; COMMENT may follow descriptions or amounts.
If you make a mistake, enter < at any prompt to go one step backward.
To end a transaction, enter . when prompted.
To quit, enter . at a date prompt or press control-d or control-c.
";

/// Read transactions at prompts and append each to the journal's file,
/// until the input ends or `.` at a date prompt; nothing with a journal
/// read from standard input (add).
pub fn add(opts: &CliOpts, j: Journal) -> Result<(), String> {
    let file = j.files.first().map_or("(unknown)", |(f, _)| f).to_string();
    if file == "-" {
        return Ok(());
    }
    eprintln!("Adding transactions to journal file {file}");
    eprint!("{HELP}");
    let today = opts.spec.today;
    let mut state = AddState {
        opts,
        layout: layout_from_raw_opts(opts)?,
        args: opts.raw.args().map(str::to_string).collect(),
        today,
        def_date: today,
        journal: j,
        similar: None,
        postings: Vec::new(),
        inferred: Vec::new(),
    };
    let mut input = Input::new();
    loop {
        let start = state.clone();
        match transaction_wizard(&mut input, &mut state)? {
            Wizard::Done(t) => {
                let t = *t;
                if opts.debug > 0 {
                    eprintln!("Skipping journal add due to debug mode.");
                } else {
                    let text = show_transaction(&t, state.layout, false);
                    super::import::append_to_journal_file_or_stdout(&file, || Ok(text))?;
                    eprintln!("Saved.");
                }
                eprintln!("Starting the next transaction (. or ctrl-D/ctrl-C to quit)");
                state = start;
                state.def_date = t.date;
                state.journal.txns.push(t);
            }
            Wizard::Restart => {
                eprintln!("Restarting this transaction.");
                state = start;
            }
            Wizard::Eof => return Ok(()),
        }
    }
}

/// What the prompts have gathered so far (AddState).
#[derive(Clone)]
struct AddState<'a> {
    opts: &'a CliOpts,
    layout: PostingLayout,
    /// Command line arguments not yet used as defaults.
    args: Vec<String>,
    today: Day,
    /// The default date of the next transaction.
    def_date: Day,
    journal: Journal,
    /// The past transaction most like the one being entered.
    similar: Option<Transaction>,
    postings: Vec<Posting>,
    /// The amounts inferred for the postings' balance assignments.
    inferred: Vec<Option<MixedAmount>>,
}

/// A step of entering a transaction (AddStep).
#[derive(Clone)]
enum Step {
    Date,
    Description(Day, String),
    /// Whether a posting was just entered.
    Posting(TxnData, bool),
    Account(TxnData),
    Amount(TxnData, String),
    Confirm(Box<Transaction>),
}

#[derive(Clone)]
struct TxnData {
    date: Day,
    code: String,
    desc: String,
    comment: String,
}

/// The previous inputs, shown again when going back with `<` (PrevInput).
#[derive(Default)]
struct PrevInput {
    date_and_code: Option<String>,
    desc_and_comment: Option<String>,
    accounts: Vec<String>,
    amounts: Vec<String>,
}

/// How entering a transaction ends.
enum Wizard {
    Done(Box<Transaction>),
    Restart,
    Eof,
}

/// A step's result: a value, or going back with `<`.
enum Answer<T> {
    Value(T),
    Back,
}

/// Interact to get a transaction (transactionWizard): the steps on a
/// stack, `<` going back one.
fn transaction_wizard(input: &mut Input, state: &mut AddState) -> Result<Wizard, String> {
    let mut prev = PrevInput::default();
    let mut stack = vec![Step::Date];
    macro_rules! ask {
        ($e:expr) => {
            match $e {
                Some(v) => v,
                None => return Ok(Wizard::Eof),
            }
        };
    }
    loop {
        let Some(step) = stack.last().cloned() else {
            stack.push(Step::Date);
            continue;
        };
        match step {
            Step::Date => match ask!(date_wizard(input, &prev, state)) {
                Answer::Value((date, code)) => {
                    drop_arg(state);
                    state.def_date = date;
                    let code_s = if code.is_empty() {
                        String::new()
                    } else {
                        format!(" ({code})")
                    };
                    prev.date_and_code = Some(format!("{date}{code_s}"));
                    stack.push(Step::Description(date, code));
                }
                Answer::Back => {}
            },
            Step::Description(date, code) => match ask!(description_wizard(input, &prev, state)) {
                Answer::Value((desc, comment)) => {
                    let similar = journal_similar_transaction(state.opts, &state.journal, &desc);
                    drop_arg(state);
                    state.postings.clear();
                    state.inferred.clear();
                    let c = if comment.is_empty() {
                        String::new()
                    } else {
                        format!("  ; {comment}")
                    };
                    prev.desc_and_comment = Some(format!("{desc}{c}"));
                    if let Some(t) = &similar {
                        eprintln!("Using this similar transaction for defaults:");
                        eprint!("{}", show_transaction(t, state.layout, false));
                    }
                    state.similar = similar;
                    let data = TxnData {
                        date,
                        code,
                        desc,
                        comment,
                    };
                    stack.push(Step::Posting(data, false));
                }
                Answer::Back => {
                    stack.pop();
                }
            },
            Step::Posting(data, entered) => {
                if state.postings.is_empty() || entered {
                    stack.push(Step::Account(data));
                    continue;
                }
                let t = Transaction {
                    date: data.date,
                    status: Status::Unmarked,
                    code: data.code.clone(),
                    description: data.desc.clone(),
                    comment: data.comment.clone(),
                    postings: state.postings.clone(),
                    ..Transaction::default()
                };
                match balance_transaction_in_journal(t, state) {
                    Ok(t) => stack.push(Step::Confirm(Box::new(t))),
                    Err(e) => {
                        eprintln!("\n{}, please re-enter.", capitalize(&e));
                        stack.pop();
                        stack.push(Step::Account(data));
                    }
                }
            }
            Step::Account(data) => match ask!(account_wizard(input, &prev, state)) {
                Answer::Value(a) if a == "." || a.is_empty() => {
                    if state.postings.is_empty() {
                        eprintln!("Please enter some postings first.");
                    } else if !postings_are_balanced(&state.journal, &state.postings) {
                        eprintln!("Please enter more postings to balance the transaction.");
                    } else {
                        stack.push(Step::Posting(data, false));
                    }
                }
                Answer::Value(a) => {
                    replace_nth_or_append(&mut prev.accounts, state.postings.len(), a.clone());
                    drop_arg(state);
                    stack.push(Step::Amount(data, a));
                }
                Answer::Back => {
                    state.postings.pop();
                    state.inferred.pop();
                    while stack
                        .last()
                        .is_some_and(|s| !matches!(s, Step::Amount(..) | Step::Description(..)))
                    {
                        stack.pop();
                    }
                }
            },
            Step::Amount(data, account) => match ask!(amount_wizard(input, &prev, state)) {
                Answer::Value((amount, assertion, (comment, tags, date, date2))) => {
                    let mixed = amount.map_or_else(MixedAmount::missing, MixedAmount::from_amount);
                    let realness = account_name_posting_type(&account);
                    let p = Posting {
                        account: strip_brackets(&account).into(),
                        amount: mixed.clone(),
                        comment: comment.trim_matches('\n').to_string(),
                        realness,
                        assertion: assertion.map(Box::new),
                        date,
                        date2,
                        tags,
                        ..Posting::default()
                    };
                    let shown = format!(
                        "{}{}",
                        crate::amount::render::show_mixed_amount_one_line(&mixed),
                        if comment.is_empty() {
                            String::new()
                        } else {
                            format!("  ;{comment}")
                        }
                    );
                    let mut postings = state.postings.clone();
                    postings.push(p.clone());
                    postings.push(Posting::new("", crate::amount::Amount::missing()));
                    let dummy = Transaction {
                        date: data.date,
                        description: data.desc.clone(),
                        postings,
                        ..Transaction::default()
                    };
                    let assignment = mixed.is_missing() && p.assertion.is_some();
                    match balance_transaction_in_journal(dummy, state) {
                        Err(e) => eprintln!("{e}"),
                        Ok(balanced) => {
                            let inferred = assignment
                                .then(|| balanced.postings.get(state.postings.len()))
                                .flatten()
                                .map(|p| p.amount.clone());
                            replace_nth_or_append(&mut prev.amounts, state.postings.len(), shown);
                            state.postings.push(p);
                            state.inferred.push(inferred);
                            drop_arg(state);
                            stack.push(Step::Posting(data, true));
                        }
                    }
                }
                Answer::Back => {
                    stack.pop();
                }
            },
            Step::Confirm(t) => {
                input.output(&show_transaction(&t, state.layout, false));
                loop {
                    let line = ask!(input.read(
                        "Save this transaction to the journal ? [y]: ",
                        "",
                        Vec::new(),
                        String::new()
                    ));
                    let line = if line.is_empty() {
                        "y".to_string()
                    } else {
                        line
                    };
                    match strip(&line).to_lowercase().chars().next() {
                        None => input.output_ln("Please enter y or n."),
                        Some('<') => {
                            stack.pop();
                            stack.pop();
                            break;
                        }
                        Some('y') => return Ok(Wizard::Done(t)),
                        Some(_) => return Ok(Wizard::Restart),
                    }
                }
            }
        }
    }
}

fn drop_arg(state: &mut AddState) {
    if !state.args.is_empty() {
        state.args.remove(0);
    }
}

fn replace_nth_or_append(xs: &mut Vec<String>, n: usize, x: String) {
    if n < xs.len() {
        xs[n] = x;
    } else {
        xs.push(x);
    }
}

/// A default shown in a prompt.
fn show_default(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(" [{s}]")
    }
}

/// Get a date, perhaps followed by a `(CODE)`, accepting smart dates; `.`
/// ends the input (dateWizard). The code is read in lower case, as
/// upstream reads the whole line.
fn date_wizard(
    input: &mut Input,
    prev: &PrevInput,
    state: &AddState,
) -> Option<Answer<(Day, String)>> {
    let def = (state.args.first().cloned()).unwrap_or_else(|| state.def_date.to_string());
    let completions = ["today", "tomorrow", "yesterday"]
        .map(str::to_string)
        .to_vec();
    loop {
        let s = input.read(
            &format!("Date{}: ", show_default(&def)),
            prev.date_and_code.as_deref().unwrap_or(""),
            completions.clone(),
            def.clone(),
        )?;
        if s == "." {
            return None;
        }
        let s = if s.is_empty() { def.clone() } else { s };
        if s == "<" {
            return Some(Answer::Back);
        }
        if let Some(dc) = parse_date_and_code(state.today, &s) {
            return Some(Answer::Value(dc));
        }
        input.output_ln(
            "A valid hledger smart date is required. Eg: 2022-08-30, 8/30, 30, yesterday.",
        );
    }
}

fn parse_date_and_code(today: Day, s: &str) -> Option<(Day, String)> {
    let lower = s.to_lowercase();
    let mut p = Parser::new(&lower);
    let d = smartdate(&mut p).ok()?;
    let mut c = Cursor::new(p.rest());
    let code = codep(&mut c).ok()?.to_string();
    c.skip_spaces();
    c.at_end().then(|| (fix_smart_date(today, &d).day, code))
}

/// Get a description, perhaps followed by `; COMMENT` (descriptionWizard).
fn description_wizard(
    input: &mut Input,
    prev: &PrevInput,
    state: &AddState,
) -> Option<Answer<(String, String)>> {
    let def = state.args.first().cloned().unwrap_or_default();
    let j = &state.journal;
    let mut payees: Vec<String> = (j.declared_payees.iter().map(|(p, _)| p.clone()))
        .chain(j.txns.iter().map(|t| t.payee().to_string()))
        .collect();
    payees.sort();
    payees.dedup();
    let mut descriptions: Vec<String> = j.txns.iter().map(|t| t.description.clone()).collect();
    descriptions.sort();
    descriptions.dedup();
    let mut completions = payees;
    for d in descriptions {
        if !completions.contains(&d) {
            completions.push(d);
        }
    }
    let s = input.read(
        &format!("Description{}: ", show_default(&def)),
        prev.desc_and_comment.as_deref().unwrap_or(""),
        completions,
        def.clone(),
    )?;
    let s = if s.is_empty() { def } else { s };
    if s == "<" {
        return Some(Answer::Back);
    }
    let (a, b) = s.split_once(';').unwrap_or((&s, ""));
    Some(Answer::Value((
        strip(a).to_string(),
        strip(b.trim_start_matches(';')).to_string(),
    )))
}

/// The account names declared or posted to, with their parents
/// (journalAccountNamesDeclaredOrImplied).
fn account_names_declared_or_implied(j: &Journal) -> Vec<String> {
    let used: Vec<&str> = j.postings().map(|p| &*p.p.account).collect();
    let mut names: Vec<String> = (j.declared_accounts.iter().map(|(a, _)| a.to_string()))
        .chain(expand_account_names(used).into_iter().map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Get an account name, in parentheses or brackets for a virtual posting;
/// `.`, or an empty answer without a default, finishes the transaction
/// when it is balanced (accountWizard).
fn account_wizard(input: &mut Input, prev: &PrevInput, state: &AddState) -> Option<Answer<String>> {
    let pnum = state.postings.len() + 1;
    let historical = (state.similar.as_ref())
        .map(|t| {
            t.postings
                .get(pnum - 1)
                .map_or_else(String::new, show_account_name)
        })
        .unwrap_or_default();
    let def = state.args.first().cloned().unwrap_or(historical);
    let can_finish =
        !state.postings.is_empty() && postings_are_balanced(&state.journal, &state.postings);
    let endmsg = match (can_finish, def.is_empty()) {
        (true, true) => " (or . or enter to finish this transaction)",
        (true, false) => " (or . to finish this transaction)",
        _ => "",
    };
    let names = account_names_declared_or_implied(&state.journal);
    loop {
        let s = input.read(
            &format!("Account {pnum}{endmsg}{}: ", show_default(&def)),
            prev.accounts
                .get(state.postings.len())
                .map_or("", String::as_str),
            names.clone(),
            def.clone(),
        )?;
        let parsed = match s.as_str() {
            "<" => Some(Answer::Back),
            "." => Some(Answer::Value(".".to_string())),
            "" if def.is_empty() && can_finish => Some(Answer::Value(String::new())),
            "" if !def.is_empty() => Some(Answer::Value(def.clone())),
            _ => {
                let mut c = Cursor::new(&s);
                match accountnamep(&mut c, true) {
                    Ok(a) if c.at_end() => {
                        let known = || names.contains(&a);
                        (!state.opts.no_new_accounts || known()).then_some(Answer::Value(a))
                    }
                    _ => None,
                }
            }
        };
        if let Some(a) = parsed {
            return Some(a);
        }
        input.output_ln(
            "A valid hledger account name is required. Eg: assets:cash, expenses:food:eating out.",
        );
    }
}

/// An amount, balance assertion and posting comment (its text, tags and
/// dates) entered.
type AmountAnswer = (
    Option<crate::amount::Amount>,
    Option<BalanceAssertion>,
    (String, Vec<Tag>, Option<Day>, Option<Day>),
);

/// Get an amount and/or a balance assertion, perhaps followed by
/// `; COMMENT` (amountWizard). The default is the command line's next
/// argument, or the similar transaction's amount while its amounts have
/// been followed, or the amount balancing the postings so far. A parse
/// error is shown and the prompt repeated.
fn amount_wizard(
    input: &mut Input,
    prev: &PrevInput,
    state: &AddState,
) -> Option<Answer<AmountAnswer>> {
    let pnum = state.postings.len() + 1;
    let j = &state.journal;
    let styles = styles_with_rounding(Rounding::No, &j.commodity_styles());
    let showamt = |m: &MixedAmount| {
        let fmt = AmountFormat {
            force_decimal_mark: true,
            ..AmountFormat::DEFAULT
        };
        show_mixed_amount_b(
            &fmt,
            &m.clone().with_precision(Precision::Natural).styled(&styles),
        )
        .s
    };
    let (historical, followed) = match &state.similar {
        None => (None, false),
        Some(t) => (
            t.postings.get(pnum - 1),
            (state.postings.iter().zip(&t.postings)).all(|(p1, p2)| p1.amount == p2.amount),
        ),
    };
    let filled: Vec<Posting> = (state.postings.iter().enumerate())
        .map(|(i, p)| match state.inferred.get(i).cloned().flatten() {
            Some(a) => Posting {
                amount: a,
                ..p.clone()
            },
            None => p.clone(),
        })
        .collect();
    let balancing = -crate::reports::postings::sum_postings(filled.iter().filter(|p| p.is_real()));
    let def = if let Some(d) = state.args.first() {
        d.clone()
    } else if let Some(hp) = historical.filter(|_| followed) {
        showamt(&hp.amount)
    } else if pnum > 1 && !balancing.looks_zero() {
        let first = balancing.amounts().into_iter().take(1);
        showamt(&MixedAmount::from_amounts(first))
    } else {
        String::new()
    };
    loop {
        let s = input.read(
            &format!("Amount  {pnum}{}: ", show_default(&def)),
            prev.amounts
                .get(state.postings.len())
                .map_or("", String::as_str),
            Vec::new(),
            def.clone(),
        )?;
        let s = if s.is_empty() { def.clone() } else { s };
        if s == "<" {
            return Some(Answer::Back);
        }
        match parse_amount_and_comment(j, state.def_date.year(), &s) {
            Ok(a) => return Some(Answer::Value(a)),
            Err(e) => input.output_ln(&e),
        }
    }
}

/// An amount, an assertion and a comment read from text, with the
/// journal's decimal mark, commodities and default year but not its
/// default commodity; or the parse error as shown (amountandcommentp).
fn parse_amount_and_comment(j: &Journal, year: i64, s: &str) -> Result<AmountAnswer, String> {
    let ctx = AmountCtx {
        decimal_mark: j.parse_decimal_mark,
        default_commodity: None,
        commodities: Some(&j.declared_commodities),
        year: j.parse_year,
    };
    let mut c = Cursor::new(s);
    let pretty = |e: ParseError| e.pretty("", s);
    let amount = match amountp(&mut c, &ctx, AmountKind::Ordinary) {
        Ok(a) => Some(a),
        Err(_) if c.pos == 0 => None,
        Err(e) => return Err(pretty(e)),
    };
    c.skip_spaces();
    let assertion = if c.peek() == Some('=') {
        // its position in the answer, with tabs to stops of 8 as megaparsec counts
        let col = s[..c.pos].chars().fold(1, |col, ch| match ch {
            '\t' => col + 8 - (col - 1) % 8,
            _ => col + 1,
        });
        let pos = crate::model::SourcePos {
            file: "".into(),
            line: 1,
            col,
        };
        Some(balance_assertionp(&mut c, &ctx, pos).map_err(pretty)?)
    } else {
        None
    };
    let comment = if c.eat(';') {
        let rest = c.rest().to_string();
        c = Cursor::new("");
        rest
    } else {
        String::new()
    };
    if !c.at_end() {
        let mut hints = vec![Item::tokens(";")];
        match &assertion {
            None => hints.push(Item::tokens("=")),
            Some(a) if a.amount.cost.is_none() => hints.push(Item::label("transaction price")),
            Some(_) => {}
        }
        hints.push(Item::EndOfInput);
        return Err(pretty(c.unexpected(&hints)));
    }
    let text = format!(";{comment}");
    let mut cc = Cursor::new(&text);
    match posting_commentp(&mut cc, Some(year), &[]) {
        Ok((_, tags, date, date2)) => Ok((amount, assertion, (comment, tags, date, date2))),
        Err(e) => Err(pretty(ParseError::fail_at(s, s.len(), e.pretty("", &text)))),
    }
}

/// Balance and check a transaction as the last of the journal it will be
/// added to: inferring amounts, filling balance assignments, and checking
/// all the journal's balance assertions (balanceTransactionInJournal).
fn balance_transaction_in_journal(t: Transaction, state: &AddState) -> Result<Transaction, String> {
    let mut j = state.journal.clone();
    j.txns.push(t);
    let mut bopts = state.opts.input.balancing.clone();
    bopts.account_types = j.account_types.clone();
    let (j, _) = crate::read::balancing::journal_balance_transactions(&bopts, j, false)?;
    j.txns
        .last()
        .cloned()
        .ok_or_else(|| "balanceTransactionInJournal: unexpected empty journal".to_string())
}

/// Whether these postings balance on their own (postingsAreBalanced).
fn postings_are_balanced(j: &Journal, ps: &[Posting]) -> bool {
    let bopts = crate::read::BalancingOpts {
        account_types: j.account_types.clone(),
        ..Default::default()
    };
    let t = Transaction {
        postings: ps.to_vec(),
        ..Transaction::default()
    };
    crate::read::balancing::balance_single_transaction(&bopts, t).is_ok()
}

/// An account name without enclosing brackets or parentheses
/// (stripbrackets).
fn strip_brackets(a: &str) -> &str {
    a.trim_start_matches(['(', '['])
        .trim_end_matches([')', ']'])
}

fn capitalize(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// Where answers come from: on a terminal, a line editor completing from
/// a list (or the default, for an empty answer) and starting with the
/// previous answer; otherwise standard input's lines, after each prompt.
struct Input {
    editor: Option<rustyline::Editor<Completions, rustyline::history::DefaultHistory>>,
}

impl Input {
    fn new() -> Input {
        let editor = std::io::stdin()
            .is_terminal()
            .then(|| rustyline::Editor::new().ok())
            .flatten();
        Input { editor }
    }

    /// The answer to a prompt, or None at the end of the input.
    fn read(
        &mut self,
        prompt: &str,
        prewritten: &str,
        completions: Vec<String>,
        default: String,
    ) -> Option<String> {
        match &mut self.editor {
            Some(ed) => {
                ed.set_helper(Some(Completions {
                    completions,
                    default,
                }));
                ed.readline_with_initial(prompt, (prewritten, "")).ok()
            }
            None => {
                self.output(prompt);
                let mut line = String::new();
                match std::io::stdin().lock().read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => Some(line.strip_suffix('\n').unwrap_or(&line).to_string()),
                }
            }
        }
    }

    fn output(&self, s: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(s.as_bytes()).and_then(|()| out.flush());
    }

    fn output_ln(&self, s: &str) {
        self.output(&format!("{s}\n"));
    }
}

/// Completion of a whole answer from a list, by prefix, or of an empty
/// answer with the default (completer).
struct Completions {
    completions: Vec<String>,
    default: String,
}

impl rustyline::completion::Completer for Completions {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let word = &line[..pos];
        if word.is_empty() {
            return Ok((0, vec![self.default.clone()]));
        }
        let matches = (self.completions.iter())
            .filter(|c| c.starts_with(word))
            .cloned()
            .collect();
        Ok((0, matches))
    }
}

impl rustyline::hint::Hinter for Completions {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for Completions {}
impl rustyline::validate::Validator for Completions {}
impl rustyline::Helper for Completions {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Realness;

    #[test]
    fn dates_and_codes() {
        let today = Day::from_ymd(2026, 11, 6).unwrap();
        let d = |y, m, dd| Day::from_ymd(y, m, dd).unwrap();
        assert_eq!(
            parse_date_and_code(today, "2025-05-01 (ABC) "),
            Some((d(2025, 5, 1), "abc".to_string()))
        );
        assert_eq!(
            parse_date_and_code(today, "yesterday"),
            Some((d(2026, 11, 5), String::new()))
        );
        assert_eq!(parse_date_and_code(today, "2009/1/32"), None);
        assert_eq!(parse_date_and_code(today, "1/2 (x"), None);
    }

    #[test]
    fn brackets() {
        assert_eq!(strip_brackets("((a))"), "a");
        assert_eq!(strip_brackets("[a:b]"), "a:b");
        assert_eq!(capitalize("unbalanced"), "Unbalanced");
        assert_eq!(account_name_posting_type("(a)"), Realness::Virtual);
    }
}
