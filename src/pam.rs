//! The lock screen's password check (E6.4): libpam through a few lines of FFI, with our service file
//! `/etc/pam.d/noobsbot-desktop` (installed by install.sh; it includes pam_faillock, so guessing is throttled).
//! Blocking (faillock and fingerprint readers take seconds): call it on a worker thread.

use std::{
    ffi::{CString, c_char, c_int, c_void},
    ptr,
};

/// Our service; `system-auth` (what ours includes) when it isn't installed, so the screen can still be unlocked.
/// Not `login`: that stack can carry pam_securetty, which refuses without a tty.
const SERVICE: &str = "noobsbot-desktop";
const FALLBACK: &str = "system-auth";

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;

#[repr(C)]
struct Message {
    style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct Response {
    resp: *mut c_char,
    retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const Message, *mut *mut Response, *mut c_void) -> c_int;

#[repr(C)]
struct Conv {
    conv: ConvFn,
    appdata: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const Conv, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}

/// Answers every hidden prompt with the password (`appdata`, a NUL-terminated string); echoed prompts (a user name),
/// info and error messages get none, so the password never goes where a name is asked. The responses are malloc'd:
/// PAM frees them.
unsafe extern "C" fn conversation(n: c_int, msgs: *mut *const Message, out: *mut *mut Response, appdata: *mut c_void) -> c_int {
    let Ok(count) = usize::try_from(n) else { return PAM_CONV_ERR };
    if count == 0 || msgs.is_null() || out.is_null() {
        return PAM_CONV_ERR;
    }
    // SAFETY: PAM passes `n` message pointers (Linux-PAM's layout) and wants `n` responses it frees with free().
    unsafe {
        let responses = libc::calloc(count, size_of::<Response>()).cast::<Response>();
        if responses.is_null() {
            return PAM_BUF_ERR;
        }
        for i in 0..count {
            let msg = *msgs.add(i);
            if !msg.is_null() && (*msg).style == PAM_PROMPT_ECHO_OFF {
                (*responses.add(i)).resp = libc::strdup(appdata.cast::<c_char>());
            }
        }
        *out = responses;
    }
    PAM_SUCCESS
}

/// Whether `password` is `user`'s, by PAM's auth and account stacks.
pub fn authenticate(user: &str, password: &str) -> bool {
    // An empty name makes PAM ask for one, and nothing here may answer that.
    if user.is_empty() {
        return false;
    }
    let service = if std::path::Path::new("/etc/pam.d").join(SERVICE).exists() { SERVICE } else { FALLBACK };
    let (Ok(service), Ok(user), Ok(secret)) = (CString::new(service), CString::new(user), CString::new(password)) else { return false };
    let mut secret = secret.into_bytes_with_nul();
    let conv = Conv { conv: conversation, appdata: secret.as_mut_ptr().cast() };
    let mut handle = ptr::null_mut();
    // SAFETY: every pointer outlives the PAM transaction, which ends in pam_end below.
    let ok = unsafe {
        let mut status = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
        if status == PAM_SUCCESS {
            status = pam_authenticate(handle, 0);
            if status == PAM_SUCCESS {
                status = pam_acct_mgmt(handle, 0);
            }
            pam_end(handle, status);
        }
        status == PAM_SUCCESS
    };
    secret.fill(0);
    ok
}

/// The user running the desktop: login name and full name (the GECOS field up to its first comma, else the login).
pub fn user() -> (String, String) {
    // SAFETY: getpwuid's result is read at once, on this thread, before any other passwd call.
    let (login, gecos) = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return (String::new(), String::new());
        }
        let text = |p: *const c_char| if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() };
        (text((*pw).pw_name), text((*pw).pw_gecos))
    };
    let full = gecos.split(',').next().unwrap_or_default().trim().to_owned();
    let full = if full.is_empty() { login.clone() } else { full };
    (login, full)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real PAM, but for a user that doesn't exist: it must say no (and touches no real account's faillock tally).
    #[test]
    fn unknown_user_is_refused() {
        assert!(!authenticate("noobsbot-no-such-user", "bogus"));
        assert!(!authenticate("nul\0user", "x"), "an interior NUL never reaches PAM");
        assert!(!authenticate("", "root"), "no name: PAM must not be asked, it would prompt for one");
    }

    #[test]
    fn current_user_has_a_name() {
        let (login, full) = user();
        assert!(!login.is_empty() && !full.is_empty());
    }
}
