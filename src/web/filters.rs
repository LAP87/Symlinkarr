//! Custom Askama template filters for the web UI.
//! Askama does not include `length` as a built-in filter, so we provide it here.

use askama::{Result, Values};

pub trait Len {
    fn askama_len(&self) -> usize;
}

impl<T> Len for Vec<T> {
    fn askama_len(&self) -> usize {
        self.len()
    }
}

impl<K, V> Len for std::collections::HashMap<K, V> {
    fn askama_len(&self) -> usize {
        self.len()
    }
}

impl Len for str {
    fn askama_len(&self) -> usize {
        self.len()
    }
}

impl Len for String {
    fn askama_len(&self) -> usize {
        self.len()
    }
}

impl<T> Len for [T] {
    fn askama_len(&self) -> usize {
        self.len()
    }
}

/// Returns the length of a collection.
/// Used in templates as `{{ collection|length }}`.
#[askama::filter_fn]
pub fn length<C: Len + ?Sized>(val: &C, _: &dyn Values) -> Result<usize> {
    Ok(val.askama_len())
}

/// Returns the current application version from Cargo metadata.
/// Used in templates as `{{ ""|app_version }}`.
#[askama::filter_fn]
pub fn app_version<T: ?Sized>(_: &T, _: &dyn Values) -> Result<&'static str> {
    Ok(env!("CARGO_PKG_VERSION"))
}
