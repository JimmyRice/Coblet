use std::collections::BTreeMap;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use crate::command::argfile_quote;
use crate::placeholder::Values;
use crate::{
    ArgFile, AssetsLocation, CommandError, EnvChange, JavaExecutable, LaunchAuth, LaunchCommand,
    MainClass, Memory, PlaceholderResolver, QuickPlay, Resolution, TemplateArg, UnknownPlaceholder,
};

/// Builder state: the Java executable is missing.
#[derive(Debug)]
pub struct NeedJava;
/// Builder state: the main class is missing.
#[derive(Debug)]
pub struct NeedMain;
/// Builder state: authentication is missing.
#[derive(Debug)]
pub struct NeedAuth;
/// Builder state: everything required is set; [`LaunchCommandBuilder::build`] is available.
#[derive(Debug)]
pub struct Ready;

/// Command line length above which an argument file is used: the Windows
/// limit of 32 767 characters, with room for the launcher's own additions.
pub const DEFAULT_ARGFILE_THRESHOLD: usize = 30_000;

#[derive(Clone, Default)]
struct Parts {
    java: Option<JavaExecutable>,
    main: Option<MainClass>,
    auth: Option<LaunchAuth>,
    classpath: Vec<PathBuf>,
    classpath_separator: Option<String>,
    game_directory: Option<PathBuf>,
    assets: Option<AssetsLocation>,
    natives_directory: Option<PathBuf>,
    library_directory: Option<PathBuf>,
    jvm_args: Vec<TemplateArg>,
    game_args: Vec<TemplateArg>,
    memory: Option<Memory>,
    resolution: Option<Resolution>,
    quick_play: Option<QuickPlay>,
    features: BTreeMap<String, bool>,
    launcher: Option<(String, String)>,
    version: Option<(String, String)>,
    user_type: Option<String>,
    env: Vec<EnvChange>,
    resolvers: Vec<Arc<dyn PlaceholderResolver>>,
    unknown: UnknownPlaceholder,
    argfile: Option<(PathBuf, usize)>,
}

/// Typestate builder of a [`LaunchCommand`].
///
/// The Java executable, main class and authentication must be set in that
/// order; only then is [`build`](LaunchCommandBuilder::build) available.
///
/// # Examples
///
/// ```
/// use coblet_launch_command::{
///     AssetsLocation, JavaExecutable, LaunchAuth, LaunchCommand, MainClass, Memory,
/// };
/// use coblet_types::{MiB, PlayerName};
///
/// let command = LaunchCommand::builder()
///     .java(JavaExecutable::new("/opt/jdk-17/bin/java"))
///     .main_class(MainClass::new("net.minecraft.client.main.Main"))
///     .auth(LaunchAuth::offline(PlayerName::new("Steve")?))
///     .classpath(["/data/libraries/a.jar", "/data/versions/1.20.1/1.20.1.jar"])
///     .classpath_separator(":")
///     .game_directory("/games/minecraft")
///     .assets(AssetsLocation::new("/data/assets", "5"))
///     .natives_directory("/data/natives/1.20.1")
///     .jvm_args(["-cp", "${classpath}"])
///     .game_args(["--username", "${auth_player_name}", "--uuid", "${auth_uuid}"])
///     .memory(Memory::new(MiB(512), MiB(2048))?)
///     .version("1.20.1", "release")
///     .build()?;
/// assert_eq!(
///     command.to_string(),
///     "/opt/jdk-17/bin/java -Xms512M -Xmx2048M -cp \
///      /data/libraries/a.jar:/data/versions/1.20.1/1.20.1.jar \
///      net.minecraft.client.main.Main --username Steve --uuid 5627dd98e6be3c21b8a8e92344183641"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub struct LaunchCommandBuilder<State> {
    parts: Parts,
    state: PhantomData<State>,
}

impl<State> std::fmt::Debug for LaunchCommandBuilder<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchCommandBuilder")
            .field("java", &self.parts.java)
            .field("main", &self.parts.main)
            .finish_non_exhaustive()
    }
}

impl LaunchCommand {
    /// Starts a builder.
    pub fn builder() -> LaunchCommandBuilder<NeedJava> {
        LaunchCommandBuilder {
            parts: Parts::default(),
            state: PhantomData,
        }
    }
}

impl<State> LaunchCommandBuilder<State> {
    fn next<Next>(self) -> LaunchCommandBuilder<Next> {
        LaunchCommandBuilder {
            parts: self.parts,
            state: PhantomData,
        }
    }

    /// Appends classpath entries in order.
    pub fn classpath<P: Into<PathBuf>>(mut self, entries: impl IntoIterator<Item = P>) -> Self {
        self.parts
            .classpath
            .extend(entries.into_iter().map(Into::into));
        self
    }

    /// Sets `${classpath_separator}`; by default `;` on Windows hosts and `:` elsewhere.
    pub fn classpath_separator(mut self, separator: impl Into<String>) -> Self {
        self.parts.classpath_separator = Some(separator.into());
        self
    }

    /// Sets the game directory, which is also the working directory.
    pub fn game_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.parts.game_directory = Some(path.into());
        self
    }

    /// Sets the assets location.
    pub fn assets(mut self, assets: AssetsLocation) -> Self {
        self.parts.assets = Some(assets);
        self
    }

    /// Sets `${natives_directory}`.
    pub fn natives_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.parts.natives_directory = Some(path.into());
        self
    }

    /// Sets `${library_directory}`.
    pub fn library_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.parts.library_directory = Some(path.into());
        self
    }

    /// Appends JVM argument templates.
    pub fn jvm_args<A: Into<TemplateArg>>(mut self, args: impl IntoIterator<Item = A>) -> Self {
        self.parts.jvm_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Appends game argument templates.
    pub fn game_args<A: Into<TemplateArg>>(mut self, args: impl IntoIterator<Item = A>) -> Self {
        self.parts
            .game_args
            .extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the heap limits, passed as `-Xms` and `-Xmx` before the other JVM arguments.
    pub fn memory(mut self, memory: Memory) -> Self {
        self.parts.memory = Some(memory);
        self
    }

    /// Sets a window size and turns on `has_custom_resolution`.
    pub fn resolution(mut self, resolution: Resolution) -> Self {
        self.parts.resolution = Some(resolution);
        self
    }

    /// Joins a world, server or Realm right after start.
    pub fn quick_play(mut self, quick_play: QuickPlay) -> Self {
        self.parts.quick_play = Some(quick_play);
        self
    }

    /// Sets a launch feature explicitly; it overrides values derived from other settings.
    pub fn feature(mut self, name: impl Into<String>, value: bool) -> Self {
        self.parts.features.insert(name.into(), value);
        self
    }

    /// Sets `${launcher_name}` and `${launcher_version}`; `coblet` and the crate version by default.
    pub fn launcher(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.parts.launcher = Some((name.into(), version.into()));
        self
    }

    /// Sets `${version_name}` and `${version_type}`.
    pub fn version(mut self, name: impl Into<String>, version_type: impl Into<String>) -> Self {
        self.parts.version = Some((name.into(), version_type.into()));
        self
    }

    /// Overrides `${user_type}`; `msa` for online sessions and `legacy` otherwise by default.
    pub fn user_type(mut self, user_type: impl Into<String>) -> Self {
        self.parts.user_type = Some(user_type.into());
        self
    }

    /// Adds environment changes.
    pub fn env(mut self, changes: impl IntoIterator<Item = EnvChange>) -> Self {
        self.parts.env.extend(changes);
        self
    }

    /// Registers a resolver for extra placeholders.
    pub fn placeholder_resolver(mut self, resolver: Arc<dyn PlaceholderResolver>) -> Self {
        self.parts.resolvers.push(resolver);
        self
    }

    /// Sets what happens to unknown placeholders; an error by default.
    pub fn unknown_placeholders(mut self, policy: UnknownPlaceholder) -> Self {
        self.parts.unknown = policy;
        self
    }

    /// Moves the JVM arguments into an argument file at `path` when the
    /// command line exceeds `threshold` characters. Only for Java 9 and later.
    pub fn argfile(mut self, path: impl Into<PathBuf>, threshold: usize) -> Self {
        self.parts.argfile = Some((path.into(), threshold));
        self
    }
}

impl LaunchCommandBuilder<NeedJava> {
    /// Sets the Java executable.
    pub fn java(mut self, java: JavaExecutable) -> LaunchCommandBuilder<NeedMain> {
        self.parts.java = Some(java);
        self.next()
    }
}

impl LaunchCommandBuilder<NeedMain> {
    /// Sets the main class.
    pub fn main_class(mut self, main: MainClass) -> LaunchCommandBuilder<NeedAuth> {
        self.parts.main = Some(main);
        self.next()
    }
}

impl LaunchCommandBuilder<NeedAuth> {
    /// Sets the identity.
    pub fn auth(mut self, auth: LaunchAuth) -> LaunchCommandBuilder<Ready> {
        self.parts.auth = Some(auth);
        self.next()
    }
}

fn path_value(path: Option<&PathBuf>) -> OsString {
    path.map(|path| path.as_os_str().to_owned())
        .unwrap_or_default()
}

impl Parts {
    fn features(&self, auth: &LaunchAuth) -> BTreeMap<String, bool> {
        let mut features = BTreeMap::new();
        let mut set = |name: &str, value: bool| {
            features.insert(name.to_owned(), value);
        };
        set("is_demo_user", matches!(auth, LaunchAuth::Demo(_)));
        set("has_custom_resolution", self.resolution.is_some());
        set("has_quick_plays_support", self.quick_play.is_some());
        set(
            "is_quick_play_singleplayer",
            matches!(self.quick_play, Some(QuickPlay::Singleplayer(_))),
        );
        set(
            "is_quick_play_multiplayer",
            matches!(self.quick_play, Some(QuickPlay::Multiplayer(_))),
        );
        set(
            "is_quick_play_realms",
            matches!(self.quick_play, Some(QuickPlay::Realms(_))),
        );
        features.extend(self.features.iter().map(|(k, v)| (k.clone(), *v)));
        features
    }

    fn auth_values(
        &self,
        auth: &LaunchAuth,
        table: &mut BTreeMap<&'static str, OsString>,
        secrets: &mut Vec<String>,
    ) {
        table.insert("auth_player_name", auth.player_name().as_str().into());
        let uuid = auth.uuid();
        match auth {
            LaunchAuth::Online(session) => {
                let token = session.access_token.expose().to_owned();
                table.insert("auth_access_token", token.clone().into());
                table.insert("auth_session", format!("token:{token}:{uuid}").into());
                secrets.push(token);
                let xuid = session
                    .xuid
                    .as_ref()
                    .map(|x| x.expose().to_owned())
                    .unwrap_or_default();
                let client = session
                    .client_id
                    .as_ref()
                    .map(|c| c.expose().to_owned())
                    .unwrap_or_default();
                table.insert("auth_xuid", xuid.clone().into());
                table.insert("clientid", client.clone().into());
                secrets.extend([xuid, client]);
                table.insert(
                    "user_type",
                    self.user_type
                        .clone()
                        .unwrap_or_else(|| "msa".to_owned())
                        .into(),
                );
            }
            LaunchAuth::Offline(_) | LaunchAuth::Demo(_) => {
                table.insert("auth_access_token", "0".into());
                table.insert("auth_session", "-".into());
                table.insert("auth_xuid", "0".into());
                table.insert("clientid", "0".into());
                table.insert(
                    "user_type",
                    self.user_type
                        .clone()
                        .unwrap_or_else(|| "legacy".to_owned())
                        .into(),
                );
            }
        }
    }

    fn table(&self, auth: &LaunchAuth) -> (BTreeMap<&'static str, OsString>, Vec<String>) {
        let mut table: BTreeMap<&'static str, OsString> = BTreeMap::new();
        let mut secrets = Vec::new();
        let separator = self
            .classpath_separator
            .clone()
            .unwrap_or_else(|| if cfg!(windows) { ";" } else { ":" }.to_owned());
        let mut classpath = OsString::new();
        for (index, entry) in self.classpath.iter().enumerate() {
            if index > 0 {
                classpath.push(&separator);
            }
            classpath.push(entry);
        }
        table.insert("classpath", classpath);
        table.insert("classpath_separator", separator.into());

        self.auth_values(auth, &mut table, &mut secrets);
        table.insert("auth_uuid", auth.uuid().into());
        table.insert("user_properties", "{}".into());

        let (name, version_type) = self.version.clone().unwrap_or_default();
        table.insert("version_name", name.into());
        table.insert("version_type", version_type.into());
        let (launcher, launcher_version) = self
            .launcher
            .clone()
            .unwrap_or_else(|| ("coblet".to_owned(), env!("CARGO_PKG_VERSION").to_owned()));
        table.insert("launcher_name", launcher.into());
        table.insert("launcher_version", launcher_version.into());

        table.insert("game_directory", path_value(self.game_directory.as_ref()));
        table.insert(
            "natives_directory",
            path_value(self.natives_directory.as_ref()),
        );
        table.insert(
            "library_directory",
            path_value(self.library_directory.as_ref()),
        );
        if let Some(assets) = &self.assets {
            table.insert("assets_root", assets.root().as_os_str().to_owned());
            table.insert("assets_index_name", assets.index().into());
            table.insert("game_assets", assets.game_assets().into_os_string());
        }
        if let Some(resolution) = self.resolution {
            table.insert("resolution_width", resolution.width.to_string().into());
            table.insert("resolution_height", resolution.height.to_string().into());
        }
        let quick = |value: &str| OsString::from(value);
        table.insert("quickPlayPath", quick("quickPlay/log.json"));
        match &self.quick_play {
            Some(QuickPlay::Singleplayer(world)) => {
                table.insert("quickPlaySingleplayer", quick(world));
            }
            Some(QuickPlay::Multiplayer(server)) => {
                table.insert("quickPlayMultiplayer", quick(server));
            }
            Some(QuickPlay::Realms(realm)) => {
                table.insert("quickPlayRealms", quick(realm));
            }
            None => {}
        }
        (table, secrets)
    }
}

impl LaunchCommandBuilder<Ready> {
    /// Builds the command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] for unknown or unterminated placeholders and
    /// a missing game directory.
    pub fn build(self) -> Result<LaunchCommand, CommandError> {
        let parts = self.parts;
        let (Some(java), Some(main), Some(auth)) = (&parts.java, &parts.main, &parts.auth) else {
            return Err(CommandError::Missing("java, main class or authentication"));
        };
        let cwd = parts
            .game_directory
            .clone()
            .ok_or(CommandError::Missing("game directory"))?;
        let features = parts.features(auth);
        let (table, secrets) = parts.table(auth);
        let values = Values {
            table,
            resolvers: &parts.resolvers,
            unknown: parts.unknown,
        };

        let mut jvm: Vec<OsString> = Vec::new();
        if let Some(memory) = parts.memory {
            jvm.push(format!("-Xms{}M", memory.min().0).into());
            jvm.push(format!("-Xmx{}M", memory.max().0).into());
        }
        for arg in &parts.jvm_args {
            if arg.condition.holds(&features) {
                jvm.push(values.substitute(&arg.value)?);
            }
        }
        let mut game: Vec<OsString> = Vec::new();
        for arg in &parts.game_args {
            if arg.condition.holds(&features) {
                game.push(values.substitute(&arg.value)?);
            }
        }

        let length: usize = java.path().as_os_str().len()
            + main.as_str().len()
            + jvm
                .iter()
                .chain(&game)
                .map(|arg| arg.len() + 1)
                .sum::<usize>();
        let mut argfile = None;
        let mut args = Vec::with_capacity(jvm.len() + game.len() + 1);
        match &parts.argfile {
            Some((path, threshold)) if length > *threshold => {
                let mut content = String::new();
                for arg in jvm
                    .iter()
                    .chain(std::iter::once(&OsString::from(main.as_str())))
                {
                    content.push_str(&argfile_quote(arg));
                    content.push('\n');
                }
                let mut reference = OsString::from("@");
                reference.push(path);
                args.push(reference);
                argfile = Some(ArgFile {
                    path: path.clone(),
                    content,
                });
            }
            _ => {
                args.extend(jvm);
                args.push(main.as_str().into());
            }
        }
        args.extend(game);
        Ok(LaunchCommand {
            program: java.path().to_path_buf(),
            args,
            env: parts.env,
            cwd,
            argfile,
            secrets: secrets.into_iter().filter(|s| !s.is_empty()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Arc;

    use coblet_types::{MiB, PlayerName};

    use crate::{
        AssetsLocation, CommandError, Credential, FeatureCondition, JavaExecutable, LaunchAuth,
        LaunchCommand, LaunchCommandBuilder, MainClass, Memory, OnlineSession, PlaceholderResolver,
        QuickPlay, Ready, Resolution, TemplateArg,
    };

    fn base(auth: LaunchAuth) -> LaunchCommandBuilder<Ready> {
        LaunchCommand::builder()
            .java(JavaExecutable::new("/jdk/bin/java"))
            .main_class(MainClass::new("net.minecraft.client.main.Main"))
            .auth(auth)
            .game_directory("/game")
            .classpath_separator(":")
    }

    fn steve() -> PlayerName {
        PlayerName::new("Steve").unwrap()
    }

    fn args(command: &LaunchCommand) -> Vec<String> {
        command
            .args()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn offline_values() {
        let command = base(LaunchAuth::offline(steve()))
            .assets(AssetsLocation::new("/assets", "pre-1.6"))
            .natives_directory("/natives")
            .game_args([
                "${auth_player_name}",
                "${auth_session}",
                "${auth_access_token}",
                "${user_type}",
                "${user_properties}",
                "${game_assets}",
                "${natives_directory}",
                "${launcher_name}",
            ])
            .build()
            .unwrap();
        assert_eq!(
            args(&command)[1..],
            [
                "Steve",
                "-",
                "0",
                "legacy",
                "{}",
                "/assets/virtual/pre-1.6",
                "/natives",
                "coblet"
            ]
        );
        assert_eq!(command.cwd(), std::path::Path::new("/game"));
    }

    #[test]
    fn memory_comes_first_and_classpath_joins() {
        let command = base(LaunchAuth::offline(steve()))
            .classpath(["/a.jar", "/b.jar"])
            .jvm_args(["-cp", "${classpath}"])
            .memory(Memory::new(MiB(256), MiB(1024)).unwrap())
            .build()
            .unwrap();
        assert_eq!(
            args(&command),
            [
                "-Xms256M",
                "-Xmx1024M",
                "-cp",
                "/a.jar:/b.jar",
                "net.minecraft.client.main.Main"
            ]
        );
    }

    #[test]
    fn features_follow_auth_resolution_and_quick_play() {
        let demo = TemplateArg::when(
            "--demo",
            FeatureCondition::when([("is_demo_user".to_owned(), true)]),
        );
        let size = TemplateArg::when(
            "--width=${resolution_width}",
            FeatureCondition::when([("has_custom_resolution".to_owned(), true)]),
        );
        let world = TemplateArg::when(
            "${quickPlaySingleplayer}",
            FeatureCondition::when([("is_quick_play_singleplayer".to_owned(), true)]),
        );
        let offline = base(LaunchAuth::offline(steve()))
            .game_args([demo.clone(), size.clone(), world.clone()])
            .build()
            .unwrap();
        assert_eq!(args(&offline), ["net.minecraft.client.main.Main"]);

        let demo_run = base(LaunchAuth::demo(steve()))
            .game_args([demo, size, world])
            .resolution(Resolution {
                width: 854,
                height: 480,
            })
            .quick_play(QuickPlay::Singleplayer("New World".to_owned()))
            .build()
            .unwrap();
        assert_eq!(args(&demo_run)[1..], ["--demo", "--width=854", "New World"]);
    }

    #[test]
    fn explicit_feature_overrides_derived_one() {
        let demo = TemplateArg::when(
            "--demo",
            FeatureCondition::when([("is_demo_user".to_owned(), true)]),
        );
        let command = base(LaunchAuth::demo(steve()))
            .game_args([demo])
            .feature("is_demo_user", false)
            .build()
            .unwrap();
        assert_eq!(args(&command).len(), 1);
    }

    #[test]
    fn online_secrets_are_redacted() {
        let mut session = OnlineSession::new(
            steve(),
            "0123456789abcdef0123456789abcdef",
            Credential::new("eyJ.secret.token"),
        );
        session.xuid = Some(Credential::new("2535400000000000"));
        let command = base(LaunchAuth::online(session))
            .game_args([
                "--accessToken",
                "${auth_access_token}",
                "--session",
                "${auth_session}",
                "--xuid",
                "${auth_xuid}",
                "--userType",
                "${user_type}",
            ])
            .build()
            .unwrap();
        let shown = command.to_string();
        let debug = format!("{command:?}");
        for text in [&shown, &debug] {
            assert!(!text.contains("eyJ.secret.token"), "{text}");
            assert!(!text.contains("2535400000000000"), "{text}");
        }
        assert!(shown.contains("--accessToken '<redacted>'"));
        assert!(shown.contains("--session 'token:<redacted>:0123456789abcdef0123456789abcdef'"));
        assert!(shown.contains("--userType msa"));
        let exposed = command.expose_secrets().to_string();
        assert!(exposed.contains("eyJ.secret.token"));
        assert_eq!(args(&command)[2], "eyJ.secret.token");
    }

    #[test]
    fn display_quotes_spaces() {
        let command = base(LaunchAuth::offline(steve()))
            .game_args(["--gameDir", "/Users/a b/.minecraft", "it's"])
            .build()
            .unwrap();
        assert_eq!(
            command.to_string(),
            "/jdk/bin/java net.minecraft.client.main.Main --gameDir '/Users/a b/.minecraft' 'it'\\''s'"
        );
    }

    #[test]
    fn argfile_holds_jvm_args_and_main_class() {
        let long = "x".repeat(100);
        let builder = || {
            base(LaunchAuth::offline(steve()))
                .classpath([format!("/{long}.jar"), "/c \"q\".jar".to_owned()])
                .jvm_args(["-cp", "${classpath}"])
                .game_args(["--version", "1.20.1"])
        };
        let short = builder().argfile("/tmp/args.txt", 10_000).build().unwrap();
        assert!(short.argfile().is_none());

        let command = builder().argfile("/tmp/args.txt", 50).build().unwrap();
        assert_eq!(args(&command), ["@/tmp/args.txt", "--version", "1.20.1"]);
        let file = command.argfile().unwrap();
        assert_eq!(file.path, std::path::Path::new("/tmp/args.txt"));
        assert_eq!(
            file.content,
            format!(
                "\"-cp\"\n\"/{long}.jar:/c \\\"q\\\".jar\"\n\"net.minecraft.client.main.Main\"\n"
            )
        );
    }

    struct Brand;

    impl PlaceholderResolver for Brand {
        fn resolve(&self, name: &str) -> Option<OsString> {
            (name == "brand").then(|| "acme".into())
        }
    }

    #[test]
    fn resolvers_and_unknown_placeholders() {
        let error = base(LaunchAuth::offline(steve()))
            .game_args(["${brand}"])
            .build()
            .unwrap_err();
        assert_eq!(error, CommandError::UnknownPlaceholder("brand".to_owned()));
        let command = base(LaunchAuth::offline(steve()))
            .game_args(["${brand}"])
            .placeholder_resolver(Arc::new(Brand))
            .build()
            .unwrap();
        assert_eq!(args(&command)[1], "acme");
    }

    #[test]
    fn game_directory_is_required() {
        let error = LaunchCommand::builder()
            .java(JavaExecutable::new("java"))
            .main_class(MainClass::new("Main"))
            .auth(LaunchAuth::offline(steve()))
            .build()
            .unwrap_err();
        assert_eq!(error, CommandError::Missing("game directory"));
    }

    #[test]
    fn every_filled_placeholder_is_listed_as_builtin() {
        for quick_play in [
            QuickPlay::Singleplayer("w".to_owned()),
            QuickPlay::Multiplayer("s".to_owned()),
            QuickPlay::Realms("r".to_owned()),
        ] {
            let builder = base(LaunchAuth::offline(steve()))
                .assets(AssetsLocation::new("/assets", "5"))
                .resolution(Resolution {
                    width: 854,
                    height: 480,
                })
                .quick_play(quick_play);
            let (table, _) = builder.parts.table(&LaunchAuth::offline(steve()));
            for name in table.keys() {
                assert!(crate::is_builtin_placeholder(name), "{name}");
            }
        }
    }
}
