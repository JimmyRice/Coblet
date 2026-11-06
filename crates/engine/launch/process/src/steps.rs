use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use coblet_java::{InstallSource, JavaContext, find_or_install, probe};
use coblet_launch_command::{
    DEFAULT_ARGFILE_THRESHOLD, EnvChange, JavaExecutable, LaunchCommand, LaunchOptions, TemplateArg,
};
use coblet_mojang::{AssetIndex, VersionTrait};
use coblet_platform::EffectiveLaunchProfile;
use coblet_runtime::{BoxFuture, Event, ProcessSpec, TransferKind, TransferProgress};
use coblet_types::OsFamily;

use crate::files::{
    FileSet, asset_tasks, by_name_directory, check_existing, copy_by_name, fetch_all,
};
use crate::natives::extract_natives;
use crate::session::Monitor;
use crate::{
    GameLayout, GameSession, LaunchContext, LaunchError, LaunchRequest, LaunchStep, LogBuffer,
    command_profile,
};

macro_rules! step {
    ($(#[$doc:meta])* $ty:ident, $name:literal, infallible $run:ident) => {
        step!($(#[$doc])* $ty, $name, |cx| {
            $run(cx);
            std::future::ready(Ok(()))
        });
    };
    ($(#[$doc:meta])* $ty:ident, $name:literal, sync $run:ident) => {
        step!($(#[$doc])* $ty, $name, |cx| std::future::ready($run(cx)));
    };
    ($(#[$doc:meta])* $ty:ident, $name:literal, $run:ident) => {
        step!($(#[$doc])* $ty, $name, |cx| $run(cx));
    };
    ($(#[$doc:meta])* $ty:ident, $name:literal, |$cx:ident| $body:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $ty;

        impl LaunchStep for $ty {
            fn name(&self) -> &str {
                $name
            }

            fn run<'a>(&'a self, $cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
                Box::pin($body)
            }
        }
    };
}

step!(
    /// Uses the configured Java after checking it, or finds or installs one that fits.
    SelectJava,
    "select-java",
    select_java
);
step!(
    /// Downloads missing or damaged game files and prepares assets.
    VerifyFiles,
    "verify-files",
    verify_files
);
step!(
    /// Extracts native jars into the natives directory.
    PrepareNatives,
    "prepare-natives",
    prepare_natives
);
step!(
    /// Chooses the client jar. Jar mods are merged here once instances support them.
    ClientJar,
    "client-jar",
    infallible client_jar
);
step!(
    /// Places the libraries old FML downloads at start; the Forge loader provides them.
    LegacyFmlLibs,
    "legacy-fml-libs",
    infallible legacy_fml_libs
);
step!(
    /// Adds the version's log4j configuration so the game logs structured events.
    LoggingConfig,
    "logging-config",
    infallible logging_config
);
step!(
    /// Runs the pre-launch command; a failure stops the launch.
    PreLaunch,
    "pre-launch",
    pre_launch
);
step!(
    /// Builds the launch command.
    BuildCommand,
    "build-command",
    sync build_command
);
step!(
    /// Starts the game, under the wrapper command when one is set, and monitors it.
    Spawn,
    "spawn",
    spawn
);

/// Returns the built-in steps in order.
#[must_use]
pub fn builtin_steps() -> Vec<Arc<dyn LaunchStep>> {
    vec![
        Arc::new(SelectJava),
        Arc::new(VerifyFiles),
        Arc::new(PrepareNatives),
        Arc::new(ClientJar),
        Arc::new(LegacyFmlLibs),
        Arc::new(LoggingConfig),
        Arc::new(PreLaunch),
        Arc::new(BuildCommand),
        Arc::new(Spawn),
    ]
}

async fn select_java(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let services = &cx.services;
    let requirement = &cx.profile.java;
    let install = if let Some(path) = &cx.request.java {
        let install = probe(&*services.spawner, path, InstallSource::Configured).await?;
        if install.major != requirement.major || install.arch != requirement.arch {
            return Err(LaunchError::JavaMismatch {
                executable: path.clone(),
                found_major: install.major,
                found_arch: install.arch.as_str().to_owned(),
                major: requirement.major,
                arch: requirement.arch.as_str().to_owned(),
            });
        }
        install
    } else {
        let progress = TransferProgress::new();
        let java = JavaContext {
            progress: Some(&progress),
            storage: &*services.storage,
            spawner: &*services.spawner,
            os: &services.os,
            search: &services.java_search,
            installer: services.java_installer.as_ref(),
            catalog: &services.catalog,
        };
        let install = find_or_install(java, requirement, &cx.cancel);
        let events = services.events.as_ref();
        let op = cx.request.operation;
        progress
            .track(events, op, TransferKind::JavaRuntime, install)
            .await?
    };
    tracing::info!(java = %install.executable.display(), version = %install.version, "selected java");
    cx.java = Some(install);
    Ok(())
}

async fn verify_files(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let services = &cx.services;
    let storage = &*services.storage;
    let (events, op) = (services.events.as_ref(), cx.request.operation);
    let set = FileSet::for_profile(&cx.profile, &cx.layout, &services.catalog).await?;
    check_existing(storage, &set.existing).await?;
    let progress = TransferProgress::new();
    let fetch = fetch_all(
        &services.downloader,
        set.tasks,
        cx.request.verify,
        &cx.cancel,
        &progress,
    );
    progress
        .track(events, op, TransferKind::GameFiles, fetch)
        .await?;
    let Some(reference) = &cx.profile.asset_index else {
        return Ok(());
    };
    let bytes = storage.read(&cx.layout.asset_index(&reference.id)).await?;
    let index = AssetIndex::from_slice(&bytes).map_err(|error| {
        LaunchError::InvalidMetadata(format!("asset index {}: {error}", reference.id))
    })?;
    let tasks = asset_tasks(&index, &cx.layout)?;
    let progress = TransferProgress::new();
    let fetch = fetch_all(
        &services.downloader,
        tasks,
        cx.request.verify,
        &cx.cancel,
        &progress,
    );
    progress
        .track(events, op, TransferKind::Assets, fetch)
        .await?;
    if let Some(base) = by_name_directory(&index, &cx.layout, &reference.id) {
        copy_by_name(storage, &index, &cx.layout, &base).await?;
        cx.game_assets = Some(base);
    }
    Ok(())
}

async fn prepare_natives(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let dir = cx.layout.natives(&cx.profile.game_version);
    extract_natives(
        &*cx.services.storage,
        &cx.layout,
        cx.profile.native_jars(),
        &dir,
    )
    .await?;
    cx.natives_directory = Some(dir);
    Ok(())
}

fn client_jar(cx: &mut LaunchContext) {
    if cx.profile.client.is_some() && cx.client_jar.is_none() {
        cx.client_jar = Some(cx.layout.client_jar(&cx.profile.game_version));
    }
}

fn legacy_fml_libs(cx: &mut LaunchContext) {
    if cx.profile.traits.contains(&VersionTrait::LegacyFmlLibs) {
        tracing::warn!(
            "legacy FML libraries are placed by the Forge loader, which is not installed"
        );
    }
}

/// Returns the JVM argument that points the game at its log configuration.
pub(crate) fn logging_argument(
    profile: &EffectiveLaunchProfile,
    layout: &GameLayout,
) -> Option<String> {
    let logging = profile.logging.as_ref()?;
    let path = layout.log_config(&logging.file.id);
    Some(logging.argument.replace("${path}", &path.to_string_lossy()))
}

fn logging_config(cx: &mut LaunchContext) {
    if let Some(argument) = logging_argument(&cx.profile, &cx.layout) {
        cx.jvm_args.push(argument);
    }
}

async fn pre_launch(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let Some(hook) = &cx.request.pre_launch else {
        return Ok(());
    };
    let storage = &*cx.services.storage;
    storage.create_dir_all(cx.layout.game_directory()).await?;
    let process = cx
        .services
        .spawner
        .spawn(hook.spec(cx.layout.game_directory()))?;
    let exit = process.wait().await?;
    if !exit.success() {
        return Err(LaunchError::PreLaunch(exit));
    }
    Ok(())
}

/// What [`launch_options`] combines.
pub(crate) struct OptionsInput<'a> {
    pub(crate) java: &'a Path,
    pub(crate) java_major: u32,
    pub(crate) os: &'a OsFamily,
    pub(crate) profile: &'a EffectiveLaunchProfile,
    pub(crate) layout: &'a GameLayout,
    pub(crate) request: &'a LaunchRequest,
    pub(crate) game_assets: Option<&'a Path>,
    pub(crate) natives_directory: Option<&'a Path>,
    pub(crate) jvm_args: &'a [String],
}

/// Builds the per-launch options of `build-command`.
pub(crate) fn launch_options(input: &OptionsInput<'_>) -> LaunchOptions {
    let OptionsInput {
        java,
        os,
        profile,
        layout,
        request,
        ..
    } = input;
    let mut options = LaunchOptions::new(
        JavaExecutable::new(java),
        request.auth.clone(),
        layout.game_directory(),
    )
    .with_assets_root(layout.assets())
    .with_library_directory(layout.libraries())
    .with_natives_directory(
        input
            .natives_directory
            .map_or_else(|| layout.natives(&profile.game_version), Path::to_path_buf),
    );
    options.game_assets = input.game_assets.map(Path::to_path_buf);
    options.memory = request.memory;
    options.resolution = request.resolution;
    options.quick_play.clone_from(&request.quick_play);
    options.launcher.clone_from(&request.launcher);
    options.classpath_separator =
        Some(if **os == OsFamily::Windows { ";" } else { ":" }.to_owned());
    options.extra_jvm_args = input
        .jvm_args
        .iter()
        .chain(&request.jvm_args)
        .map(|arg| TemplateArg::new(arg.as_str()))
        .collect();
    options.extra_game_args = request
        .game_args
        .iter()
        .map(|arg| TemplateArg::new(arg.as_str()))
        .collect();
    if input.java_major >= 9 {
        let file = layout
            .data_root()
            .join("argfiles")
            .join(format!("{}.txt", profile.game_version.as_str()));
        options.argfile = Some((file, DEFAULT_ARGFILE_THRESHOLD));
    }
    options
}

fn build_command(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let mut profile = command_profile(&cx.profile, &cx.layout);
    if let (Some(jar), Some(_)) = (&cx.client_jar, &cx.profile.client)
        && let Some(last) = profile.classpath.last_mut()
    {
        last.clone_from(jar);
    }
    let java = cx.java.as_ref().ok_or(LaunchError::MissingState {
        step: "build-command",
        needs: "a java selected by select-java",
    })?;
    let options = launch_options(&OptionsInput {
        java: &java.executable,
        java_major: java.major,
        os: &cx.services.os,
        profile: &cx.profile,
        layout: &cx.layout,
        request: &cx.request,
        game_assets: cx.game_assets.as_deref(),
        natives_directory: cx.natives_directory.as_deref(),
        jvm_args: &cx.jvm_args,
    });
    cx.command = Some(LaunchCommand::from_profile(&profile, &options)?);
    Ok(())
}

fn process_spec(command: &LaunchCommand, wrapper: &[OsString]) -> ProcessSpec {
    let mut args: Vec<OsString> = Vec::new();
    let program = match wrapper.split_first() {
        Some((program, rest)) => {
            args.extend(rest.iter().cloned());
            args.push(command.program().as_os_str().to_owned());
            program.into()
        }
        None => command.program().to_path_buf(),
    };
    args.extend(command.args().iter().cloned());
    let mut spec = ProcessSpec::new(program).args(args).cwd(command.cwd());
    for change in command.env() {
        match change {
            EnvChange::Set { name, value } => spec = spec.env(name, value),
            EnvChange::PrependPaths { name, paths } => {
                let inherited = std::env::var_os(name).unwrap_or_default();
                let joined = std::env::join_paths(
                    paths
                        .iter()
                        .cloned()
                        .chain(std::env::split_paths(&inherited)),
                );
                if let Ok(joined) = joined {
                    spec = spec.env(name, joined);
                }
            }
        }
    }
    spec
}

async fn spawn(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let command = cx.command.clone().ok_or(LaunchError::MissingState {
        step: "spawn",
        needs: "a command built by build-command",
    })?;
    let storage = &cx.services.storage;
    storage.create_dir_all(command.cwd()).await?;
    if let Some(file) = command.argfile() {
        if let Some(parent) = file.path.parent() {
            storage.create_dir_all(parent).await?;
        }
        storage
            .write_atomic(&file.path, file.content.clone().into())
            .await?;
    }
    if cx.cancel.is_cancelled() {
        return Err(LaunchError::Cancelled);
    }
    let spec = process_spec(&command, &cx.request.wrapper);
    tracing::info!(command = %command, "starting the game");
    let process = cx.services.spawner.spawn(spec)?;
    let redacting = command.clone();
    let monitor = Monitor {
        logs: Arc::new(LogBuffer::new(cx.request.log_capacity)),
        redact: Arc::new(move |text: &str| redacting.redact(text)),
        events: cx.services.events.clone(),
        storage: Arc::clone(storage),
        spawner: Arc::clone(&cx.services.spawner),
        game_directory: cx.layout.game_directory().to_path_buf(),
        post_exit: cx.request.post_exit.clone(),
        op: cx.request.operation,
    };
    let session = GameSession::start(command, cx.profile.clone(), process, monitor);
    if let Some(events) = cx
        .services
        .events
        .as_ref()
        .filter(|bus| bus.has_subscribers())
    {
        events.publish(Event::GameStarted {
            op: cx.request.operation,
            pid: session.pid(),
        });
    }
    cx.session = Some(session);
    Ok(())
}

#[cfg(test)]
#[path = "../tests/steps.rs"]
mod tests;
