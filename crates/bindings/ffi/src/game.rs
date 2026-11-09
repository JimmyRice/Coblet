use std::ffi::c_char;

use std::str::FromStr as _;

use std::sync::{Mutex, PoisonError};

use coblet::launch::{GameKiller, GameSession, LaunchRequest, LogItem, LogStream};
use coblet::launch_command::LaunchAuth;
use coblet::runtime::{GameLogLine, OperationId};
use coblet::types::{ExtensionId, PlayerName};

use crate::context::Coblet;
use crate::guard::{guard, put};
use crate::types::{
    CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, optional_text, text,
};

/// How the game identifies the player.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletLaunchMode {
    /// Offline: local name and offline UUID.
    Offline = 0,
    /// Demo mode.
    Demo = 1,
}

/// How a game ended.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CobletGameExit {
    /// Whether `code` is set.
    pub has_code: bool,
    /// Exit code of the process.
    pub code: i32,
    /// Whether `coblet_game_kill` ended the game.
    pub killed: bool,
    /// Whether the game probably crashed.
    pub suspected_crash: bool,
    /// Run time in milliseconds.
    pub duration_ms: u64,
}

/// Length of an operation ID buffer: 36 characters and a NUL.
pub const COBLET_OPERATION_ID_LEN: usize = 37;

/// Writes an operation ID with its NUL into a caller's buffer.
///
/// # Safety
///
/// `out` must be NULL or point to `COBLET_OPERATION_ID_LEN` writable bytes.
#[allow(unsafe_code, reason = "writes a caller buffer")]
pub(crate) unsafe fn write_operation_id(op: OperationId, out: *mut c_char) -> Result<(), Failure> {
    if out.is_null() {
        return Err(Failure::argument("out_id", "is NULL"));
    }
    let text = op.to_string();
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.len() + 1, COBLET_OPERATION_ID_LEN);
    // SAFETY: the caller guarantees COBLET_OPERATION_ID_LEN writable bytes;
    // a UUID has 36 bytes, so the NUL fits.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), out, bytes.len());
        out.add(bytes.len()).write(0);
    }
    Ok(())
}

/// Reads an optional operation ID; NULL gives a random one.
///
/// # Safety
///
/// `id` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads a string argument")]
pub(crate) unsafe fn operation_id(id: *const c_char) -> Result<Option<OperationId>, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    let Some(id) = (unsafe { optional_text(id, "operation_id") })? else {
        return Ok(None);
    };
    OperationId::from_str(id)
        .map(Some)
        .map_err(|error| Failure::argument("operation_id", &error.to_string()))
}

/// Describes a log line as `{"schema":1,"source","level","thread",
/// "logger","message","time","raw"}`, `time` in Unix milliseconds or null.
pub(crate) fn log_json(line: &GameLogLine) -> serde_json::Value {
    let time = line
        .time
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
    serde_json::json!({
        "schema": 1,
        "source": format!("{:?}", line.source).to_lowercase(),
        "level": line.level.as_str(),
        "thread": line.thread,
        "logger": line.logger,
        "message": line.message,
        "time": time,
        "raw": line.raw,
    })
}

/// Running game handle.
///
/// Several threads may use one handle at once: `coblet_game_kill` stops
/// the game while another thread blocks in `coblet_game_wait` or
/// `coblet_game_next_log`. Only `coblet_game_free` needs the handle to
/// itself.
pub struct CobletGame {
    op: OperationId,
    runtime: tokio::runtime::Handle,
    report: String,
    session: Mutex<Option<GameSession>>,
    killer: GameKiller,
    logs: tokio::sync::Mutex<LogStream>,
}

/// Starts a version offline or in Demo mode.
///
/// `game_dir` may be NULL for `<data_root>/games/<version>`. On success
/// `*out_game` receives a handle to free with `coblet_game_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_game` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch(
    coblet: *const Coblet,
    version_id: *const c_char,
    player_name: *const c_char,
    mode: CobletLaunchMode,
    game_dir: *const c_char,
    out_game: *mut *mut CobletGame,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees; no customizers.
    unsafe {
        coblet_launch_customized(
            coblet,
            version_id,
            player_name,
            mode,
            game_dir,
            std::ptr::null(),
            0,
            std::ptr::null(),
            out_game,
            out_err,
        )
    }
}

/// Starts a version like `coblet_launch` and runs the customizers whose
/// IDs are listed in `customizers`, in order.
///
/// `operation_id`, from `coblet_generate_operation_id`, is the launch ID
/// its events carry; NULL picks a random one.
///
/// # Safety
///
/// As `coblet_launch`; `customizers` must point to `count` NUL-terminated
/// strings, or be NULL when `count` is 0; `operation_id` must be NULL or
/// NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch_customized(
    coblet: *const Coblet,
    version_id: *const c_char,
    player_name: *const c_char,
    mode: CobletLaunchMode,
    game_dir: *const c_char,
    customizers: *const *const c_char,
    count: usize,
    operation_id: *const c_char,
    out_game: *mut *mut CobletGame,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees; no loader.
    unsafe {
        coblet_launch_loader(
            coblet,
            version_id,
            std::ptr::null(),
            std::ptr::null(),
            player_name,
            mode,
            game_dir,
            customizers,
            count,
            operation_id,
            out_game,
            out_err,
        )
    }
}

/// Starts a version like `coblet_launch_customized` with a Mod Loader
/// version on top: the loader's install recipe is resolved with the game
/// version, its files are downloaded and the game starts with its main
/// class and arguments.
///
/// `loader` and `loader_version` are both NULL, for the plain game, or
/// both set. Unknown loaders and versions fail with
/// `COBLET_ERROR_KIND_NOT_FOUND`, unmet component requirements with
/// `COBLET_ERROR_KIND_UNSUPPORTED`.
///
/// # Safety
///
/// As `coblet_launch_customized`; `loader` and `loader_version` must be
/// NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch_loader(
    coblet: *const Coblet,
    version_id: *const c_char,
    loader: *const c_char,
    loader_version: *const c_char,
    player_name: *const c_char,
    mode: CobletLaunchMode,
    game_dir: *const c_char,
    customizers: *const *const c_char,
    count: usize,
    operation_id: *const c_char,
    out_game: *mut *mut CobletGame,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let version = text(version_id, "version_id")?;
            let choice = crate::loaders::loader_choice(loader, loader_version)?;
            let op = self::operation_id(operation_id)?;
            let name = PlayerName::new(text(player_name, "player_name")?)
                .map_err(|error| Failure::argument("player_name", &error.to_string()))?;
            if out_game.is_null() {
                return Err(Failure::argument("out_game", "is NULL"));
            }
            let game_dir = match optional_text(game_dir, "game_dir")? {
                Some(dir) => dir.into(),
                None => handle.inner.data_root().join("games").join(version),
            };
            let auth = match mode {
                CobletLaunchMode::Offline => LaunchAuth::offline(name),
                CobletLaunchMode::Demo => LaunchAuth::demo(name),
            };
            let mut request = with_customizers(LaunchRequest::new(auth), customizers, count)?;
            if let Some(op) = op {
                request = request.with_operation(op);
            }
            start(
                handle,
                version,
                choice.as_ref(),
                game_dir,
                request,
                out_game,
            )
        })
    }
}

/// Adds the customizers listed in a C array to a request.
///
/// # Safety
///
/// `customizers` must point to `count` NUL-terminated strings, or be NULL
/// when `count` is 0.
#[allow(unsafe_code, reason = "reads a C string array")]
unsafe fn with_customizers(
    mut request: LaunchRequest,
    customizers: *const *const c_char,
    count: usize,
) -> Result<LaunchRequest, Failure> {
    if count == 0 {
        return Ok(request);
    }
    if customizers.is_null() {
        return Err(Failure::argument("customizers", "is NULL"));
    }
    // SAFETY: the caller guarantees `count` readable pointers.
    for &id in unsafe { std::slice::from_raw_parts(customizers, count) } {
        // SAFETY: the caller guarantees each pointer is NUL-terminated.
        let id = ExtensionId::new(unsafe { text(id, "customizer id") }?)
            .map_err(|error| Failure::argument("customizer id", &error.to_string()))?;
        request = request.with_customizer(id);
    }
    Ok(request)
}

/// Launches a request and hands the game handle out.
///
/// # Safety
///
/// `out_game` must be writable.
#[allow(unsafe_code, reason = "writes the game out-parameter")]
unsafe fn start(
    handle: &Coblet,
    version: &str,
    loader: Option<&(ExtensionId, String)>,
    game_dir: std::path::PathBuf,
    request: LaunchRequest,
    out_game: *mut *mut CobletGame,
) -> Result<CobletStatus, Failure> {
    let session = handle.runtime.block_on(async {
        match loader {
            Some((id, chosen)) => {
                handle
                    .inner
                    .launch_with_loader(version, id, chosen, game_dir, request)
                    .await
            }
            None => handle.inner.launch(version, game_dir, request).await,
        }
    })?;
    let logs = session.logs();
    let game = Box::into_raw(Box::new(CobletGame {
        op: session.operation(),
        runtime: handle.runtime.handle().clone(),
        report: session.profile().explain() + &session.placeholders().explain(),
        killer: session.killer(),
        session: Mutex::new(Some(session)),
        logs: tokio::sync::Mutex::new(logs),
    }));
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { put(out_game, game, "out_game") }.inspect_err(|_| {
        // SAFETY: the handle was created above and never handed out.
        drop(unsafe { Box::from_raw(game) });
    })?;
    Ok(CobletStatus::Ok)
}

/// Starts a version with a saved account, renewing its tokens first when
/// needed, and runs the customizers listed in `customizers`, in order.
///
/// `game_dir` may be NULL for `<data_root>/games/<version>`, and
/// `operation_id` NULL for a random launch ID. Fails with
/// `COBLET_ERROR_KIND_NO_LICENSE` for an account that does not own the
/// game and `COBLET_ERROR_KIND_AUTH` when the user has to sign in again.
///
/// # Safety
///
/// As `coblet_launch_customized`; `account_id` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch_account(
    coblet: *const Coblet,
    version_id: *const c_char,
    account_id: *const c_char,
    game_dir: *const c_char,
    customizers: *const *const c_char,
    count: usize,
    operation_id: *const c_char,
    out_game: *mut *mut CobletGame,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let version = text(version_id, "version_id")?;
            let op = self::operation_id(operation_id)?;
            let account = crate::accounts::account_id(account_id, "account_id")?;
            if out_game.is_null() {
                return Err(Failure::argument("out_game", "is NULL"));
            }
            let game_dir = match optional_text(game_dir, "game_dir")? {
                Some(dir) => dir.into(),
                None => handle.inner.data_root().join("games").join(version),
            };
            let request = handle
                .runtime
                .block_on(handle.inner.launch_request(&account))?;
            let mut request = with_customizers(request, customizers, count)?;
            if let Some(op) = op {
                request = request.with_operation(op);
            }
            start(handle, version, None, game_dir, request, out_game)
        })
    }
}

/// Returns the launch report: where every library, argument and the Java
/// requirement came from, including the changes of customizers.
///
/// On success `*out_report` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `game` must be a live handle; `out_report` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_report(
    game: *const CobletGame,
    out_report: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_ref()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            put(out_report, CobletString::new(&game.report), "out_report")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Waits for the next log line and returns it as JSON.
///
/// The document is `{"schema":1,"source","level","thread","logger",
/// "message","time","raw"}` with `time` in Unix milliseconds or null.
/// Returns `COBLET_STATUS_END` once the game ended and every line was read.
///
/// The call blocks until a line arrives, so call it from a thread of your
/// own, never from a coblet callback. `coblet_events_next` delivers the
/// same lines of every game, with a timeout.
///
/// # Safety
///
/// `game` must be a live handle; `out_line` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_next_log(
    game: *const CobletGame,
    out_line: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_ref()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            if out_line.is_null() {
                return Err(Failure::argument("out_line", "is NULL"));
            }
            let next = game
                .runtime
                .block_on(async { game.logs.lock().await.next().await });
            let json = match next {
                None => return Ok(CobletStatus::End),
                Some(LogItem::Missed(count)) => {
                    let message = format!("{count} lines were skipped");
                    serde_json::json!({
                        "schema": 1,
                        "source": "launcher",
                        "level": "WARN",
                        "message": message,
                        "time": null,
                        "raw": message,
                    })
                }
                Some(LogItem::Line(line)) => log_json(&line),
            };
            put(out_line, CobletString::new(&json.to_string()), "out_line")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Writes the launch ID that this game's events carry into `out_id`, a
/// buffer of `COBLET_OPERATION_ID_LEN` bytes.
///
/// # Safety
///
/// `game` must be a live handle; `out_id` must point to
/// `COBLET_OPERATION_ID_LEN` writable bytes; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_operation_id(
    game: *const CobletGame,
    out_id: *mut c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_ref()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            write_operation_id(game.op, out_id)?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Writes a new random operation ID into `out_id`, a buffer of
/// `COBLET_OPERATION_ID_LEN` bytes. Pass it as a launch's `operation_id`
/// to match the launch's events before the launch call returns.
///
/// # Safety
///
/// `out_id` must point to `COBLET_OPERATION_ID_LEN` writable bytes;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_generate_operation_id(
    out_id: *mut c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            write_operation_id(OperationId::random(), out_id)?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Kills the game, also while another thread blocks in `coblet_game_wait`;
/// `coblet_game_wait` still reports the exit. Calling it after the game
/// ended does nothing.
///
/// # Safety
///
/// `game` must be NULL or a live handle.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_kill(game: *const CobletGame) -> CobletStatus {
    // SAFETY: the caller passes NULL or a live handle.
    let Some(game) = (unsafe { game.as_ref() }) else {
        return CobletStatus::InvalidArgument;
    };
    game.killer.kill();
    CobletStatus::Ok
}

/// Waits for the game to end. Can be called once per handle.
///
/// # Safety
///
/// `game` must be a live handle; `out_exit` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_wait(
    game: *const CobletGame,
    out_exit: *mut CobletGameExit,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_ref()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            if out_exit.is_null() {
                return Err(Failure::argument("out_exit", "is NULL"));
            }
            let taken = game
                .session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let session = taken.ok_or_else(|| {
                Failure::new(
                    CobletErrorKind::InvalidState,
                    "the game was already waited for",
                )
            })?;
            let exit = game
                .runtime
                .block_on(session.wait())
                .map_err(|error| Failure::new(CobletErrorKind::Launch, error.to_string()))?;
            let report = CobletGameExit {
                has_code: exit.exit.code.is_some(),
                code: exit.exit.code.unwrap_or_default(),
                killed: exit.killed,
                suspected_crash: exit.suspected_crash,
                duration_ms: u64::try_from(exit.duration.as_millis()).unwrap_or(u64::MAX),
            };
            put(out_exit, report, "out_exit")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees a game handle; NULL is ignored. The game keeps running.
///
/// # Safety
///
/// `game` must be NULL or a handle from `coblet_launch` that is not used afterwards.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_free(game: *mut CobletGame) {
    if !game.is_null() {
        // SAFETY: the handle came from `Box::into_raw` in `coblet_launch`.
        drop(unsafe { Box::from_raw(game) });
    }
}
