//! Work over independent items, on all cores with the `parallel` feature
//! (on by default) when there is enough of it, otherwise one item after
//! another. The results are the same either way, in the items' order.
//!
//! Each function is told how much work the items hold, in transactions,
//! postings or report lines: below `MIN_WORK` they are done on this thread,
//! as starting the threads would cost more than they save, unless the
//! threads have been started already.

#[cfg(feature = "parallel")]
const MIN_WORK: usize = 5000;

/// Whether to use the threads for this much work, starting them if so.
#[cfg(feature = "parallel")]
fn parallel(work: usize) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STARTED: AtomicBool = AtomicBool::new(false);
    if work >= MIN_WORK {
        STARTED.store(true, Ordering::Relaxed);
    }
    STARTED.load(Ordering::Relaxed)
}

/// `f` of each item, in order.
pub fn map<T: Sync, U: Send>(
    work: usize,
    items: &[T],
    f: impl Fn(&T) -> U + Sync + Send,
) -> Vec<U> {
    #[cfg(feature = "parallel")]
    if parallel(work) {
        use rayon::prelude::*;
        return items.par_iter().map(f).collect();
    }
    let _ = work;
    items.iter().map(f).collect()
}

/// `f` of each item, consuming them, in order.
pub fn map_owned<T: Send, U: Send>(
    work: usize,
    items: Vec<T>,
    f: impl Fn(T) -> U + Sync + Send,
) -> Vec<U> {
    #[cfg(feature = "parallel")]
    if parallel(work) {
        use rayon::prelude::*;
        return items.into_par_iter().map(f).collect();
    }
    let _ = work;
    items.into_iter().map(f).collect()
}

/// `f` applied to each item in place.
pub fn for_each_mut<T: Send>(work: usize, items: &mut [T], f: impl Fn(&mut T) + Sync + Send) {
    #[cfg(feature = "parallel")]
    if parallel(work) {
        use rayon::prelude::*;
        return items.par_iter_mut().for_each(f);
    }
    let _ = work;
    items.iter_mut().for_each(f)
}
