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

// Dead-code allowance: page templates adopt the `thousands` filter in UI overhaul
// phase B; the allow covers the macro-generated filter plumbing until then.
#[allow(dead_code)]
mod thousands_filter {
    use askama::{Result, Values};

    /// Formats an integer with comma digit grouping (e.g. `77177` -> `77,177`).
    /// Used in templates as `{{ count|thousands }}`.
    #[askama::filter_fn]
    pub fn thousands<T: std::fmt::Display + ?Sized>(val: &T, _: &dyn Values) -> Result<String> {
        Ok(group_thousands(&val.to_string()))
    }

    /// Groups the digits of a plain (optionally negative) integer string with commas.
    /// Anything that is not a plain integer is returned unchanged.
    pub(super) fn group_thousands(raw: &str) -> String {
        let (sign, digits) = match raw.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", raw),
        };

        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return raw.to_string();
        }

        let mut out = String::with_capacity(raw.len() + digits.len() / 3);
        out.push_str(sign);

        let offset = digits.len() % 3;
        for (i, ch) in digits.chars().enumerate() {
            if i != 0 && (i + 3 - offset) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }

        out
    }
}

#[allow(unused_imports)] // consumed by templates starting in UI overhaul phase B
pub use thousands_filter::thousands;

#[cfg(test)]
mod tests {
    use super::thousands_filter::group_thousands;

    #[test]
    fn groups_plain_integers() {
        assert_eq!(group_thousands("0"), "0");
        assert_eq!(group_thousands("999"), "999");
        assert_eq!(group_thousands("1000"), "1,000");
        assert_eq!(group_thousands("77177"), "77,177");
        assert_eq!(group_thousands("1234567"), "1,234,567");
    }

    #[test]
    fn groups_negative_integers() {
        assert_eq!(group_thousands("-1"), "-1");
        assert_eq!(group_thousands("-1000"), "-1,000");
        assert_eq!(group_thousands("-1234567"), "-1,234,567");
    }

    #[test]
    fn passes_through_non_integers() {
        assert_eq!(group_thousands(""), "");
        assert_eq!(group_thousands("12.5"), "12.5");
        assert_eq!(group_thousands("n/a"), "n/a");
    }

    #[test]
    fn renders_through_askama() {
        use askama::Template;

        mod filters {
            pub use crate::web::filters::*;
        }

        #[derive(Template)]
        #[template(source = "{{ value|thousands }}", ext = "txt")]
        struct ThousandsTemplate {
            value: i64,
        }

        let rendered = ThousandsTemplate { value: 1234567 }.render().unwrap();
        assert_eq!(rendered, "1,234,567");
    }
}
