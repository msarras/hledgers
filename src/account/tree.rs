//! Account trees with per-period data (Hledger.Data.Account,
//! Hledger.Data.BalanceData, Hledger.Data.PeriodData): the accounts that
//! postings use, with their parents, under a made-up root account; each
//! with a value for each report period and for the time before the report.
//! The balance reports build one from postings, accumulate and mark it, and
//! read their rows from it.
//!
//! The tree is an arena: accounts refer to their parent and subaccounts by
//! index, the root is first, and a parent always comes before its
//! subaccounts.

use std::collections::{BTreeMap, HashMap};
use std::ops::{Add, AddAssign};

use super::AccountName;
use crate::amount::MixedAmount;
use crate::dates::Day;
use crate::model::Journal;

/// What balance reports need of an account in a period: its balance
/// excluding and including its subaccounts (each typically a change or an
/// end balance), and its number of postings (BalanceData).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BalanceData {
    pub excl: MixedAmount,
    pub incl: MixedAmount,
    pub postings: usize,
}

impl BalanceData {
    /// With both balances transformed (mapBalanceData).
    pub fn map(&self, f: impl Fn(&MixedAmount) -> MixedAmount) -> BalanceData {
        BalanceData {
            excl: f(&self.excl),
            incl: f(&self.incl),
            postings: self.postings,
        }
    }

    /// Both balances combined with the other's, keeping this posting count
    /// (opBalanceData).
    pub fn op(
        &self,
        other: &BalanceData,
        f: impl Fn(&MixedAmount, &MixedAmount) -> MixedAmount,
    ) -> BalanceData {
        BalanceData {
            excl: f(&self.excl, &other.excl),
            incl: f(&self.incl, &other.incl),
            postings: self.postings,
        }
    }
}

/// The sum: balances and posting counts added (BalanceData's Semigroup).
impl Add for &BalanceData {
    type Output = BalanceData;

    fn add(self, other: &BalanceData) -> BalanceData {
        BalanceData {
            excl: &self.excl + &other.excl,
            incl: &self.incl + &other.incl,
            postings: self.postings + other.postings,
        }
    }
}

/// The sum, reusing the amounts of both.
impl Add for BalanceData {
    type Output = BalanceData;

    fn add(self, other: BalanceData) -> BalanceData {
        BalanceData {
            excl: self.excl + other.excl,
            incl: self.incl + other.incl,
            postings: self.postings + other.postings,
        }
    }
}

impl AddAssign<&BalanceData> for BalanceData {
    fn add_assign(&mut self, other: &BalanceData) {
        let this = std::mem::take(self);
        *self = BalanceData {
            excl: this.excl + &other.excl,
            incl: this.incl + &other.incl,
            postings: this.postings + other.postings,
        };
    }
}

/// A value for each of some contiguous report periods, known by their
/// start dates, and one for the time before them (PeriodData).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeriodData<T> {
    pub pre: T,
    pub periods: BTreeMap<Day, T>,
}

impl<T> PeriodData<T> {
    /// The value before the periods, and the periods' start dates with
    /// their values (periodDataFromList).
    pub fn from_list(pre: T, periods: impl IntoIterator<Item = (Day, T)>) -> PeriodData<T> {
        PeriodData {
            pre,
            periods: periods.into_iter().collect(),
        }
    }

    /// Each value transformed (fmap).
    pub fn map<U>(&self, mut f: impl FnMut(&T) -> U) -> PeriodData<U> {
        PeriodData {
            pre: f(&self.pre),
            periods: self.periods.iter().map(|(d, v)| (*d, f(v))).collect(),
        }
    }

    /// The values of the periods both have, combined (opPeriodData).
    pub fn op<U, V>(&self, other: &PeriodData<U>, f: impl Fn(&T, &U) -> V) -> PeriodData<V> {
        PeriodData {
            pre: f(&self.pre, &other.pre),
            periods: (self.periods.iter())
                .filter_map(|(d, a)| other.periods.get(d).map(|b| (*d, f(a, b))))
                .collect(),
        }
    }

    /// The values of the periods either has: those only in this one, only in
    /// the other, or in both, combined by the three functions
    /// (mergePeriodData).
    pub fn merge<U, V>(
        self,
        other: PeriodData<U>,
        only1: impl Fn(T) -> V,
        only2: impl Fn(U) -> V,
        both: impl Fn(T, U) -> V,
    ) -> PeriodData<V> {
        let mut periods = BTreeMap::new();
        let mut others = other.periods;
        for (d, a) in self.periods {
            let v = match others.remove(&d) {
                Some(b) => both(a, b),
                None => only1(a),
            };
            periods.insert(d, v);
        }
        for (d, b) in others {
            periods.insert(d, only2(b));
        }
        PeriodData {
            pre: both(self.pre, other.pre),
            periods,
        }
    }

    /// With `x` for each period of `pad` this has no value for
    /// (padPeriodData).
    pub fn pad<U>(&mut self, x: &T, pad: &PeriodData<U>)
    where
        T: Clone,
    {
        for d in pad.periods.keys() {
            self.periods.entry(*d).or_insert_with(|| x.clone());
        }
    }

    /// The value of the last period, if any.
    pub fn last(&self) -> Option<&T> {
        self.periods.values().next_back()
    }
}

impl PeriodData<BalanceData> {
    /// With this data added to the period starting on this date, or to the
    /// time before the periods (insertPeriodData).
    pub fn insert(&mut self, day: Option<Day>, b: BalanceData) {
        match day {
            None => self.pre += &b,
            Some(d) => match self.periods.get_mut(&d) {
                Some(old) => *old = b + std::mem::take(old),
                None => {
                    self.periods.insert(d, b);
                }
            },
        }
    }

    /// The other's values added to these, period by period, a period only
    /// one has keeping its value (PeriodData's Semigroup).
    pub fn add(&mut self, other: &PeriodData<BalanceData>) {
        self.pre += &other.pre;
        for (d, b) in &other.periods {
            match self.periods.get_mut(d) {
                Some(a) => *a += b,
                None => {
                    self.periods.insert(*d, b.clone());
                }
            }
        }
    }

    /// Running totals: each value the sum of those up to it, the value
    /// before the periods first (cumulativeSum).
    pub fn cumulative_sum(&self) -> PeriodData<BalanceData> {
        let mut total = BalanceData::default();
        self.map(|b| {
            total = &total + b;
            total.clone()
        })
    }

    /// The changes between running totals: each value less the one before
    /// it, keeping its posting count (periodChanges).
    pub fn period_changes(&self) -> PeriodData<BalanceData> {
        let mut prev = BalanceData::default();
        self.map(|b| {
            let change = b.op(&prev, |x, y| x - y);
            prev = b.clone();
            change
        })
    }
}

/// An account in a tree (Account).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account<T> {
    /// The full name; the root's is made up.
    pub name: AccountName,
    /// The order of its account directive, if it was declared
    /// (adeclarationinfo's adideclarationorder).
    pub declared: Option<usize>,
    pub parent: Option<usize>,
    pub subs: Vec<usize>,
    /// Whether a report can leave it out (aboring).
    pub boring: bool,
    pub data: PeriodData<T>,
}

/// A tree of accounts, the root first (Account, with the knot tied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountTree<T> {
    pub accounts: Vec<Account<T>>,
}

/// The root's index.
pub const ROOT: usize = 0;

impl<T> AccountTree<T> {
    /// A tree of these account names and their parents, under a root of
    /// this name and data; each account's data comes from `data`, and
    /// subaccounts are in name order (accountTreeFromBalanceAndNames).
    pub fn from_names<'a>(
        root: &str,
        root_data: PeriodData<T>,
        names: impl IntoIterator<Item = &'a str>,
        mut data: impl FnMut(&str) -> PeriodData<T>,
    ) -> AccountTree<T> {
        let mut all: Vec<&str> = names
            .into_iter()
            .flat_map(super::expand_account_name)
            .collect();
        all.sort_unstable();
        all.dedup();
        let mut tree = AccountTree {
            accounts: vec![Account::new(AccountName::from(root), None, root_data)],
        };
        // a parent's name sorts before its subaccounts', so it is added first
        let mut index: HashMap<&str, usize> = HashMap::with_capacity(all.len());
        for name in all {
            let parent = name
                .rfind(super::ACCT_SEP)
                .and_then(|i| index.get(&name[..i]).copied())
                .unwrap_or(ROOT);
            let i = tree.accounts.len();
            tree.accounts.push(Account::new(
                AccountName::from(name),
                Some(parent),
                data(name),
            ));
            tree.accounts[parent].subs.push(i);
            index.insert(name, i);
        }
        tree
    }

    pub fn root(&self) -> &Account<T> {
        &self.accounts[ROOT]
    }

    /// Whether the predicate holds for this account or any account below it
    /// (anyAccounts).
    pub fn any(&self, i: usize, p: &impl Fn(&Account<T>) -> bool) -> bool {
        let a = &self.accounts[i];
        p(a) || a.subs.iter().any(|&s| self.any(s, p))
    }

    /// Each account's subaccounts sorted by a key, ties keeping their order
    /// (sortAccountTreeOn).
    pub fn sort_subs_by_key<K: Ord>(&mut self, key: impl Fn(&Account<T>) -> K) {
        for i in 0..self.accounts.len() {
            let mut subs = std::mem::take(&mut self.accounts[i].subs);
            subs.sort_by_cached_key(|&s| key(&self.accounts[s]));
            self.accounts[i].subs = subs;
        }
    }

    /// Each account's subaccounts sorted by the order of their account
    /// directives, the undeclared ones last, then by name
    /// (sortAccountTreeByDeclaration).
    pub fn sort_by_declaration(&mut self) {
        self.sort_subs_by_key(|a| (a.declared.unwrap_or(usize::MAX), a.name.clone()));
    }

    /// With each account's declaration order from the journal's account
    /// directives (accountSetDeclarationInfo).
    pub fn set_declarations(&mut self, j: &Journal) {
        let mut order: HashMap<&str, usize> = HashMap::new();
        for (a, info) in &j.declared_accounts {
            order.entry(a).or_insert(info.order);
        }
        for a in &mut self.accounts {
            a.declared = order.get(&*a.name).copied();
        }
    }

    /// Each account's data transformed (mapPeriodData, or fmap).
    pub fn map_data<U>(self, mut f: impl FnMut(PeriodData<T>) -> PeriodData<U>) -> AccountTree<U> {
        AccountTree {
            accounts: (self.accounts.into_iter())
                .map(|a| Account {
                    name: a.name,
                    declared: a.declared,
                    parent: a.parent,
                    subs: a.subs,
                    boring: a.boring,
                    data: f(a.data),
                })
                .collect(),
        }
    }

    /// The two trees as one, their roots assumed to have the same name:
    /// each account's data from either or both, each period's value from
    /// either or both, declared if either is, boring if both are; the
    /// subaccounts of an account in both in name order (mergeAccounts).
    pub fn merge<B>(&self, other: &AccountTree<B>) -> AccountTree<(Option<T>, Option<B>)>
    where
        T: Clone,
        B: Clone,
    {
        let mut out = AccountTree {
            accounts: Vec::new(),
        };
        merge_into(&mut out, None, Some((self, ROOT)), Some((other, ROOT)));
        out
    }
}

impl<T> Account<T> {
    fn new(name: AccountName, parent: Option<usize>, data: PeriodData<T>) -> Account<T> {
        Account {
            name,
            declared: None,
            parent,
            subs: Vec::new(),
            boring: false,
            data,
        }
    }
}

/// Add the merge of an account of one tree and the same-named account of
/// another, either perhaps absent, and their subaccounts, below `parent`.
fn merge_into<A: Clone, B: Clone>(
    out: &mut AccountTree<(Option<A>, Option<B>)>,
    parent: Option<usize>,
    a: Option<(&AccountTree<A>, usize)>,
    b: Option<(&AccountTree<B>, usize)>,
) {
    let (x, y) = (
        a.map(|(t, i)| &t.accounts[i]),
        b.map(|(t, i)| &t.accounts[i]),
    );
    let some_a = |d: &PeriodData<A>| d.map(|v| (Some(v.clone()), None));
    let some_b = |d: &PeriodData<B>| d.map(|v| (None, Some(v.clone())));
    let (name, declared, boring, data) = match (x, y) {
        (Some(x), Some(y)) => (
            x.name.clone(),
            x.declared.or(y.declared),
            x.boring && y.boring,
            x.data.clone().merge(
                y.data.clone(),
                |v| (Some(v), None),
                |v| (None, Some(v)),
                |v, w| (Some(v), Some(w)),
            ),
        ),
        (Some(x), None) => (x.name.clone(), x.declared, x.boring, some_a(&x.data)),
        (None, Some(y)) => (y.name.clone(), y.declared, y.boring, some_b(&y.data)),
        (None, None) => return,
    };
    let i = out.accounts.len();
    out.accounts.push(Account {
        declared,
        boring,
        ..Account::new(name, parent, data)
    });
    if let Some(p) = parent {
        out.accounts[p].subs.push(i);
    }
    // an account in both has its subaccounts merged in name order; one
    // only in one tree keeps its subaccounts' order
    let mut xs: Vec<usize> = x.map_or_else(Vec::new, |x| x.subs.clone());
    let mut ys: Vec<usize> = y.map_or_else(Vec::new, |y| y.subs.clone());
    if x.is_some() && y.is_some() {
        let (ta, tb) = (a.expect("present").0, b.expect("present").0);
        xs.sort_by(|&s, &t| ta.accounts[s].name.cmp(&ta.accounts[t].name));
        ys.sort_by(|&s, &t| tb.accounts[s].name.cmp(&tb.accounts[t].name));
    }
    let (mut xi, mut yi) = (xs.into_iter().peekable(), ys.into_iter().peekable());
    loop {
        let next = match (xi.peek(), yi.peek()) {
            (Some(&s), Some(&t)) => {
                let (ta, tb) = (a.expect("present").0, b.expect("present").0);
                match ta.accounts[s].name.cmp(&tb.accounts[t].name) {
                    std::cmp::Ordering::Equal => (xi.next(), yi.next()),
                    std::cmp::Ordering::Less => (xi.next(), None),
                    std::cmp::Ordering::Greater => (None, yi.next()),
                }
            }
            (Some(_), None) => (xi.next(), None),
            (None, Some(_)) => (None, yi.next()),
            (None, None) => break,
        };
        let sub_a = next.0.map(|s| (a.expect("present").0, s));
        let sub_b = next.1.map(|t| (b.expect("present").0, t));
        merge_into(out, Some(i), sub_a, sub_b);
    }
}

impl AccountTree<BalanceData> {
    /// The tree of the accounts these postings use, each with its postings'
    /// amounts added up in the period starting on their date (or before the
    /// periods if they have none) as its balance excluding subaccounts,
    /// then with the balances including subaccounts calculated. A posting to
    /// the account `...` (an account clipped to depth 0) belongs to the root
    /// (accountFromPostings).
    pub fn from_postings<'a>(
        postings: impl IntoIterator<Item = (Option<Day>, &'a str, &'a MixedAmount)>,
    ) -> AccountTree<BalanceData> {
        // each account's postings, in order, are added up apart from the others'
        let mut by_account: HashMap<&str, Vec<(Option<Day>, &MixedAmount)>> = HashMap::new();
        for (day, account, amount) in postings {
            by_account.entry(account).or_default().push((day, amount));
        }
        let by_account: Vec<_> = by_account.into_iter().collect();
        let mut balances: HashMap<&str, PeriodData<BalanceData>> =
            crate::par::map_owned(by_account, |(account, postings)| {
                let mut data = PeriodData::default();
                for (day, amount) in postings {
                    let b = BalanceData {
                        excl: amount.clone(),
                        incl: MixedAmount::null(),
                        postings: 1,
                    };
                    data.insert(day, b);
                }
                (account, data)
            })
            .into_iter()
            .collect();
        let root_data = balances.remove("...").unwrap_or_default();
        let names: Vec<&str> = balances.keys().copied().collect();
        let mut tree = AccountTree::from_names("root", root_data, names, |n| {
            balances.remove(n).unwrap_or_default()
        });
        tree.sum();
        tree
    }

    /// With every account's balances including subaccounts recalculated:
    /// its own balance plus its subaccounts', period by period
    /// (sumAccounts). The subaccounts are added from the last to the first,
    /// each on the left of the sum so far, as foldMap nests them: with
    /// Data.Decimal's zero shortcut, the places of a sum that passes through
    /// zero depend on the order (1,000.50 + (-1,000.50 + 1) is 1.00;
    /// (1,000.50 + -1,000.50) + 1 is 1), and a merged amount keeps the
    /// left operand's cost.
    pub fn sum(&mut self) {
        for i in (0..self.accounts.len()).rev() {
            let mut subtotals = PeriodData::default();
            for &s in self.accounts[i].subs.iter().rev() {
                let mut sub = self.accounts[s].data.clone();
                sub.add(&subtotals);
                subtotals = sub;
            }
            let this = std::mem::take(&mut self.accounts[i].data);
            self.accounts[i].data = subtotals.merge(
                this,
                |children| BalanceData {
                    incl: children.incl,
                    ..BalanceData::default()
                },
                |this| BalanceData {
                    incl: this.excl.clone(),
                    ..this
                },
                |children, this| BalanceData {
                    incl: &this.excl + &children.incl,
                    ..this
                },
            );
        }
    }
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::usd;
    use crate::decimal::Decimal;

    fn usds(n: i64) -> MixedAmount {
        MixedAmount::from_amount(usd(Decimal::from_i64(n)))
    }

    fn bd(e: i64, i: i64, n: usize) -> BalanceData {
        BalanceData {
            excl: usds(e),
            incl: usds(i),
            postings: n,
        }
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    // tests_BalanceData.
    #[cfg_attr(test, test)]
    pub(crate) fn balance_data_ops() {
        let (a, b) = (bd(1, 2, 5), bd(3, 4, 0));
        assert_eq!(a.op(&b, |x, y| x + y), bd(4, 6, 5));
        assert_eq!(a.op(&b, |x, y| x - y), bd(-2, -2, 5));
    }

    // tests_PeriodData.
    #[cfg_attr(test, test)]
    pub(crate) fn period_data_semigroup() {
        let list = |h: i64, x: i64, y: i64| {
            PeriodData::from_list(
                bd(h, 0, 0),
                [
                    (day(2000, 1, 1), bd(x, 0, 0)),
                    (day(2004, 2, 28), bd(y, 0, 0)),
                ],
            )
        };
        let day_map = list(1, 2, 3);
        assert_eq!(day_map.periods.len() + 1, 3);
        let mut doubled = day_map.clone();
        doubled.add(&day_map);
        assert_eq!(doubled, list(2, 4, 6));
        let mut same = day_map.clone();
        same.add(&PeriodData::default());
        assert_eq!(same, day_map);
    }

    // tests_Account.
    #[cfg_attr(test, test)]
    pub(crate) fn account_from_no_postings() {
        let t = AccountTree::from_postings([]);
        let empty: AccountTree<BalanceData> =
            AccountTree::from_names("root", PeriodData::default(), [], |_| PeriodData::default());
        assert_eq!(t, empty);
        assert!(t.accounts.iter().all(|a| a.data.periods.is_empty()));
    }

    // The subaccounts' balances are summed from the last, as foldMap nests
    // them, which decides the places of a sum passing through zero.
    #[test]
    fn account_tree_sums_from_the_last_subaccount() {
        let m = |s: &str| MixedAmount::from_amount(usd(s.parse().unwrap()));
        let (a, b, c) = (m("1000.50"), m("-1000.50"), m("1"));
        let t = AccountTree::from_postings([(None, "a", &a), (None, "b", &b), (None, "c", &c)]);
        assert_eq!(t.root().data.pre.incl, m("1.00"));
        assert_eq!(t.root().data.pre.incl.amounts()[0].quantity.places(), 2);
    }

    #[test]
    fn account_tree_sums() {
        let (a, b, c) = (usds(1), usds(2), usds(4));
        let jan = Some(day(2024, 1, 1));
        let t = AccountTree::from_postings([
            (jan, "a:b", &a),
            (None, "a:b:c", &b),
            (jan, "a:d", &c),
            (jan, "a:b", &a),
        ]);
        let names: Vec<&str> = t.accounts.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["root", "a", "a:b", "a:b:c", "a:d"]);
        assert_eq!(t.accounts[1].subs, [2, 4]);
        let ab = &t.accounts[2].data;
        assert_eq!(ab.periods[&day(2024, 1, 1)], bd(2, 2, 2));
        assert_eq!(ab.pre, bd(0, 2, 0));
        let root = &t.root().data;
        assert_eq!(root.periods[&day(2024, 1, 1)], bd(0, 6, 0));
        assert_eq!(root.pre.incl, usds(2));
        assert!(t.any(ROOT, &|a| &*a.name == "a:d"));
        assert!(!t.any(2, &|a| &*a.name == "a:d"));
    }

    #[test]
    fn merging_trees() {
        let one = |v: i64| PeriodData::from_list(v, [(day(2024, 1, 1), v)]);
        let t1 = AccountTree::from_names("root", one(0), ["b", "a:x"], |_| one(1));
        let mut t2 = AccountTree::from_names("root", one(0), ["c", "a"], |_| one(2));
        t2.accounts[1].boring = true;
        let m = t1.merge(&t2);
        let names: Vec<&str> = m.accounts.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["root", "a", "a:x", "b", "c"]);
        let a = &m.accounts[1];
        assert_eq!(a.data.pre, (Some(1), Some(2)));
        assert!(!a.boring);
        assert_eq!(m.accounts[4].data.pre, (None, Some(2)));
    }

    #[test]
    fn sorting_trees() {
        let mut t: AccountTree<BalanceData> =
            AccountTree::from_names("root", PeriodData::default(), ["b", "a", "c"], |_| {
                PeriodData::default()
            });
        t.accounts[3].declared = Some(1);
        t.sort_by_declaration();
        let order: Vec<&str> = t
            .root()
            .subs
            .iter()
            .map(|&i| &*t.accounts[i].name)
            .collect();
        assert_eq!(order, ["c", "a", "b"]);
        let changes = PeriodData::from_list(bd(1, 1, 1), [(day(2024, 1, 1), bd(2, 2, 1))]);
        let hist = changes.cumulative_sum();
        assert_eq!(hist.periods[&day(2024, 1, 1)], bd(3, 3, 2));
        // a change keeps the running total's posting count
        let back = hist.period_changes();
        assert_eq!(back.pre, bd(1, 1, 1));
        assert_eq!(back.periods[&day(2024, 1, 1)], bd(2, 2, 2));
    }
}
