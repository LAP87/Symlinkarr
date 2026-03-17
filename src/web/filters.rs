//! Custom Askama template filters for the web UI.
//! Askama 0.12 does not include `length` as a built-in filter, so we provide it here.

use askama::Result;

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
pub fn length<C: Len + ?Sized>(val: &C) -> Result<usize> {
    Ok(val.askama_len())
}
