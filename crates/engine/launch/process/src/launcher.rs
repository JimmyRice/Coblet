use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use coblet_launch_command::UnknownPlaceholder;
use coblet_runtime::{Event, EventBus, OperationOutcome, Registry, RegistryError};
use coblet_types::ExtensionId;

use crate::customize::Placement;
use crate::{
    CustomizeCtx, GameSession, LaunchContext, LaunchCustomizer, LaunchError, LaunchStep,
    PlaceholderOrigin, builtin_steps, check_placeholder_name, diff,
};

/// Ordered chain of [`LaunchStep`]s that prepares and starts the game.
///
/// [`Launcher::new`] holds the built-in steps; steps can be inserted,
/// replaced or removed by name. Registered [`LaunchCustomizer`]s run before
/// the first step when the [`LaunchRequest`](crate::LaunchRequest) names
/// them. A customizer may add or remove authentication arguments only
/// after [`Launcher::allow_auth_changes`].
#[derive(Clone)]
pub struct Launcher {
    steps: Vec<Arc<dyn LaunchStep>>,
    customizers: Registry<dyn LaunchCustomizer>,
    auth_grants: BTreeSet<ExtensionId>,
    placeholders: BTreeMap<String, String>,
    unknown: UnknownPlaceholder,
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launcher")
            .field("steps", &self.names())
            .field("customizers", &self.customizers)
            .field("auth_grants", &self.auth_grants)
            .field("placeholders", &self.placeholders.keys())
            .field("unknown", &self.unknown)
            .finish()
    }
}

fn position(steps: &[Arc<dyn LaunchStep>], name: &str) -> Result<usize, LaunchError> {
    steps
        .iter()
        .position(|step| step.name() == name)
        .ok_or_else(|| LaunchError::UnknownStep(name.to_owned()))
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Launcher {
    /// Creates a launcher with the built-in steps.
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: builtin_steps(),
            customizers: Registry::new(),
            auth_grants: BTreeSet::new(),
            placeholders: BTreeMap::new(),
            unknown: UnknownPlaceholder::Error,
        }
    }

    /// Creates a launcher with no steps.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            steps: Vec::new(),
            customizers: Registry::new(),
            auth_grants: BTreeSet::new(),
            placeholders: BTreeMap::new(),
            unknown: UnknownPlaceholder::Error,
        }
    }

    /// Returns the step names in order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.steps.iter().map(|step| step.name()).collect()
    }

    fn position(&self, name: &str) -> Result<usize, LaunchError> {
        position(&self.steps, name)
    }

    /// Appends a step.
    #[must_use]
    pub fn with_step(mut self, step: Arc<dyn LaunchStep>) -> Self {
        self.steps.push(step);
        self
    }

    /// Inserts a step before the one named `name`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownStep`] when no step has that name.
    pub fn insert_before(
        &mut self,
        name: &str,
        step: Arc<dyn LaunchStep>,
    ) -> Result<(), LaunchError> {
        let index = self.position(name)?;
        self.steps.insert(index, step);
        Ok(())
    }

    /// Inserts a step after the one named `name`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownStep`] when no step has that name.
    pub fn insert_after(
        &mut self,
        name: &str,
        step: Arc<dyn LaunchStep>,
    ) -> Result<(), LaunchError> {
        let index = self.position(name)?;
        self.steps.insert(index + 1, step);
        Ok(())
    }

    /// Replaces the step named `name`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownStep`] when no step has that name.
    pub fn replace(&mut self, name: &str, step: Arc<dyn LaunchStep>) -> Result<(), LaunchError> {
        let index = self.position(name)?;
        self.steps[index] = step;
        Ok(())
    }

    /// Removes the step named `name`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownStep`] when no step has that name.
    pub fn remove(&mut self, name: &str) -> Result<(), LaunchError> {
        let index = self.position(name)?;
        self.steps.remove(index);
        Ok(())
    }

    /// Registers a customizer that launch requests can choose by ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when another customizer has the
    /// same ID; the existing one stays in place.
    pub fn register_customizer(
        &mut self,
        customizer: Arc<dyn LaunchCustomizer>,
    ) -> Result<(), RegistryError> {
        self.customizers.register(customizer)
    }

    /// Lets the registered customizer `id` add and remove authentication
    /// arguments, such as `--accessToken` or `${auth_uuid}`.
    ///
    /// Without this, a launch fails with [`LaunchError::AuthArgument`] when
    /// that customizer changes one, so a customizer cannot redirect or
    /// leak the account on its own. Grant it only to customizers the
    /// launcher trusts with the account, such as one for a third-party
    /// login server. The changes still appear in the explanation.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownCustomizer`] when no customizer is
    /// registered as `id`.
    pub fn allow_auth_changes(&mut self, id: ExtensionId) -> Result<(), LaunchError> {
        if self.customizers.get(&id).is_none() {
            return Err(LaunchError::UnknownCustomizer(id));
        }
        self.auth_grants.insert(id);
        Ok(())
    }

    /// Tells whether the customizer `id` may change authentication arguments.
    #[must_use]
    pub fn allows_auth_changes(&self, id: &ExtensionId) -> bool {
        self.auth_grants.contains(id)
    }

    /// Returns the registered customizers.
    #[must_use]
    pub fn customizers(&self) -> &Registry<dyn LaunchCustomizer> {
        &self.customizers
    }

    /// Sets `${name}` to `value` in every launch; the request, customizers
    /// and steps can replace it.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::InvalidPlaceholder`] when coblet fills that
    /// placeholder itself or the name is not usable.
    pub fn set_placeholder(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), LaunchError> {
        let name = name.into();
        check_placeholder_name(&name)?;
        self.placeholders.insert(name, value.into());
        Ok(())
    }

    /// Returns the values set with [`set_placeholder`](Self::set_placeholder).
    #[must_use]
    pub fn placeholders(&self) -> &BTreeMap<String, String> {
        &self.placeholders
    }

    /// Sets what happens to placeholders nothing fills; an error by default.
    pub fn set_unknown_placeholders(&mut self, policy: UnknownPlaceholder) {
        self.unknown = policy;
    }

    /// Returns what happens to placeholders nothing fills.
    #[must_use]
    pub fn unknown_placeholders(&self) -> UnknownPlaceholder {
        self.unknown
    }

    /// Puts the static and requested placeholder values into `cx`.
    fn seed_placeholders(&self, cx: &mut LaunchContext) -> Result<(), LaunchError> {
        cx.placeholders.set_unknown(self.unknown);
        for (name, value) in &self.placeholders {
            cx.placeholders
                .set(name, value, PlaceholderOrigin::Static)?;
        }
        for (name, value) in &cx.request.placeholders {
            cx.placeholders
                .set(name, value, PlaceholderOrigin::Request)?;
        }
        Ok(())
    }

    /// Runs the customizers the request names on the profile, records their
    /// changes and returns the steps of this launch.
    fn customize(&self, cx: &mut LaunchContext) -> Result<Vec<Arc<dyn LaunchStep>>, LaunchError> {
        let chosen = cx
            .request
            .customizers
            .iter()
            .map(|id| {
                self.customizers
                    .get(id)
                    .cloned()
                    .ok_or_else(|| LaunchError::UnknownCustomizer(id.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut steps = self.steps.clone();
        if chosen.is_empty() {
            return Ok(steps);
        }
        let ctx = CustomizeCtx::new(
            cx.profile.platform.clone(),
            cx.layout.clone(),
            cx.profile.traits.clone(),
        );
        for customizer in chosen {
            if !customizer.applies_to(&cx.profile.traits) {
                tracing::debug!(customizer = %customizer.id(), "customizer does not apply");
                continue;
            }
            tracing::debug!(customizer = %customizer.id(), "running launch customizer");
            let before = cx.profile.clone();
            customizer.customize(&mut cx.profile, &ctx)?;
            if !self.allows_auth_changes(customizer.id())
                && let Some(argument) = diff::auth_change(&before, &cx.profile)
            {
                cx.profile = before;
                return Err(LaunchError::AuthArgument {
                    customizer: customizer.id().clone(),
                    argument,
                });
            }
            diff::record(&before, &mut cx.profile, customizer.id());
            for (name, value) in ctx.take_placeholders() {
                cx.placeholders.set(
                    name,
                    value,
                    PlaceholderOrigin::Customizer(customizer.id().clone()),
                )?;
            }
        }
        for extra in ctx.into_steps() {
            let index = position(&steps, &extra.anchor)?;
            let index = match extra.placement {
                Placement::Before => index,
                Placement::After => index + 1,
            };
            steps.insert(index, extra.step);
        }
        Ok(steps)
    }

    /// Runs the chosen customizers, then every step on `cx`, stopping at the
    /// first failure or cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::UnknownCustomizer`] when the request names a
    /// customizer that is not registered, [`LaunchError::AuthArgument`]
    /// when a customizer without permission adds or removes an
    /// authentication argument, the failure of the first customizer or
    /// step that fails, or [`LaunchError::Cancelled`].
    pub async fn run(&self, cx: &mut LaunchContext) -> Result<(), LaunchError> {
        let events = cx.services.events.clone();
        let op = cx.request.operation;
        publish(events.as_ref(), || Event::OperationStarted {
            op,
            kind: "launch",
        });
        let result = self.run_steps(cx, events.as_ref()).await;
        publish(events.as_ref(), || Event::OperationFinished {
            op,
            outcome: match &result {
                Ok(()) => OperationOutcome::Succeeded,
                Err(LaunchError::Cancelled) => OperationOutcome::Cancelled,
                Err(error) => OperationOutcome::Failed {
                    message: error.to_string(),
                },
            },
        });
        result
    }

    async fn run_steps(
        &self,
        cx: &mut LaunchContext,
        events: Option<&EventBus>,
    ) -> Result<(), LaunchError> {
        if cx.cancel.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        self.seed_placeholders(cx)?;
        let steps = self.customize(cx)?;
        let count = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        for (index, step) in (0_u32..).zip(&steps) {
            if cx.cancel.is_cancelled() {
                return Err(LaunchError::Cancelled);
            }
            tracing::debug!(step = step.name(), "running launch step");
            publish(events, || Event::LaunchStep {
                op: cx.request.operation,
                step: step.name().to_owned(),
                index,
                count,
            });
            step.run(cx).await?;
        }
        Ok(())
    }

    /// Runs every step and returns the started game.
    ///
    /// # Errors
    ///
    /// Returns the failure of the first step that fails, or
    /// [`LaunchError::MissingState`] when no step started the game.
    pub async fn launch(&self, mut cx: LaunchContext) -> Result<GameSession, LaunchError> {
        self.run(&mut cx).await?;
        cx.session.take().ok_or(LaunchError::MissingState {
            step: "launch",
            needs: "a session started by spawn",
        })
    }
}

/// Publishes an event built only when someone listens.
fn publish(events: Option<&EventBus>, event: impl FnOnce() -> Event) {
    if let Some(events) = events.filter(|events| events.has_subscribers()) {
        events.publish(event());
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_download::{Downloader, DownloaderConfig};
    use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
    use coblet_mojang::{RawVersionJson, TraitTable};
    use coblet_platform::{ProfileEngine, ResolvedProfile};
    use coblet_runtime::{
        BoxFuture, LogLevel, LogSource, Request, Response, TokioSpawner, TokioStorage, Transport,
        TransportError,
    };
    use coblet_types::{Arch, OsFamily, Platform, PlayerName};

    use super::Launcher;
    use crate::{
        GameLayout, LaunchContext, LaunchError, LaunchHook, LaunchRequest, LaunchServices,
        LaunchStep, LogItem,
    };

    /// Transport for launches that need no network; it is never called.
    struct Offline;

    impl Transport for Offline {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(std::future::pending())
        }
    }

    const VERSION: &str = r#"{
    "id": "t1",
    "type": "release",
    "mainClass": "net.minecraft.client.Main",
    "minecraftArguments": "--username ${auth_player_name} --accessToken ${auth_access_token} --gameDir ${game_directory}",
    "libraries": [],
    "javaVersion": { "component": "java-runtime-alpha", "majorVersion": 16 },
    "releaseTime": "2020-01-01T00:00:00+00:00",
    "time": "2020-01-01T00:00:00+00:00"
}"#;

    /// A `java` that answers the version probe, records its arguments, logs,
    /// writes a crash report and exits with 3.
    const FAKE_JAVA: &str = r#"#!/bin/sh
if [ "$1" = "-XshowSettings:properties" ]; then
  echo 'Property settings:' >&2
  echo '    java.home = /fake/16' >&2
  echo '    java.version = 16.0.2' >&2
  echo '    os.arch = amd64' >&2
  exit 0
fi
printf '%s\n' "$@" > args.txt
echo '<log4j:Event logger="net.minecraft.client.Main" timestamp="1700000000000" level="INFO" thread="main">'
echo '  <log4j:Message><![CDATA[Setting user: Steve token secret-token-123]]></log4j:Message>'
echo '</log4j:Event>'
echo 'stderr line' >&2
mkdir -p crash-reports
echo crash > crash-reports/crash-1.txt
exit 3
"#;

    fn write_script(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn context(root: &Path, auth: LaunchAuth) -> LaunchContext {
        let storage = Arc::new(TokioStorage::new());
        let transport: Arc<dyn coblet_runtime::Transport> = Arc::new(Offline);
        let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), root.join("meta"));
        let services = LaunchServices::new(
            storage.clone(),
            Arc::new(TokioSpawner::new()),
            Downloader::new(transport, storage, DownloaderConfig::default()),
            Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
            OsFamily::Linux,
        );
        let raw = RawVersionJson::from_slice(VERSION.as_bytes()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
        let platform = Platform::new(OsFamily::Linux, Arch::X86_64);
        let profile = ProfileEngine::builtin()
            .resolve(&resolved, &platform)
            .unwrap();
        let java = root.join("jdk/bin/java");
        write_script(&java, FAKE_JAVA);
        let request = LaunchRequest::new(auth).with_java(&java);
        LaunchContext::new(
            services,
            profile,
            GameLayout::new(root.join("data"), root.join("game")),
            request,
        )
    }

    fn online() -> LaunchAuth {
        LaunchAuth::online(OnlineSession::new(
            PlayerName::new("Steve").unwrap(),
            "0123456789abcdef0123456789abcdef",
            Credential::new("secret-token-123"),
        ))
    }

    #[tokio::test]
    async fn launches_streams_redacted_logs_and_reports_the_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), online());
        let marker = dir.path().join("post-exit.txt");
        write_script(
            &dir.path().join("post.sh"),
            &format!(
                "#!/bin/sh\necho \"$COBLET_GAME_DIR\" > '{}'\n",
                marker.display()
            ),
        );
        cx.request.post_exit = Some(LaunchHook::new(
            dir.path().join("post.sh"),
            Vec::<String>::new(),
        ));
        let session = Launcher::new().launch(cx).await.unwrap();
        assert!(!session.command().to_string().contains("secret-token-123"));
        let mut logs = session.logs();
        let exit = session.wait().await.unwrap();

        assert_eq!(exit.exit.code, Some(3));
        assert!(!exit.killed);
        assert!(exit.suspected_crash);
        assert_eq!(exit.crash_reports.len(), 1);

        let mut lines = Vec::new();
        while let Some(item) = logs.next().await {
            if let LogItem::Line(line) = item {
                lines.push(line);
            }
        }
        let event = lines
            .iter()
            .find(|line| line.logger.as_deref() == Some("net.minecraft.client.Main"))
            .unwrap();
        assert_eq!(event.message, "Setting user: Steve token <redacted>");
        assert_eq!(event.level, LogLevel::Info);
        assert!(
            lines
                .iter()
                .any(|line| line.source == LogSource::Stderr && line.message == "stderr line")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.source == LogSource::Launcher
                    && line.message.contains("exit code 3"))
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.raw.contains("secret-token-123"))
        );

        let args = std::fs::read_to_string(dir.path().join("game/args.txt")).unwrap();
        assert!(
            args.contains(
                "net.minecraft.client.Main\n--username\nSteve\n--accessToken\nsecret-token-123\n"
            ),
            "{args}"
        );
        assert!(args.contains("-Djava.library.path="));
        let post = std::fs::read_to_string(marker).unwrap();
        assert_eq!(post.trim(), dir.path().join("game").to_string_lossy());
    }

    #[tokio::test]
    async fn rejects_a_configured_java_of_the_wrong_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        cx.profile.java.major = 17;
        let error = Launcher::new().launch(cx).await.unwrap_err();
        assert!(
            matches!(
                error,
                LaunchError::JavaMismatch {
                    found_major: 16,
                    major: 17,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_failing_pre_launch_command_stops_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        write_script(&dir.path().join("pre.sh"), "#!/bin/sh\nexit 1\n");
        cx.request.pre_launch = Some(LaunchHook::new(
            dir.path().join("pre.sh"),
            Vec::<String>::new(),
        ));
        let error = Launcher::new().launch(cx).await.unwrap_err();
        assert!(
            matches!(error, LaunchError::PreLaunch(exit) if exit.code == Some(1)),
            "{error}"
        );
        assert!(!dir.path().join("game/args.txt").exists());
    }

    struct Mark;

    impl LaunchStep for Mark {
        fn name(&self) -> &'static str {
            "mark"
        }

        fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async move {
                cx.request.game_args.push("--marked".to_owned());
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn steps_can_be_inserted_replaced_and_removed() {
        let mut launcher = Launcher::new();
        assert_eq!(
            launcher.names(),
            [
                "select-java",
                "verify-files",
                "prepare-natives",
                "client-jar",
                "legacy-fml-libs",
                "logging-config",
                "pre-launch",
                "build-command",
                "spawn"
            ]
        );
        launcher
            .insert_before("build-command", Arc::new(Mark))
            .unwrap();
        launcher.remove("spawn").unwrap();
        assert!(matches!(
            launcher.remove("spawn"),
            Err(LaunchError::UnknownStep(_))
        ));
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        launcher.run(&mut cx).await.unwrap();
        let command = cx.command.unwrap();
        assert_eq!(command.args().last().unwrap(), "--marked");
        assert!(cx.session.is_none());
    }

    struct Agent(coblet_types::ExtensionId);

    impl coblet_runtime::Extension for Agent {
        fn id(&self) -> &coblet_types::ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Agent"
        }
    }

    impl crate::LaunchCustomizer for Agent {
        fn applies_to(&self, _traits: &coblet_mojang::VersionTraits) -> bool {
            true
        }

        fn customize(
            &self,
            profile: &mut coblet_platform::EffectiveLaunchProfile,
            ctx: &crate::CustomizeCtx,
        ) -> Result<(), LaunchError> {
            profile.main_class = "acme.Wrapper".to_owned();
            profile.jvm_args.push(coblet_platform::ProfileArg::always(
                "-javaagent:/opt/fix.jar",
            ));
            ctx.add_step_after("pre-launch", Arc::new(Mark));
            Ok(())
        }
    }

    #[tokio::test]
    async fn customizer_changes_reach_the_game_command() {
        let id = coblet_types::ExtensionId::new("coblet-test:agent").unwrap();
        let mut launcher = Launcher::new();
        launcher
            .register_customizer(Arc::new(Agent(id.clone())))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        cx.request = cx.request.with_customizer(id);
        let session = launcher.launch(cx).await.unwrap();
        assert!(
            session
                .profile()
                .explain()
                .contains("[customize] coblet-test:agent: jvm `-javaagent:/opt/fix.jar` added")
        );
        session.wait().await.unwrap();
        let args = std::fs::read_to_string(dir.path().join("game/args.txt")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let agent = args
            .iter()
            .position(|arg| *arg == "-javaagent:/opt/fix.jar");
        let main = args.iter().position(|arg| *arg == "acme.Wrapper");
        assert!(agent.is_some() && agent < main, "{args:?}");
        assert_eq!(args.last(), Some(&"--marked"));
    }

    struct Values(coblet_types::ExtensionId);

    impl coblet_runtime::Extension for Values {
        fn id(&self) -> &coblet_types::ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Values"
        }
    }

    impl crate::LaunchCustomizer for Values {
        fn applies_to(&self, _traits: &coblet_mojang::VersionTraits) -> bool {
            true
        }

        fn customize(
            &self,
            profile: &mut coblet_platform::EffectiveLaunchProfile,
            ctx: &crate::CustomizeCtx,
        ) -> Result<(), LaunchError> {
            profile.jvm_args.push(coblet_platform::ProfileArg::always(
                "-Dacme=${acme_a}/${acme_b}/${acme_c}/${acme_d}",
            ));
            ctx.set_placeholder("acme_c", "customizer")?;
            ctx.set_placeholder("acme_d", "customizer")?;
            assert!(ctx.set_placeholder("auth_access_token", "stolen").is_err());
            Ok(())
        }
    }

    struct SetD;

    impl LaunchStep for SetD {
        fn name(&self) -> &'static str {
            "set-d"
        }

        fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async move {
                cx.placeholders.set(
                    "acme_d",
                    "step",
                    crate::PlaceholderOrigin::Step("set-d".to_owned()),
                )
            })
        }
    }

    #[tokio::test]
    async fn placeholder_values_come_from_launcher_request_customizers_and_steps() {
        let id = coblet_types::ExtensionId::new("coblet-test:values").unwrap();
        let mut launcher = Launcher::new();
        launcher
            .register_customizer(Arc::new(Values(id.clone())))
            .unwrap();
        launcher.set_placeholder("acme_a", "static").unwrap();
        launcher.set_placeholder("acme_b", "static").unwrap();
        assert!(launcher.set_placeholder("version_name", "x").is_err());
        launcher
            .insert_before("build-command", Arc::new(SetD))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        cx.request = cx
            .request
            .with_customizer(id.clone())
            .with_placeholder("acme_b", "request");
        let session = launcher.launch(cx).await.unwrap();
        assert_eq!(
            session.placeholders().explain(),
            "placeholder acme_a from static\n\
             placeholder acme_b from request\n\
             placeholder acme_c from customizer coblet-test:values\n\
             placeholder acme_d from step set-d\n"
        );
        session.wait().await.unwrap();
        let args = std::fs::read_to_string(dir.path().join("game/args.txt")).unwrap();
        assert!(
            args.lines()
                .any(|arg| arg == "-Dacme=static/request/customizer/step"),
            "{args}"
        );

        launcher.remove("set-d").unwrap();
        let mut cx = context(
            dir.path(),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
        );
        cx.request = cx
            .request
            .with_customizer(id)
            .with_placeholder("auth_uuid", "forged");
        assert!(matches!(
            launcher.launch(cx).await,
            Err(LaunchError::InvalidPlaceholder { .. })
        ));
    }

    struct Unknown(coblet_types::ExtensionId);

    impl coblet_runtime::Extension for Unknown {
        fn id(&self) -> &coblet_types::ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Unknown"
        }
    }

    impl crate::LaunchCustomizer for Unknown {
        fn applies_to(&self, _traits: &coblet_mojang::VersionTraits) -> bool {
            true
        }

        fn customize(
            &self,
            profile: &mut coblet_platform::EffectiveLaunchProfile,
            _ctx: &crate::CustomizeCtx,
        ) -> Result<(), LaunchError> {
            profile
                .jvm_args
                .push(coblet_platform::ProfileArg::always("-Dacme=${acme_nobody}"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn unknown_placeholders_follow_the_launcher_policy() {
        let id = coblet_types::ExtensionId::new("coblet-test:unknown").unwrap();
        let mut launcher = Launcher::new();
        launcher
            .register_customizer(Arc::new(Unknown(id.clone())))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cx = || {
            let mut cx = context(
                dir.path(),
                LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
            );
            cx.request = cx.request.clone().with_customizer(id.clone());
            cx
        };
        assert!(matches!(
            launcher.launch(cx()).await,
            Err(LaunchError::Command(
                coblet_launch_command::CommandError::UnknownPlaceholder(name)
            )) if name == "acme_nobody"
        ));
        launcher.set_unknown_placeholders(coblet_launch_command::UnknownPlaceholder::Keep);
        launcher.launch(cx()).await.unwrap().wait().await.unwrap();
        let args = std::fs::read_to_string(dir.path().join("game/args.txt")).unwrap();
        assert!(
            args.lines().any(|arg| arg == "-Dacme=${acme_nobody}"),
            "{args}"
        );
    }

    /// Collects the events published so far, waiting briefly for stragglers.
    async fn drain(events: &mut coblet_runtime::EventSubscription) -> Vec<coblet_runtime::Event> {
        let mut seen = Vec::new();
        let wait = std::time::Duration::from_millis(200);
        while let Ok(Some(item)) = tokio::time::timeout(wait, events.recv()).await {
            match item {
                coblet_runtime::Received::Event(event) => seen.push(event),
                coblet_runtime::Received::Missed(count) => panic!("missed {count} events"),
            }
        }
        seen
    }

    #[tokio::test]
    async fn launches_report_steps_and_the_game_by_launch_id() {
        use coblet_runtime::{Event, EventBus, OperationId, OperationOutcome};

        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), online());
        let bus = EventBus::new(1024);
        let mut events = bus.subscribe();
        cx.services.events = Some(bus);
        let op = OperationId::random();
        cx.request = cx.request.with_operation(op);
        let session = Launcher::new().launch(cx).await.unwrap();
        assert_eq!(session.operation(), op);
        let pid = session.pid();
        session.wait().await.unwrap();
        let seen = drain(&mut events).await;

        assert!(seen.iter().all(|event| event.op() == Some(op)), "{seen:?}");
        assert_eq!(seen[0], Event::OperationStarted { op, kind: "launch" });
        let steps: Vec<_> = seen
            .iter()
            .filter_map(|event| match event {
                Event::LaunchStep {
                    step, index, count, ..
                } => Some((step.as_str(), *index, *count)),
                _ => None,
            })
            .collect();
        let launcher = Launcher::new();
        let names = launcher.names();
        assert_eq!(steps.len(), names.len());
        assert_eq!(
            steps[0],
            ("select-java", 0, u32::try_from(names.len()).unwrap())
        );
        assert_eq!(steps.last().unwrap().0, "spawn");
        assert!(seen.contains(&Event::GameStarted { op, pid }));
        assert!(seen.contains(&Event::OperationFinished {
            op,
            outcome: OperationOutcome::Succeeded
        }));
        assert!(seen.iter().any(|event| matches!(
            event,
            Event::GameLog { line, .. } if line.message == "stderr line"
        )));
        let Some(Event::GameExited {
            exit,
            killed: false,
            suspected_crash: true,
            ..
        }) = seen.last()
        else {
            panic!("the game exit comes last: {seen:?}");
        };
        assert_eq!(exit.unwrap().code, Some(3));
    }

    #[tokio::test]
    async fn failed_launches_finish_their_operation() {
        use coblet_runtime::{Event, EventBus, OperationOutcome};

        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), online());
        cx.request.java = Some(dir.path().join("no-java"));
        let bus = EventBus::new(64);
        let mut events = bus.subscribe();
        cx.services.events = Some(bus);
        let op = cx.request.operation;
        assert!(Launcher::new().launch(cx).await.is_err());
        let seen = drain(&mut events).await;
        assert!(matches!(
            seen.last(),
            Some(Event::OperationFinished {
                op: finished,
                outcome: OperationOutcome::Failed { .. },
            }) if *finished == op
        ));
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, Event::GameStarted { .. }))
        );
    }

    #[tokio::test]
    async fn a_killer_stops_the_game_while_another_task_waits() {
        let dir = tempfile::tempdir().unwrap();
        let cx = context(dir.path(), online());
        // Answers the version probe like FAKE_JAVA, then runs until killed.
        write_script(
            &dir.path().join("jdk/bin/java"),
            &FAKE_JAVA.replace("printf '%s\\n' \"$@\" > args.txt", "exec sleep 60"),
        );
        let session = Launcher::new().launch(cx).await.unwrap();
        let killer = session.killer();
        let waiter = tokio::spawn(session.wait());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        killer.kill();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
            .await
            .expect("the game should end soon after the kill")
            .unwrap()
            .unwrap();
        assert!(exit.killed);
        assert!(!exit.suspected_crash);
        killer.kill();
    }
}

#[cfg(test)]
mod customize_tests {
    use std::path::Path;
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_download::{Downloader, DownloaderConfig};
    use coblet_launch_command::LaunchAuth;
    use coblet_mojang::{RawVersionJson, TraitTable, VersionTrait, VersionTraits};
    use coblet_platform::{EffectiveLaunchProfile, ProfileArg, ProfileEngine, ResolvedProfile};
    use coblet_runtime::{
        BoxFuture, Extension, Request, Response, TokioSpawner, TokioStorage, Transport,
        TransportError,
    };
    use coblet_types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

    use super::Launcher;
    use crate::{
        CustomizeCtx, GameLayout, LaunchContext, LaunchCustomizer, LaunchError, LaunchRequest,
        LaunchServices, LaunchStep,
    };

    struct Offline;

    impl Transport for Offline {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(std::future::pending())
        }
    }

    /// Step that appends its name to the game arguments, to show it ran and where.
    struct Mark(&'static str);

    impl LaunchStep for Mark {
        fn name(&self) -> &str {
            self.0
        }

        fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async move {
                cx.request.game_args.push(self.0.to_owned());
                Ok(())
            })
        }
    }

    /// Customizer for applet versions: wraps the main class, adds a Java
    /// agent and a step before `probe`.
    struct Applet {
        id: ExtensionId,
        needs: VersionTrait,
        fail: bool,
        anchor: &'static str,
    }

    impl Applet {
        fn new(id: &str) -> Self {
            Self {
                id: ExtensionId::new(id).unwrap(),
                needs: VersionTrait::Applet,
                fail: false,
                anchor: "probe",
            }
        }
    }

    impl Extension for Applet {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Applet test"
        }
    }

    impl LaunchCustomizer for Applet {
        fn applies_to(&self, traits: &VersionTraits) -> bool {
            traits.contains(&self.needs)
        }

        fn customize(
            &self,
            profile: &mut EffectiveLaunchProfile,
            ctx: &CustomizeCtx,
        ) -> Result<(), LaunchError> {
            if self.fail {
                return Err(LaunchError::Step {
                    step: self.id.to_string(),
                    message: "agent jar is missing".to_owned(),
                });
            }
            assert!(ctx.traits.contains(&VersionTrait::Applet));
            assert_eq!(ctx.platform.os(), &OsFamily::Linux);
            profile.main_class = "acme.AppletWrapper".to_owned();
            profile
                .jvm_args
                .push(ProfileArg::always("-javaagent:/opt/acme/fix.jar"));
            ctx.add_step_before(self.anchor, Arc::new(Mark("extra")));
            Ok(())
        }
    }

    fn context(root: &Path, version: &str, request: LaunchRequest) -> LaunchContext {
        let storage = Arc::new(TokioStorage::new());
        let transport: Arc<dyn Transport> = Arc::new(Offline);
        let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), root.join("meta"));
        let services = LaunchServices::new(
            storage.clone(),
            Arc::new(TokioSpawner::new()),
            Downloader::new(transport, storage, DownloaderConfig::default()),
            Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
            OsFamily::Linux,
        );
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../../../fixtures/mojang/{version}.json"));
        let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
        let profile = ProfileEngine::builtin()
            .resolve(&resolved, &Platform::new(OsFamily::Linux, Arch::X86_64))
            .unwrap();
        LaunchContext::new(
            services,
            profile,
            GameLayout::new(root.join("data"), root.join("game")),
            request,
        )
    }

    fn request() -> LaunchRequest {
        LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
    }

    fn chosen(id: &str) -> LaunchRequest {
        request().with_customizer(ExtensionId::new(id).unwrap())
    }

    fn launcher(customizers: impl IntoIterator<Item = Applet>) -> Launcher {
        let mut launcher = Launcher::empty().with_step(Arc::new(Mark("probe")));
        for customizer in customizers {
            launcher.register_customizer(Arc::new(customizer)).unwrap();
        }
        launcher
    }

    fn customize_lines(profile: &EffectiveLaunchProfile) -> Vec<String> {
        profile
            .explain()
            .lines()
            .filter(|line| line.starts_with("[customize]"))
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn a_chosen_customizer_edits_the_profile_and_adds_a_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
        launcher([Applet::new("coblet-test:x")])
            .run(&mut cx)
            .await
            .unwrap();
        assert_eq!(cx.profile.main_class, "acme.AppletWrapper");
        assert_eq!(cx.request.game_args, ["extra", "probe"]);
        assert_eq!(
            customize_lines(&cx.profile),
            [
                "[customize] coblet-test:x: main class set to `acme.AppletWrapper` (was `net.minecraft.launchwrapper.Launch`)",
                "[customize] coblet-test:x: jvm `-javaagent:/opt/acme/fix.jar` added",
            ]
        );
    }

    #[tokio::test]
    async fn customizers_run_only_when_chosen() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "c0.30_01c", request());
        let before = cx.profile.clone();
        launcher([Applet::new("coblet-test:x")])
            .run(&mut cx)
            .await
            .unwrap();
        assert_eq!(cx.profile, before);
        assert_eq!(cx.request.game_args, ["probe"]);
    }

    #[tokio::test]
    async fn customizers_skip_versions_they_do_not_apply_to() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "1.20.1", chosen("coblet-test:x"));
        let before = cx.profile.clone();
        launcher([Applet::new("coblet-test:x")])
            .run(&mut cx)
            .await
            .unwrap();
        assert_eq!(cx.profile, before);

        let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
        let before = cx.profile.clone();
        let customizer = Applet {
            needs: VersionTrait::LegacyFmlLibs,
            ..Applet::new("coblet-test:x")
        };
        launcher([customizer]).run(&mut cx).await.unwrap();
        assert_eq!(cx.profile, before);
    }

    #[tokio::test]
    async fn a_customizer_error_stops_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
        let customizer = Applet {
            fail: true,
            ..Applet::new("coblet-test:x")
        };
        let error = launcher([customizer]).run(&mut cx).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "step `coblet-test:x` failed: agent jar is missing"
        );
        assert_eq!(cx.request.game_args.len(), 0);
    }

    #[tokio::test]
    async fn an_unknown_customizer_fails_before_any_runs() {
        let dir = tempfile::tempdir().unwrap();
        let request =
            chosen("coblet-test:x").with_customizer(ExtensionId::new("acme:gone").unwrap());
        let mut cx = context(dir.path(), "c0.30_01c", request);
        let before = cx.profile.clone();
        let error = launcher([Applet::new("coblet-test:x")])
            .run(&mut cx)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, LaunchError::UnknownCustomizer(id) if id.as_str() == "acme:gone"),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "no launch customizer registered as `acme:gone`"
        );
        assert_eq!(cx.profile, before);
    }

    #[tokio::test]
    async fn an_extra_step_needs_an_existing_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
        let customizer = Applet {
            anchor: "missing",
            ..Applet::new("coblet-test:x")
        };
        let error = launcher([customizer]).run(&mut cx).await.unwrap_err();
        assert!(matches!(error, LaunchError::UnknownStep(name) if name == "missing"));
    }

    #[test]
    fn registering_the_same_id_twice_fails() {
        let mut launcher = launcher([Applet::new("coblet-test:x")]);
        assert!(
            launcher
                .register_customizer(Arc::new(Applet::new("coblet-test:x")))
                .is_err()
        );
        assert_eq!(launcher.customizers().len(), 1);
    }

    /// Customizer that runs `edit` on every version.
    struct Edit {
        id: ExtensionId,
        edit: fn(&mut EffectiveLaunchProfile),
    }

    impl Edit {
        fn new(id: &str, edit: fn(&mut EffectiveLaunchProfile)) -> Arc<Self> {
            Arc::new(Self {
                id: ExtensionId::new(id).unwrap(),
                edit,
            })
        }
    }

    impl Extension for Edit {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Edit test"
        }
    }

    impl LaunchCustomizer for Edit {
        fn applies_to(&self, _traits: &VersionTraits) -> bool {
            true
        }

        fn customize(
            &self,
            profile: &mut EffectiveLaunchProfile,
            _ctx: &CustomizeCtx,
        ) -> Result<(), LaunchError> {
            (self.edit)(profile);
            Ok(())
        }
    }

    fn add_auth_server(profile: &mut EffectiveLaunchProfile) {
        profile.jvm_args.push(ProfileArg::always(
            "-Dauth.server=https://auth.example.com/${auth_uuid}",
        ));
    }

    fn drop_username(profile: &mut EffectiveLaunchProfile) {
        profile.game_args.retain(|arg| arg.value != "--username");
    }

    fn edit_launcher(edits: &[Arc<Edit>]) -> Launcher {
        let mut launcher = Launcher::empty().with_step(Arc::new(Mark("probe")));
        for edit in edits {
            launcher.register_customizer(edit.clone()).unwrap();
        }
        launcher
    }

    #[tokio::test]
    async fn customizers_may_not_change_auth_arguments_by_default() {
        let dir = tempfile::tempdir().unwrap();
        for (edit, argument) in [
            (
                add_auth_server as fn(&mut EffectiveLaunchProfile),
                "-Dauth.server=https://auth.example.com/${auth_uuid}",
            ),
            (drop_username, "--username"),
        ] {
            let mut cx = context(dir.path(), "1.20.1", chosen("acme:auth"));
            let before = cx.profile.clone();
            let error = edit_launcher(&[Edit::new("acme:auth", edit)])
                .run(&mut cx)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    &error,
                    LaunchError::AuthArgument { customizer, argument: found }
                        if customizer.as_str() == "acme:auth" && found == argument
                ),
                "{error}"
            );
            assert_eq!(
                error.to_string(),
                format!(
                    "launch customizer `acme:auth` may not add or remove authentication argument `{argument}` unless the launcher allows it"
                )
            );
            assert_eq!(cx.profile, before);
            assert_eq!(cx.request.game_args.len(), 0);
        }
    }

    #[tokio::test]
    async fn a_granted_customizer_changes_auth_arguments_and_shows_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "1.20.1", chosen("acme:auth"));
        let mut launcher = edit_launcher(&[Edit::new("acme:auth", add_auth_server)]);
        launcher
            .allow_auth_changes(ExtensionId::new("acme:auth").unwrap())
            .unwrap();
        launcher.run(&mut cx).await.unwrap();
        assert_eq!(
            customize_lines(&cx.profile),
            [
                "[customize] acme:auth: jvm `-Dauth.server=https://auth.example.com/${auth_uuid}` added"
            ]
        );
        assert_eq!(cx.request.game_args, ["probe"]);
    }

    #[tokio::test]
    async fn a_grant_covers_only_its_own_customizer() {
        let dir = tempfile::tempdir().unwrap();
        let request =
            chosen("acme:trusted").with_customizer(ExtensionId::new("acme:other").unwrap());
        let mut cx = context(dir.path(), "1.20.1", request);
        let mut launcher = edit_launcher(&[
            Edit::new("acme:trusted", add_auth_server),
            Edit::new("acme:other", drop_username),
        ]);
        launcher
            .allow_auth_changes(ExtensionId::new("acme:trusted").unwrap())
            .unwrap();
        let error = launcher.run(&mut cx).await.unwrap_err();
        assert!(
            matches!(&error, LaunchError::AuthArgument { customizer, .. } if customizer.as_str() == "acme:other"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn other_changes_and_moved_auth_arguments_need_no_grant() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "1.20.1", chosen("acme:plain"));
        let edit = Edit::new("acme:plain", |profile| {
            profile.game_args.reverse();
            profile
                .jvm_args
                .push(ProfileArg::always("-Dacme.flag=true"));
        });
        edit_launcher(&[edit]).run(&mut cx).await.unwrap();
        assert_eq!(
            customize_lines(&cx.profile),
            [
                "[customize] acme:plain: jvm `-Dacme.flag=true` added",
                "[customize] acme:plain: game arguments reordered",
            ]
        );
    }

    #[test]
    fn only_registered_customizers_can_be_granted() {
        let mut launcher = edit_launcher(&[Edit::new("acme:auth", add_auth_server)]);
        let error = launcher
            .allow_auth_changes(ExtensionId::new("acme:gone").unwrap())
            .unwrap_err();
        assert!(matches!(error, LaunchError::UnknownCustomizer(id) if id.as_str() == "acme:gone"));
        assert!(!launcher.allows_auth_changes(&ExtensionId::new("acme:gone").unwrap()));
        launcher
            .allow_auth_changes(ExtensionId::new("acme:auth").unwrap())
            .unwrap();
        assert!(launcher.allows_auth_changes(&ExtensionId::new("acme:auth").unwrap()));
    }

    fn with_component(cx: &mut LaunchContext, json: &str) {
        let component = coblet_platform::ComponentPatch::from_slice(json.as_bytes()).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang/1.20.1.json");
        let raw = RawVersionJson::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits)
            .unwrap()
            .compose([&coblet_platform::Component::new(component)])
            .unwrap();
        cx.profile = ProfileEngine::builtin()
            .resolve(&resolved, &Platform::new(OsFamily::Linux, Arch::X86_64))
            .unwrap();
    }

    #[tokio::test]
    async fn maven_files_are_fetched_but_kept_off_the_classpath() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "1.20.1", request());
        with_component(
            &mut cx,
            r#"{ "uid": "acme.loader", "version": "1", "mavenFiles": [{
                "name": "acme:installer:1",
                "downloads": { "artifact": { "url": "https://maven.example/acme/installer-1.jar", "sha1": "0123456789abcdef0123456789abcdef01234567", "size": 3 } }
            }] }"#,
        );
        assert!(
            cx.profile
                .classpath()
                .all(|file| !file.path.contains("installer"))
        );
        let set = crate::files::FileSet::for_profile(&cx.profile, &cx.layout, &cx.services.catalog)
            .await
            .unwrap();
        assert!(set.tasks.iter().any(|task| matches!(
            &task.dest,
            coblet_download::Destination::Path(path) if path.ends_with("acme/installer/1/installer-1.jar")
        )));
    }

    #[tokio::test]
    async fn jar_mods_and_agents_are_refused_until_supported() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = context(dir.path(), "1.20.1", request());
        with_component(
            &mut cx,
            r#"{ "uid": "acme.old", "version": "1", "jarMods": [{ "name": "acme:jarmod:1" }] }"#,
        );
        let error = crate::ClientJar.run(&mut cx).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "not supported yet: 1 jar mod(s) to merge into the client jar"
        );

        with_component(
            &mut cx,
            r#"{ "uid": "acme.agent", "version": "1", "+agents": [{ "name": "acme:agent:1" }] }"#,
        );
        let error = crate::VerifyFiles.run(&mut cx).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "not supported yet: java agent acme:agent:1"
        );
    }
}
