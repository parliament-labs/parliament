//! The client's locale as psql reads it: psql calls `setlocale(LC_ALL,
//! "")` at startup and its printer takes the numeric conventions from
//! `localeconv()` (`setDecimalLocale`) — the locale its environment
//! names (`LC_ALL`, else `LC_NUMERIC`, else `LANG`). noctua runs where
//! psql would, so its environment's locale is the client's; the shell
//! gets it with each session (`SessionOpened.numeric_locale`) for psql's
//! `\pset numericlocale`. What psql makes of it lives in the PG plane.

use std::ffi::CStr;
use std::sync::OnceLock;

use shared::NumericLocale;

/// The numeric conventions of the locale noctua's environment names —
/// the C locale's (empty) when it names one the system lacks, for any
/// category, as psql's `setlocale(LC_ALL, "")` fails then and leaves
/// psql in C (`LANG=xx_YY` with `LC_NUMERIC=de_DE.UTF-8` prints
/// `1,234,567.5`).
pub fn numeric_locale() -> NumericLocale {
    static LOCALE: OnceLock<NumericLocale> = OnceLock::new();
    LOCALE.get_or_init(|| conventions(c"").unwrap_or_default()).clone()
}

/// `name`'s numeric conventions, as `localeconv()` gives them under it
/// (`""`: the environment's — every category, as `setlocale(LC_ALL,
/// "")` reads it), or None when the system lacks it, or any category
/// the environment names. Read on this thread alone (`uselocale`), so
/// the process's own locale — the C library's number formatting
/// elsewhere in noctua — is left as it is.
#[cfg(unix)]
fn conventions(name: &CStr) -> Option<NumericLocale> {
    let text = |p: *const libc::c_char| if p.is_null() {
        String::new()
    } else {
        // SAFETY: localeconv's strings are NUL-terminated and live until
        // the next localeconv / locale change on this thread.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    };
    // SAFETY: newlocale copies `name`; the locale is installed on this
    // thread only for the localeconv read, then the previous one is
    // restored and the new one freed. lconv's fields are read before
    // either happens.
    unsafe {
        let locale = libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
        if locale.is_null() {
            return None;
        }
        let previous = libc::uselocale(locale);
        let conv = libc::localeconv();
        let read = (!conv.is_null()).then(|| {
            let conv = &*conv;
            NumericLocale {
                decimal_point: text(conv.decimal_point),
                thousands_sep: text(conv.thousands_sep),
                grouping: if conv.grouping.is_null() { 0 } else { i32::from(*conv.grouping) },
            }
        });
        libc::uselocale(previous);
        libc::freelocale(locale);
        read
    }
}

#[cfg(not(unix))]
fn conventions(_name: &CStr) -> Option<NumericLocale> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C library's conventions for locales this machine may have:
    /// each is checked only when installed.
    #[test]
    fn conventions_are_the_c_librarys() {
        assert_eq!(conventions(c"C"), Some(NumericLocale { decimal_point: ".".into(), thousands_sep: String::new(), grouping: 0 }));
        if let Some(german) = conventions(c"de_DE.UTF-8") {
            assert_eq!((german.decimal_point.as_str(), german.thousands_sep.as_str(), german.grouping), (",", ".", 3));
        }
        if let Some(american) = conventions(c"en_US.UTF-8") {
            assert_eq!((american.decimal_point.as_str(), american.thousands_sep.as_str(), american.grouping), (".", ",", 3));
        }
        assert_eq!(conventions(c"xx_NOWHERE.UTF-8"), None);
    }
}
