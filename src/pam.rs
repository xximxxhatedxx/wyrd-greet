//! Hardened PAM authentication implementation for `--mode=lock`.
//!
//! Performs synchronous `pam_start` -> `pam_authenticate` -> `pam_acct_mgmt` -> `pam_end`
//! on a dedicated worker thread while zeroizing all intermediate password buffers.

use std::ffi::CString;
use zeroize::Zeroize;

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct PamMessage {
    pub(crate) msg_style: libc::c_int,
    pub(crate) msg: *const libc::c_char,
}

#[repr(C)]
pub(crate) struct PamResponse {
    pub(crate) resp: *mut libc::c_char,
    pub(crate) resp_retcode: libc::c_int,
}

#[repr(C)]
struct PamConv {
    conv: extern "C" fn(
        num_msg: libc::c_int,
        msg: *mut *const PamMessage,
        resp: *mut *mut PamResponse,
        appdata_ptr: *mut libc::c_void,
    ) -> libc::c_int,
    appdata_ptr: *mut libc::c_void,
}

extern "C" {
    fn pam_start(
        service_name: *const libc::c_char,
        user: *const libc::c_char,
        pam_conversation: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> libc::c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: libc::c_int) -> libc::c_int;
    fn pam_acct_mgmt(pamh: *mut PamHandle, flags: libc::c_int) -> libc::c_int;
    fn pam_end(pamh: *mut PamHandle, pam_status: libc::c_int) -> libc::c_int;
}

pub(crate) const PAM_PROMPT_ECHO_OFF: libc::c_int = 1;
pub(crate) const PAM_PROMPT_ECHO_ON: libc::c_int = 2;
#[cfg(test)]
pub(crate) const PAM_TEXT_INFO: libc::c_int = 4;
pub(crate) const PAM_SUCCESS: libc::c_int = 0;
pub(crate) const PAM_CONV_ERR: libc::c_int = 19;
pub(crate) const PAM_BUF_ERR: libc::c_int = 5;

/// Zeroizes and frees any `resp` strings already allocated in `responses[0..allocated_count]`,
/// then frees the `responses` array itself.
///
/// # Safety
/// `responses` must be a valid pointer returned by `libc::calloc(num_msg, size_of::<PamResponse>())`,
/// and `allocated_count` must be `<= num_msg as usize`. Every non-null `resp` pointer in
/// `0..allocated_count` must have been allocated via `libc::strdup`.
unsafe fn free_partial_responses(responses: *mut PamResponse, allocated_count: usize) {
    for j in 0..allocated_count {
        let r_ptr = (*responses.add(j)).resp;
        if !r_ptr.is_null() {
            let len = libc::strlen(r_ptr);
            std::ptr::write_bytes(r_ptr as *mut u8, 0, len);
            libc::free(r_ptr as *mut libc::c_void);
        }
    }
    libc::free(responses as *mut libc::c_void);
}

/// PAM conversation callback invoked synchronously by `libpam` during `pam_authenticate`.
pub(crate) extern "C" fn pam_conversation_fn(
    num_msg: libc::c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut libc::c_void,
) -> libc::c_int {
    if num_msg <= 0 || appdata_ptr.is_null() || msg.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }

    // # Safety
    // - `appdata_ptr` was initialized in `authenticate_user` from `&password as *const String`,
    //   which remains pinned on the stack frame of `authenticate_user` for the entire synchronous
    //   duration of `pam_authenticate(pamh, 0)`. `pam_conversation_fn` is never called after
    //   `pam_authenticate` returns.
    // - `msg` points to an array of `num_msg` `*const PamMessage` pointers supplied by `libpam`.
    // - `resp` is a non-null out-pointer where `libpam` takes ownership of the `calloc`-allocated
    //   `PamResponse` array on `PAM_SUCCESS`.
    unsafe {
        let password = &*(appdata_ptr as *const String);
        let responses =
            libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if responses.is_null() {
            return PAM_BUF_ERR;
        }

        for i in 0..(num_msg as usize) {
            let m = *msg.add(i);
            if m.is_null() {
                free_partial_responses(responses, i);
                return PAM_CONV_ERR;
            }
            let style = (*m).msg_style;
            if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
                let Ok(c_str) = CString::new(password.as_str()) else {
                    free_partial_responses(responses, i);
                    return PAM_CONV_ERR;
                };
                let r = libc::strdup(c_str.as_ptr());
                // Zeroize the intermediate CString buffer immediately after duplicating for libpam.
                let mut bytes = c_str.into_bytes_with_nul();
                bytes.zeroize();

                if r.is_null() {
                    free_partial_responses(responses, i);
                    return PAM_BUF_ERR;
                }
                (*responses.add(i)).resp = r;
                (*responses.add(i)).resp_retcode = 0;
            }
        }
        *resp = responses;
    }
    PAM_SUCCESS
}

/// Authenticates `username` against the given PAM `service` and verifies account validity
/// via `pam_acct_mgmt`. Zeroizes `password` before returning on every code path.
pub fn authenticate_user(service: &str, username: &str, mut password: String) -> bool {
    let Ok(service_c) = CString::new(service) else {
        password.zeroize();
        return false;
    };
    let Ok(user_c) = CString::new(username) else {
        password.zeroize();
        return false;
    };

    let conv = PamConv {
        conv: pam_conversation_fn,
        appdata_ptr: &password as *const String as *mut libc::c_void,
    };

    let mut pamh: *mut PamHandle = std::ptr::null_mut();

    // # Safety
    // `service_c` and `user_c` are valid NUL-terminated CStrings, `conv` lives on the stack
    // outliving `pam_end`, and `pamh` is a valid out-pointer.
    let start_res = unsafe { pam_start(service_c.as_ptr(), user_c.as_ptr(), &conv, &mut pamh) };
    if start_res != PAM_SUCCESS || pamh.is_null() {
        password.zeroize();
        return false;
    }

    // # Safety
    // `pamh` is a valid PAM handle returned by `pam_start`. `conv.appdata_ptr` points to
    // `password`, which remains alive and unmutated on this stack frame until `pam_authenticate`
    // returns.
    let auth_res = unsafe { pam_authenticate(pamh, 0) };

    // # Safety
    // If authentication succeeded, verify account/password-expiry policies via `pam_acct_mgmt`
    // before ending the PAM transaction.
    let final_status = if auth_res == PAM_SUCCESS {
        unsafe { pam_acct_mgmt(pamh, 0) }
    } else {
        auth_res
    };

    // # Safety
    // `pamh` was initialized by `pam_start` and is terminated exactly once here.
    unsafe {
        pam_end(pamh, final_status);
    }
    password.zeroize();

    final_status == PAM_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pam_conversation_rejects_non_positive_num_msg() {
        let pw = String::from("secret");
        let msg = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: std::ptr::null(),
        };
        let mut msg_ptr: *const PamMessage = &msg;
        let mut resp_ptr: *mut PamResponse = std::ptr::null_mut();

        assert_eq!(
            pam_conversation_fn(
                0,
                &mut msg_ptr,
                &mut resp_ptr,
                &pw as *const String as *mut libc::c_void,
            ),
            PAM_CONV_ERR
        );
        assert_eq!(
            pam_conversation_fn(
                -3,
                &mut msg_ptr,
                &mut resp_ptr,
                &pw as *const String as *mut libc::c_void,
            ),
            PAM_CONV_ERR
        );
    }

    #[test]
    fn test_pam_conversation_rejects_null_pointers() {
        let pw = String::from("secret");
        let msg = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: std::ptr::null(),
        };
        let mut msg_ptr: *const PamMessage = &msg;
        let mut resp_ptr: *mut PamResponse = std::ptr::null_mut();

        // Null msg array pointer
        assert_eq!(
            pam_conversation_fn(
                1,
                std::ptr::null_mut(),
                &mut resp_ptr,
                &pw as *const String as *mut libc::c_void,
            ),
            PAM_CONV_ERR
        );
        // Null resp out-pointer
        assert_eq!(
            pam_conversation_fn(
                1,
                &mut msg_ptr,
                std::ptr::null_mut(),
                &pw as *const String as *mut libc::c_void,
            ),
            PAM_CONV_ERR
        );
        // Null appdata_ptr
        assert_eq!(
            pam_conversation_fn(1, &mut msg_ptr, &mut resp_ptr, std::ptr::null_mut()),
            PAM_CONV_ERR
        );
        // Null inner PamMessage pointer
        let mut null_inner: *const PamMessage = std::ptr::null();
        assert_eq!(
            pam_conversation_fn(
                1,
                &mut null_inner,
                &mut resp_ptr,
                &pw as *const String as *mut libc::c_void,
            ),
            PAM_CONV_ERR
        );
    }

    #[test]
    fn test_pam_conversation_echo_off_duplicates_password_and_ignores_info() {
        let pw = String::from("correct-horse-battery");
        let m1 = PamMessage {
            msg_style: PAM_TEXT_INFO,
            msg: std::ptr::null(),
        };
        let m2 = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: std::ptr::null(),
        };
        let mut msgs: [*const PamMessage; 2] = [&m1, &m2];
        let mut resp_ptr: *mut PamResponse = std::ptr::null_mut();

        let code = pam_conversation_fn(
            2,
            msgs.as_mut_ptr(),
            &mut resp_ptr,
            &pw as *const String as *mut libc::c_void,
        );
        assert_eq!(code, PAM_SUCCESS);
        assert!(!resp_ptr.is_null());

        // # Safety
        // `resp_ptr` was allocated with `calloc(2, size_of::<PamResponse>())` by `pam_conversation_fn`.
        unsafe {
            assert!((*resp_ptr.add(0)).resp.is_null());
            assert!(!(*resp_ptr.add(1)).resp.is_null());
            let c_str = std::ffi::CStr::from_ptr((*resp_ptr.add(1)).resp);
            assert_eq!(c_str.to_str().unwrap(), "correct-horse-battery");
            free_partial_responses(resp_ptr, 2);
        }
    }
}
