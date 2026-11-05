#![allow(unsafe_code, reason = "drives the C ABI")]

use std::ffi::{CStr, CString};
use std::sync::Mutex;

use coblet::auth::{AccountState, AccountUpdate, AuthFlow, PlayerProfile, callback_channel};
use coblet::runtime::{AuthPrompt, CancellationToken, HttpUrl, UserAgent};
use coblet::types::{ExtensionId, PlayerName};

use super::{
    CobletLogin, coblet_login_cancel, coblet_login_finish, coblet_login_free,
    coblet_login_prompt_json, coblet_login_submit_redirect,
};
use crate::context::{coblet_error_free, coblet_string_free};
use crate::types::{CobletError, CobletErrorKind, CobletStatus, CobletString};

fn kind(err: *mut CobletError) -> CobletErrorKind {
    assert!(!err.is_null());
    // SAFETY: the error was just returned and is freed once.
    unsafe {
        let kind = (*err).kind;
        coblet_error_free(err);
        kind
    }
}

fn take(value: CobletString) -> serde_json::Value {
    // SAFETY: the string was just returned and is freed once.
    unsafe {
        let text = CStr::from_ptr(value.ptr).to_str().unwrap().to_owned();
        coblet_string_free(value);
        serde_json::from_str(&text).unwrap()
    }
}

/// Builds a login handle around `flow` with the offline method, which
/// saves whatever account update the flow returns.
fn login(
    runtime: &tokio::runtime::Runtime,
    dir: &tempfile::TempDir,
    flow: AuthFlow,
) -> *mut CobletLogin {
    let coblet = coblet::Coblet::builder()
        .data_root(dir.path())
        .user_agent(UserAgent::new("coblet-ffi-test/1.0").unwrap())
        .build()
        .unwrap();
    Box::into_raw(Box::new(CobletLogin {
        runtime: runtime.handle().clone(),
        coblet,
        method: ExtensionId::new("coblet:offline").unwrap(),
        prompt: flow.prompt().clone(),
        callback: flow.callback().cloned(),
        flow: Mutex::new(Some(flow)),
        cancel: CancellationToken::new(),
    }))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn logins_take_a_redirect_and_finish_once() {
    let runtime = runtime();
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = callback_channel();
    let prompt = AuthPrompt::OpenUrl {
        url: HttpUrl::new("https://login.example/authorize").unwrap(),
        redirect_uri: "myapp://auth".to_owned(),
    };
    let flow = AuthFlow::new(prompt, async move {
        assert_eq!(receiver.recv().await?, "myapp://auth?code=c&state=s");
        let profile = PlayerProfile::new(
            PlayerName::new("Notch").unwrap(),
            "069a79f444e94726a5befca90e38aaf5",
        )
        .unwrap();
        Ok(AccountUpdate::new(AccountState::Ready).with_profile(profile))
    })
    .with_callback(sender);
    let login = login(&runtime, &dir, flow);
    let mut out = CobletString::empty();
    let mut err = std::ptr::null_mut();
    let uri = CString::new("myapp://auth?code=c&state=s").unwrap();
    // SAFETY: a live login handle, valid strings and out-pointers.
    unsafe {
        assert_eq!(
            coblet_login_prompt_json(login, &raw mut out, &raw mut err),
            CobletStatus::Ok
        );
        let prompt = take(std::mem::replace(&mut out, CobletString::empty()));
        assert_eq!(prompt["type"], "open_url");
        assert_eq!(prompt["redirect_uri"], "myapp://auth");
        assert_eq!(
            coblet_login_submit_redirect(login, uri.as_ptr(), &raw mut err),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_login_submit_redirect(login, uri.as_ptr(), &raw mut err),
            CobletStatus::Error
        );
        assert_eq!(kind(err), CobletErrorKind::InvalidState);
        assert_eq!(
            coblet_login_finish(login, &raw mut out, &raw mut err),
            CobletStatus::Ok
        );
        let account = take(std::mem::replace(&mut out, CobletString::empty()));
        assert_eq!(account["schema"], 1);
        assert_eq!(account["player"]["name"], "Notch");
        assert_eq!(
            coblet_login_finish(login, &raw mut out, &raw mut err),
            CobletStatus::Error
        );
        assert_eq!(kind(err), CobletErrorKind::InvalidState);
        coblet_login_free(login);
    }
}

#[test]
fn cancelled_logins_fail_with_cancelled() {
    let runtime = runtime();
    let dir = tempfile::tempdir().unwrap();
    let flow = AuthFlow::new(AuthPrompt::Nothing, std::future::pending());
    let login = login(&runtime, &dir, flow);
    let address = login as usize;
    let waiter = std::thread::spawn(move || {
        let mut out = CobletString::empty();
        let mut err = std::ptr::null_mut();
        // SAFETY: the handle stays live until the thread is joined.
        let status = unsafe {
            coblet_login_finish(address as *const CobletLogin, &raw mut out, &raw mut err)
        };
        (status, kind(err))
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    // SAFETY: a live handle; cancel may run while another thread finishes.
    unsafe { coblet_login_cancel(login) };
    assert_eq!(
        waiter.join().unwrap(),
        (CobletStatus::Error, CobletErrorKind::Cancelled)
    );
    let mut err = std::ptr::null_mut();
    let uri = CString::new("x://y").unwrap();
    // SAFETY: a live handle, then freed once.
    unsafe {
        assert_eq!(
            coblet_login_submit_redirect(login, uri.as_ptr(), &raw mut err),
            CobletStatus::Error
        );
        assert_eq!(kind(err), CobletErrorKind::InvalidState);
        coblet_login_free(login);
    }
}
