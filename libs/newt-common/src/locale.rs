//! Locale bootstrap.
//!
//! macOS is the one platform that hands a GUI process no locale at all:
//! launchd exports no `LANG`/`LC_*`, and no login shell supplies one either
//! (there is no `/etc/profile.d` locale hook). Bash then fails `setlocale` on
//! startup and prints a warning per category into every terminal we spawn.
//! Terminal.app, iTerm2 and Ghostty all set `LANG` themselves; so do we.
//!
//! Nothing to do elsewhere: Linux gets `LANG` from pam_env
//! (`/etc/default/locale`, `/etc/environment`) even for a non-login
//! `ssh host cmd`, and Windows has no equivalent.

/// Export `LANG` from the system locale when the environment carries none.
///
/// Only ever `LANG`, never `LC_ALL`. ssh forwards `LC_*` by default
/// (`SendEnv LANG LC_*`) and `LC_ALL` outranks everything on the far side, so
/// exporting it would push our locale onto every remote we connect to and warn
/// there whenever it isn't generated. `LANG` is the lowest-precedence variable
/// and the one distros override via pam_env, so it travels harmlessly.
///
/// Call from `main` before spawning any threads — `set_var` is not thread-safe.
pub fn ensure_locale() {
    if !cfg!(target_os = "macos") {
        return;
    }
    // Launched from a terminal (`cargo dev`), the inherited locale is already
    // the user's own — don't second-guess it.
    let inherited = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    if inherited {
        return;
    }

    #[cfg(target_os = "macos")]
    if let Some(lang) = macos::pick_lang() {
        log::debug!("no locale in environment, exporting LANG={}", lang);
        // Safety: documented as main-before-threads.
        unsafe { std::env::set_var("LANG", lang) };
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString};

    /// First candidate the C library actually recognises: the user's own
    /// region, then `en_US.UTF-8`. CFLocale can hand back identifiers with no
    /// libc counterpart (`zh_Hans_CN`), hence the probe rather than blind trust.
    pub(super) fn pick_lang() -> Option<String> {
        current_locale_id()
            .map(|id| format!("{}.UTF-8", id))
            .filter(|c| libc_knows(c))
            .or_else(|| Some("en_US.UTF-8".to_string()).filter(|c| libc_knows(c)))
    }

    /// `CFLocaleCopyCurrent` identifier, minus any ICU keyword suffix —
    /// `en_US@rg=iezzzz` is a perfectly normal thing for it to return.
    fn current_locale_id() -> Option<String> {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;
        use core_foundation_sys::base::CFRelease;
        use core_foundation_sys::locale::{CFLocaleCopyCurrent, CFLocaleGetIdentifier};

        unsafe {
            let locale = CFLocaleCopyCurrent();
            if locale.is_null() {
                return None;
            }
            // GetIdentifier follows the Get rule; wrap_under_get_rule retains,
            // so the String outlives the CFRelease below.
            let id = CFString::wrap_under_get_rule(CFLocaleGetIdentifier(locale)).to_string();
            CFRelease(locale.cast());

            let id = id.split('@').next().unwrap_or(&id).trim().to_string();
            (!id.is_empty()).then_some(id)
        }
    }

    /// Probe and restore: we want the value for our children, not to move this
    /// process off the C locale (which would change how linked C code formats
    /// and parses numbers).
    fn libc_knows(candidate: &str) -> bool {
        let Ok(candidate) = CString::new(candidate) else {
            return false;
        };
        unsafe {
            let saved = libc::setlocale(libc::LC_ALL, std::ptr::null());
            let saved = (!saved.is_null()).then(|| CStr::from_ptr(saved).to_owned());

            let ok = !libc::setlocale(libc::LC_ALL, candidate.as_ptr()).is_null();

            if let Some(saved) = saved {
                libc::setlocale(libc::LC_ALL, saved.as_ptr());
            }
            ok
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn picks_a_locale_libc_accepts() {
            // Whatever this machine's region is, the result must be usable —
            // handing bash a locale it can't load is the bug we're fixing.
            let lang = pick_lang().expect("macOS always has en_US.UTF-8 to fall back on");
            assert!(lang.ends_with(".UTF-8"), "not UTF-8: {lang}");
            assert!(libc_knows(&lang), "libc rejects {lang}");
        }

        #[test]
        fn nonsense_locale_is_rejected() {
            assert!(!libc_knows("xx_YY.UTF-8"));
        }

        #[test]
        fn probe_restores_process_locale() {
            let before = unsafe { libc::setlocale(libc::LC_ALL, std::ptr::null()) };
            let before = unsafe { CStr::from_ptr(before) }.to_owned();
            libc_knows("en_US.UTF-8");
            let after = unsafe { libc::setlocale(libc::LC_ALL, std::ptr::null()) };
            let after = unsafe { CStr::from_ptr(after) }.to_owned();
            assert_eq!(before, after);
        }

        #[test]
        fn identifier_keyword_suffix_is_stripped() {
            // CFLocale hands back things like `en_US@rg=iezzzz`.
            let id = current_locale_id().expect("a current locale");
            assert!(!id.contains('@'), "keyword suffix survived: {id}");
        }
    }
}
