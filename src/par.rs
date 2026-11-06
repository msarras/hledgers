//! Work over independent items, on all cores with the `parallel` feature
//! (on by default), otherwise one item after another. The results are the
//! same either way, in the items' order.

/// `f` of each item, in order.
pub fn map<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> U + Sync + Send) -> Vec<U> {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        items.par_iter().map(f).collect()
    }
    #[cfg(not(feature = "parallel"))]
    {
        items.iter().map(f).collect()
    }
}

/// `f` of each item, consuming them, in order.
pub fn map_owned<T: Send, U: Send>(items: Vec<T>, f: impl Fn(T) -> U + Sync + Send) -> Vec<U> {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        items.into_par_iter().map(f).collect()
    }
    #[cfg(not(feature = "parallel"))]
    {
        items.into_iter().map(f).collect()
    }
}

/// `f` applied to each item in place.
pub fn for_each_mut<T: Send>(items: &mut [T], f: impl Fn(&mut T) + Sync + Send) {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        items.par_iter_mut().for_each(f)
    }
    #[cfg(not(feature = "parallel"))]
    {
        items.iter_mut().for_each(f)
    }
}
