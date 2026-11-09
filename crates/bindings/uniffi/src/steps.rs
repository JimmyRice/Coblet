use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use coblet::launch::{LaunchContext, LaunchError, PlaceholderOrigin, step_names};
use coblet::runtime::BoxFuture;

use crate::customize::trait_names;
use crate::placeholders::placeholder_name;
use crate::{Coblet, CobletError, ErrorKind};

/// Launch step written in Swift, Kotlin or Python.
///
/// A customizer can add one to a single launch with
/// `ProfileEditor.add_step_before`; `CobletBuilder.add_step_before`,
/// `add_step_after` and `replace_step` put one in every launch. It runs on
/// a worker thread and may block.
#[uniffi::export(foreign)]
pub trait LaunchStep: Send + Sync {
    /// Returns the step name, such as `acme-start-proxy`; read once when
    /// the step is added.
    fn name(&self) -> String;

    /// Runs the step; an error stops the launch.
    ///
    /// What it sets through `cx` takes effect when it returns without an
    /// error.
    ///
    /// # Errors
    ///
    /// The error reaches the caller of the launch as a launch failure.
    fn run(&self, cx: Arc<StepContext>) -> Result<(), CobletError>;
}

/// What a step set, applied to the launch when it returns.
#[derive(Default)]
struct Changes {
    java: Option<String>,
    natives_dir: Option<String>,
    client_jar: Option<String>,
    game_assets: Option<String>,
    jvm_args: Vec<String>,
    placeholders: BTreeMap<String, String>,
}

/// The launch as a step sees it, and what it changed.
struct State {
    game_version: String,
    traits: Vec<String>,
    data_root: String,
    game_dir: String,
    java: Option<String>,
    java_major: Option<u32>,
    natives_dir: Option<String>,
    client_jar: Option<String>,
    game_assets: Option<String>,
    jvm_args: Vec<String>,
    placeholders: BTreeMap<String, String>,
    changes: Changes,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl State {
    fn new(cx: &LaunchContext) -> Self {
        Self {
            game_version: cx.profile.game_version.as_str().to_owned(),
            traits: trait_names(&cx.profile.traits),
            data_root: text(cx.layout.data_root()),
            game_dir: text(cx.layout.game_directory()),
            java: cx.java.as_ref().map(|java| text(&java.executable)),
            java_major: cx.java.as_ref().map(|java| java.major),
            natives_dir: cx.natives_directory.as_deref().map(text),
            client_jar: cx.client_jar.as_deref().map(text),
            game_assets: cx.game_assets.as_deref().map(text),
            jvm_args: cx.jvm_args.clone(),
            placeholders: cx
                .placeholders
                .names()
                .filter_map(|name| {
                    let value = cx.placeholders.get(name)?;
                    Some((name.to_owned(), value.to_owned()))
                })
                .collect(),
            changes: Changes::default(),
        }
    }
}

/// The launch as a [`LaunchStep`] sees it; what the step sets here takes
/// effect for the later steps when it returns without an error.
///
/// Reads include what the step set. It is valid only during `run`;
/// afterwards every method fails with `InvalidState`.
#[derive(uniffi::Object)]
pub struct StepContext {
    state: Mutex<Option<State>>,
}

impl std::fmt::Debug for StepContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepContext").finish_non_exhaustive()
    }
}

impl StepContext {
    fn with<T>(&self, body: impl FnOnce(&mut State) -> T) -> Result<T, CobletError> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let state = guard.as_mut().ok_or_else(|| {
            CobletError::new(
                ErrorKind::InvalidState,
                "the step context is used after run returned",
            )
        })?;
        Ok(body(state))
    }

    fn close(&self) -> Option<Changes> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|state| state.changes)
    }
}

fn path_value(value: String, what: &str) -> Result<String, CobletError> {
    if Path::new(&value).is_absolute() {
        Ok(value)
    } else {
        Err(CobletError::new(
            ErrorKind::InvalidArgument,
            format!("{what} `{value}` must be an absolute path"),
        ))
    }
}

#[uniffi::export]
impl StepContext {
    /// Returns the game version ID.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn game_version(&self) -> Result<String, CobletError> {
        self.with(|state| state.game_version.clone())
    }

    /// Returns the version traits, such as `applet`.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn traits(&self) -> Result<Vec<String>, CobletError> {
        self.with(|state| state.traits.clone())
    }

    /// Returns the directory for shared game files.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn data_root(&self) -> Result<String, CobletError> {
        self.with(|state| state.data_root.clone())
    }

    /// Returns the working directory of the game.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn game_dir(&self) -> Result<String, CobletError> {
        self.with(|state| state.game_dir.clone())
    }

    /// Returns the Java executable, once `select-java` ran or a step set one.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn java(&self) -> Result<Option<String>, CobletError> {
        self.with(|state| state.changes.java.clone().or_else(|| state.java.clone()))
    }

    /// Returns the major version of the Java `select-java` chose; a Java a
    /// step set is checked only after the step returns.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn java_major(&self) -> Result<Option<u32>, CobletError> {
        self.with(|state| state.java_major)
    }

    /// Uses the `java` at `path` for this launch, as a replacement of
    /// `select-java` does. After the step returns coblet runs it and checks
    /// it fits the version; the launch fails otherwise.
    ///
    /// # Errors
    ///
    /// Fails for a relative path and after `run` returned.
    pub fn set_java(&self, path: String) -> Result<(), CobletError> {
        let path = path_value(path, "java")?;
        self.with(|state| state.changes.java = Some(path))
    }

    /// Returns the natives directory, once `prepare-natives` ran or a step set one.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn natives_dir(&self) -> Result<Option<String>, CobletError> {
        self.with(|state| {
            state
                .changes
                .natives_dir
                .clone()
                .or_else(|| state.natives_dir.clone())
        })
    }

    /// Sets the natives directory, as a replacement of `prepare-natives` does.
    ///
    /// # Errors
    ///
    /// Fails for a relative path and after `run` returned.
    pub fn set_natives_dir(&self, path: String) -> Result<(), CobletError> {
        let path = path_value(path, "natives directory")?;
        self.with(|state| state.changes.natives_dir = Some(path))
    }

    /// Returns the client jar, once `client-jar` ran or a step set one.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn client_jar(&self) -> Result<Option<String>, CobletError> {
        self.with(|state| {
            state
                .changes
                .client_jar
                .clone()
                .or_else(|| state.client_jar.clone())
        })
    }

    /// Starts the game from this client jar, as a replacement of
    /// `client-jar` does.
    ///
    /// # Errors
    ///
    /// Fails for a relative path and after `run` returned.
    pub fn set_client_jar(&self, path: String) -> Result<(), CobletError> {
        let path = path_value(path, "client jar")?;
        self.with(|state| state.changes.client_jar = Some(path))
    }

    /// Returns `${game_assets}`, once `verify-files` set it for an old
    /// asset index or a step set it.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn game_assets(&self) -> Result<Option<String>, CobletError> {
        self.with(|state| {
            state
                .changes
                .game_assets
                .clone()
                .or_else(|| state.game_assets.clone())
        })
    }

    /// Sets `${game_assets}`, as a replacement of `verify-files` does for
    /// old asset indexes.
    ///
    /// # Errors
    ///
    /// Fails for a relative path and after `run` returned.
    pub fn set_game_assets(&self, path: String) -> Result<(), CobletError> {
        let path = path_value(path, "game assets")?;
        self.with(|state| state.changes.game_assets = Some(path))
    }

    /// Returns the JVM arguments steps added, such as the log configuration.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn jvm_args(&self) -> Result<Vec<String>, CobletError> {
        self.with(|state| {
            state
                .jvm_args
                .iter()
                .chain(&state.changes.jvm_args)
                .cloned()
                .collect()
        })
    }

    /// Adds a JVM argument after those of the version; it may use `${name}`
    /// placeholders.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn add_jvm_arg(&self, value: String) -> Result<(), CobletError> {
        self.with(|state| state.changes.jvm_args.push(value))
    }

    /// Returns the value of `${name}` set so far, when it has one.
    ///
    /// # Errors
    ///
    /// Fails after `run` returned.
    pub fn placeholder(&self, name: &str) -> Result<Option<String>, CobletError> {
        self.with(|state| {
            state
                .changes
                .placeholders
                .get(name)
                .or_else(|| state.placeholders.get(name))
                .cloned()
        })
    }

    /// Sets `${name}` to `value` for the rest of the launch.
    ///
    /// # Errors
    ///
    /// Fails for a name coblet fills itself, such as `auth_access_token`,
    /// an empty name or one with `$`, `{` or `}`, and after `run` returned.
    pub fn set_placeholder(&self, name: String, value: String) -> Result<(), CobletError> {
        placeholder_name(&name)?;
        self.with(|state| {
            state.changes.placeholders.insert(name, value);
        })
    }
}

/// Runs a foreign step as a Rust launch step.
pub(crate) struct ForeignStep {
    name: String,
    inner: Arc<dyn LaunchStep>,
}

impl ForeignStep {
    pub(crate) fn new(inner: Arc<dyn LaunchStep>) -> Self {
        Self {
            name: inner.name(),
            inner,
        }
    }

    async fn apply(&self, changes: Changes, cx: &mut LaunchContext) -> Result<(), LaunchError> {
        if let Some(java) = changes.java {
            cx.use_java(Path::new(&java)).await?;
        }
        if let Some(dir) = changes.natives_dir {
            cx.natives_directory = Some(PathBuf::from(dir));
        }
        if let Some(jar) = changes.client_jar {
            cx.client_jar = Some(PathBuf::from(jar));
        }
        if let Some(assets) = changes.game_assets {
            cx.game_assets = Some(PathBuf::from(assets));
        }
        cx.jvm_args.extend(changes.jvm_args);
        for (name, value) in changes.placeholders {
            cx.placeholders
                .set(name, value, PlaceholderOrigin::Step(self.name.clone()))?;
        }
        Ok(())
    }
}

impl coblet::launch::LaunchStep for ForeignStep {
    fn name(&self) -> &str {
        &self.name
    }

    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        let context = Arc::new(StepContext {
            state: Mutex::new(Some(State::new(cx))),
        });
        let given = Arc::clone(&context);
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let failed = |message: String| LaunchError::Step {
                step: self.name.clone(),
                message,
            };
            let result = tokio::task::spawn_blocking(move || inner.run(given)).await;
            let changes = context.close();
            result
                .map_err(|error| failed(format!("the step stopped: {error}")))?
                .map_err(|error| failed(error.to_string()))?;
            match changes {
                Some(changes) => self.apply(changes, cx).await,
                None => Ok(()),
            }
        })
    }
}

/// Checks that a step added from this language may replace or remove
/// the step named `name`.
pub(crate) fn replaceable(name: &str) -> Result<(), CobletError> {
    if name == step_names::BUILD_COMMAND || name == step_names::SPAWN {
        return Err(CobletError::new(
            ErrorKind::InvalidArgument,
            format!(
                "step `{name}` makes the command or the game process, which only Rust code can, so it cannot be replaced or removed here"
            ),
        ));
    }
    Ok(())
}

#[uniffi::export]
impl Coblet {
    /// Lists the launch steps in the order they run, such as
    /// `select-java`, with the steps added by the builder.
    #[must_use]
    pub fn launch_steps(&self) -> Vec<String> {
        self.inner
            .launcher()
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use coblet::launch::{LaunchContext, LaunchError, LaunchRequest, Launcher, PlaceholderOrigin};
    use coblet::launch_command::LaunchAuth;
    use coblet::types::PlayerName;

    use super::{ForeignStep, LaunchStep, StepContext, replaceable};
    use crate::testing::launch_context;
    use crate::{CobletBuilder, CobletError, ErrorKind};

    /// A `java` that only answers the version probe.
    const FAKE_JAVA: &str = r#"#!/bin/sh
echo 'Property settings:' >&2
echo '    java.home = /fake/17' >&2
echo "    java.version = $VERSION" >&2
echo '    os.arch = amd64' >&2
"#;

    fn fake_java(dir: &Path, version: &str) -> String {
        let path = dir.join(format!("java-{version}"));
        std::fs::write(&path, FAKE_JAVA.replace("$VERSION", version)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Behaves like a step written in Python that stands in for others.
    struct Script {
        java: String,
        fail: bool,
        kept: Mutex<Option<Arc<StepContext>>>,
    }

    impl LaunchStep for Script {
        fn name(&self) -> String {
            "acme-prepare".to_owned()
        }

        fn run(&self, cx: Arc<StepContext>) -> Result<(), CobletError> {
            assert_eq!(cx.game_version()?, "1.20.1");
            assert_eq!(cx.placeholder("acme_asked")?.as_deref(), Some("yes"));
            cx.set_java(self.java.clone())?;
            assert_eq!(cx.java()?, Some(self.java.clone()));
            assert_eq!(cx.java_major()?, None);
            cx.set_natives_dir("/opt/natives".to_owned())?;
            cx.set_client_jar("/opt/client.jar".to_owned())?;
            cx.set_game_assets("/opt/assets".to_owned())?;
            cx.add_jvm_arg("-Dacme=${acme_port}".to_owned())?;
            cx.set_placeholder("acme_port".to_owned(), "25565".to_owned())?;
            assert_eq!(cx.placeholder("acme_port")?.as_deref(), Some("25565"));
            assert_eq!(cx.jvm_args()?, ["-Dacme=${acme_port}"]);
            let error = cx.set_client_jar("client.jar".to_owned()).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidArgument);
            let error = cx
                .set_placeholder("auth_uuid".to_owned(), "x".to_owned())
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidArgument);
            *self.kept.lock().unwrap() = Some(cx);
            if self.fail {
                return Err(CobletError::new(ErrorKind::Other, "mirror is down"));
            }
            Ok(())
        }
    }

    async fn run(dir: &Path, script: Arc<Script>) -> (Result<(), LaunchError>, LaunchContext) {
        let launcher = Launcher::empty().with_step(Arc::new(ForeignStep::new(script)));
        let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
            .with_placeholder("acme_asked", "yes");
        let mut cx = launch_context(dir, "1.20.1", request);
        let result = launcher.run(&mut cx).await;
        (result, cx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn what_a_step_sets_reaches_the_launch_when_it_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let script = Arc::new(Script {
            java: fake_java(dir.path(), "17.0.2"),
            fail: false,
            kept: Mutex::new(None),
        });
        let (result, cx) = run(dir.path(), Arc::clone(&script)).await;
        result.unwrap();
        assert_eq!(cx.java.as_ref().unwrap().major, 17);
        assert_eq!(
            cx.natives_directory.as_deref(),
            Some(Path::new("/opt/natives"))
        );
        assert_eq!(cx.client_jar.as_deref(), Some(Path::new("/opt/client.jar")));
        assert_eq!(cx.game_assets.as_deref(), Some(Path::new("/opt/assets")));
        assert_eq!(cx.jvm_args, ["-Dacme=${acme_port}"]);
        assert_eq!(cx.placeholders.get("acme_port"), Some("25565"));
        assert_eq!(
            cx.placeholders.origin("acme_port"),
            Some(&PlaceholderOrigin::Step("acme-prepare".to_owned()))
        );
        let kept = script.kept.lock().unwrap().clone().unwrap();
        assert_eq!(kept.game_dir().unwrap_err().kind(), ErrorKind::InvalidState);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_step_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let script = Arc::new(Script {
            java: fake_java(dir.path(), "17.0.2"),
            fail: true,
            kept: Mutex::new(None),
        });
        let (result, cx) = run(dir.path(), script).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "step `acme-prepare` failed: mirror is down"
        );
        assert!(cx.java.is_none() && cx.client_jar.is_none() && cx.jvm_args.is_empty());
        assert_eq!(cx.placeholders.get("acme_port"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_java_that_does_not_fit_fails_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let script = Arc::new(Script {
            java: fake_java(dir.path(), "1.8.0_74"),
            fail: false,
            kept: Mutex::new(None),
        });
        let (result, _) = run(dir.path(), script).await;
        assert!(
            matches!(
                result,
                Err(LaunchError::JavaMismatch { found_major: 8, .. })
            ),
            "{result:?}"
        );
    }

    struct Named(&'static str);

    impl LaunchStep for Named {
        fn name(&self) -> String {
            self.0.to_owned()
        }

        fn run(&self, _cx: Arc<StepContext>) -> Result<(), CobletError> {
            Ok(())
        }
    }

    #[test]
    fn the_builder_adds_replaces_and_removes_steps() {
        let builder = || {
            CobletBuilder::new(
                "/tmp/coblet-uniffi-steps".to_owned(),
                "coblet-uniffi-test/1.0".to_owned(),
            )
            .unwrap()
        };
        let made = builder();
        made.add_step_before("build-command".to_owned(), Arc::new(Named("acme-proxy")))
            .unwrap();
        made.add_step_after("acme-proxy".to_owned(), Arc::new(Named("acme-after")))
            .unwrap();
        made.replace_step("client-jar".to_owned(), Arc::new(Named("acme-jar")))
            .unwrap();
        made.remove_step("logging-config".to_owned()).unwrap();
        for name in ["build-command", "spawn"] {
            assert_eq!(
                made.remove_step(name.to_owned()).unwrap_err().kind(),
                ErrorKind::InvalidArgument
            );
            assert_eq!(
                made.replace_step(name.to_owned(), Arc::new(Named("x")))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidArgument
            );
        }
        let coblet = made.build().unwrap();
        assert_eq!(
            coblet.launch_steps(),
            [
                "select-java",
                "verify-files",
                "prepare-natives",
                "acme-jar",
                "legacy-fml-libs",
                "pre-launch",
                "resolve-placeholders",
                "acme-proxy",
                "acme-after",
                "build-command",
                "spawn"
            ]
        );
        let made = builder();
        made.remove_step("acme-missing".to_owned()).unwrap();
        assert_eq!(made.build().unwrap_err().kind(), ErrorKind::Launch);
        assert!(replaceable("pre-launch").is_ok());
    }
}
