use std::ffi::c_char;

use coblet::launch::{GameSession, LaunchRequest, LogItem, LogStream};
use coblet::launch_command::LaunchAuth;
use coblet::types::PlayerName;

use crate::guard::{guard, put};
use crate::sdk::CobletSdk;
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

/// Running game handle.
pub struct CobletGame {
    runtime: tokio::runtime::Handle,
    session: Option<GameSession>,
    logs: LogStream,
}

/// Starts a version offline or in Demo mode.
///
/// `game_dir` may be NULL for `<data_root>/games/<version>`. On success
/// `*out_game` receives a handle to free with `coblet_game_free`.
///
/// # Safety
///
/// `sdk` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_game` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_launch(
    sdk: *const CobletSdk,
    version_id: *const c_char,
    player_name: *const c_char,
    mode: CobletLaunchMode,
    game_dir: *const c_char,
    out_game: *mut *mut CobletGame,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = sdk
                .as_ref()
                .ok_or_else(|| Failure::argument("sdk", "is NULL"))?;
            let version = text(version_id, "version_id")?;
            let name = PlayerName::new(text(player_name, "player_name")?)
                .map_err(|error| Failure::argument("player_name", &error.to_string()))?;
            if out_game.is_null() {
                return Err(Failure::argument("out_game", "is NULL"));
            }
            let game_dir = match optional_text(game_dir, "game_dir")? {
                Some(dir) => dir.into(),
                None => handle.sdk.data_root().join("games").join(version),
            };
            let auth = match mode {
                CobletLaunchMode::Offline => LaunchAuth::offline(name),
                CobletLaunchMode::Demo => LaunchAuth::demo(name),
            };
            let session = handle.runtime.block_on(handle.sdk.launch(
                version,
                game_dir,
                LaunchRequest::new(auth),
            ))?;
            let logs = session.logs();
            let game = Box::new(CobletGame {
                runtime: handle.runtime.handle().clone(),
                session: Some(session),
                logs,
            });
            put(out_game, Box::into_raw(game), "out_game")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Waits for the next log line and returns it as JSON.
///
/// The document is `{"schema":1,"source","level","thread","logger","message"}`.
/// Returns `COBLET_STATUS_END` once the game ended and every line was read.
///
/// # Safety
///
/// `game` must be a live handle not used by another thread; `out_line` must
/// be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_next_log(
    game: *mut CobletGame,
    out_line: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_mut()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            if out_line.is_null() {
                return Err(Failure::argument("out_line", "is NULL"));
            }
            let json = match game.runtime.block_on(game.logs.next()) {
                None => return Ok(CobletStatus::End),
                Some(LogItem::Missed(count)) => serde_json::json!({
                    "schema": 1,
                    "source": "launcher",
                    "level": "WARN",
                    "message": format!("{count} lines were skipped"),
                }),
                Some(LogItem::Line(line)) => serde_json::json!({
                    "schema": 1,
                    "source": format!("{:?}", line.source).to_lowercase(),
                    "level": line.level.as_str(),
                    "thread": line.thread,
                    "logger": line.logger,
                    "message": line.message,
                }),
            };
            put(out_line, CobletString::new(&json.to_string()), "out_line")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Kills the game; `coblet_game_wait` still reports the exit.
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
    match &game.session {
        Some(session) => {
            session.kill();
            CobletStatus::Ok
        }
        None => CobletStatus::Error,
    }
}

/// Waits for the game to end. Can be called once per handle.
///
/// # Safety
///
/// `game` must be a live handle not used by another thread; `out_exit` must
/// be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_game_wait(
    game: *mut CobletGame,
    out_exit: *mut CobletGameExit,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let game = game
                .as_mut()
                .ok_or_else(|| Failure::argument("game", "is NULL"))?;
            if out_exit.is_null() {
                return Err(Failure::argument("out_exit", "is NULL"));
            }
            let session = game.session.take().ok_or_else(|| {
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
