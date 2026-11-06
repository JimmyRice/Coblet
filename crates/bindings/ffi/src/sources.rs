use std::ffi::{CString, c_char, c_void};

use coblet::runtime::{ArtifactKind, HttpUrl, UrlRewriter};

use crate::customize::Owned;
use crate::guard::guard;
use crate::types::{CobletStatus, Failure, text};

/// Callback table of an address rewriter written in C, for mirrors that a
/// rule from `coblet_builder_add_mirror` cannot describe.
///
/// `rewrite` runs once per request on the library's runtime threads,
/// possibly several at once, and should return quickly. `kind` names what
/// the request fetches in snake case, such as `library`, `asset` or
/// `metadata`. To change the address the callback passes the new one to
/// `coblet_rewrite_set_url` on `out`, which is valid only during the call;
/// otherwise the address stays. The library owns `user_data` from the call
/// that receives the table, even when it fails, and calls `free` once when
/// the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletUrlRewriter {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Looks at one request address.
    pub rewrite: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            url: *const c_char,
            kind: *const c_char,
            out: *mut CobletRewrite,
        ),
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Result of one `rewrite` callback.
#[derive(Debug, Default)]
pub struct CobletRewrite {
    url: Option<String>,
}

type Rewrite = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut CobletRewrite);

/// C rewriter used as a coblet rewriter.
pub(crate) struct ForeignRewriter {
    rewrite: Rewrite,
    owned: Owned,
}

impl std::fmt::Debug for ForeignRewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignRewriter").finish_non_exhaustive()
    }
}

impl ForeignRewriter {
    /// Takes over a callback table.
    pub(crate) fn new(table: &CobletUrlRewriter) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        let rewrite = table
            .rewrite
            .ok_or_else(|| Failure::argument("url rewriter", "needs a rewrite callback"))?;
        Ok(Self { rewrite, owned })
    }
}

/// Returns the snake case name of a kind, or the name of an `Other` kind.
pub(crate) fn kind_name(kind: &ArtifactKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(map)) => map
            .into_iter()
            .next()
            .and_then(|(_, value)| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

impl UrlRewriter for ForeignRewriter {
    #[allow(unsafe_code, reason = "calls a C rewriter callback")]
    fn rewrite(&self, url: &HttpUrl, kind: &ArtifactKind) -> Option<HttpUrl> {
        let url = CString::new(url.as_str()).ok()?;
        let kind = CString::new(kind_name(kind)).ok()?;
        let mut out = CobletRewrite::default();
        // SAFETY: the table owner promised a callback that accepts its user
        // data, two NUL-terminated strings and the result handle, all valid
        // for the duration of the call.
        unsafe {
            (self.rewrite)(
                self.owned.user_data,
                url.as_ptr(),
                kind.as_ptr(),
                &raw mut out,
            );
        };
        HttpUrl::new(out.url?).ok()
    }
}

/// Parses an optional JSON array of kind names, such as `["library", "metadata"]`.
pub(crate) fn parse_kinds(json: Option<&str>) -> Result<Option<Vec<ArtifactKind>>, Failure> {
    let Some(json) = json else {
        return Ok(None);
    };
    let kinds: Vec<ArtifactKind> = serde_json::from_str(json)
        .map_err(|error| Failure::argument("kinds", &error.to_string()))?;
    Ok((!kinds.is_empty()).then_some(kinds))
}

/// Sets the address a running `rewrite` callback wants instead; the
/// library copies it. An address that is not HTTP or HTTPS is ignored.
///
/// # Safety
///
/// `out` must be the handle passed to the running `rewrite` callback;
/// `url` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_rewrite_set_url(
    out: *mut CobletRewrite,
    url: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            let out = out
                .as_mut()
                .ok_or_else(|| Failure::argument("out", "is NULL"))?;
            out.url = Some(text(url, "url")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

#[cfg(test)]
#[path = "../tests/sources.rs"]
mod tests;
