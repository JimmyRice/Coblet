use std::sync::{Arc, Mutex, PoisonError};

use coblet::auth::{EncryptedFileStore, MsaClientId};
use coblet::platform::{DataProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{ExtensionId, Platform};

use crate::customize::ForeignCustomizer;
use crate::loaders::ForeignLoader;
use crate::profile::ForeignProfile;
use crate::secrets::{ForeignSecrets, encryption_key};
use crate::sources::ForeignRewriter;
use crate::{
    ArtifactKind, Coblet, CobletError, ErrorKind, LaunchCustomizer, ModLoader, PlatformInfo,
    PlatformProfile, SecretStore, UrlRewriter,
};

/// Where tokens are kept.
enum Secrets {
    Memory,
    Foreign(Arc<dyn SecretStore>),
    File(String, coblet::auth::EncryptionKey),
}

struct Settings {
    data_root: String,
    user_agent: UserAgent,
    customizers: Vec<ForeignCustomizer>,
    auth_grants: Vec<ExtensionId>,
    profiles: Vec<Arc<dyn coblet::platform::PlatformProfile>>,
    platform: Option<Platform>,
    msa_client_id: Option<MsaClientId>,
    secrets: Secrets,
    rewriters: Vec<Arc<dyn coblet::runtime::UrlRewriter>>,
    mirror_fallback: bool,
    version_sources: Vec<coblet::mojang::MojangVersions>,
    loaders: Vec<(Arc<dyn coblet::loader::ModLoader>, bool)>,
}

/// Settings of a [`Coblet`] before it is built: Platform Profiles, launch
/// customizers, the Microsoft Client ID and where tokens are kept.
///
/// [`CobletBuilder::build`] can be called once.
#[derive(uniffi::Object)]
pub struct CobletBuilder {
    settings: Mutex<Option<Settings>>,
}

impl std::fmt::Debug for CobletBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletBuilder").finish_non_exhaustive()
    }
}

fn invalid(message: impl Into<String>) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, message)
}

impl CobletBuilder {
    fn add_profile(
        &self,
        profile: Arc<dyn coblet::platform::PlatformProfile>,
    ) -> Result<(), CobletError> {
        self.with(|settings| {
            let taken = settings
                .profiles
                .iter()
                .chain(ProfileEngine::builtin().profiles())
                .any(|known| known.id() == profile.id());
            if taken {
                return Err(invalid(format!(
                    "platform profile `{}` is already registered",
                    profile.id()
                )));
            }
            settings.profiles.push(profile);
            Ok(())
        })
    }

    /// Adds a Mod Loader; `replace` puts it in place of one with the same ID.
    fn add_mod_loader(
        &self,
        loader: Arc<dyn coblet::loader::ModLoader>,
        replace: bool,
    ) -> Result<(), CobletError> {
        self.with(|settings| {
            let taken = settings
                .loaders
                .iter()
                .any(|(known, replaces)| !*replaces && known.id() == loader.id());
            if taken && !replace {
                return Err(invalid(format!(
                    "mod loader `{}` is already registered",
                    loader.id()
                )));
            }
            settings.loaders.push((loader, replace));
            Ok(())
        })
    }

    fn data_loader(
        parsed: Result<coblet::loader::DataLoader, coblet::loader::LoaderError>,
    ) -> Result<Arc<dyn coblet::loader::ModLoader>, CobletError> {
        Ok(Arc::new(
            parsed.map_err(|error| invalid(error.to_string()))?,
        ))
    }

    fn with<T>(
        &self,
        body: impl FnOnce(&mut Settings) -> Result<T, CobletError>,
    ) -> Result<T, CobletError> {
        let mut guard = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
        let settings = guard.as_mut().ok_or_else(|| {
            CobletError::new(ErrorKind::InvalidState, "the builder was already built")
        })?;
        body(settings)
    }
}

#[uniffi::export]
impl CobletBuilder {
    /// Starts building a `Coblet` with the default transport.
    ///
    /// `user_agent` identifies the launcher to web services, such as
    /// `MyLauncher/1.0 (contact@example.com)`.
    ///
    /// # Errors
    ///
    /// Fails when the User-Agent is malformed.
    #[uniffi::constructor]
    pub fn new(data_root: String, user_agent: String) -> Result<Arc<Self>, CobletError> {
        let user_agent = UserAgent::new(user_agent).map_err(|error| invalid(error.to_string()))?;
        Ok(Arc::new(Self {
            settings: Mutex::new(Some(Settings {
                data_root,
                user_agent,
                customizers: Vec::new(),
                auth_grants: Vec::new(),
                profiles: Vec::new(),
                platform: None,
                msa_client_id: None,
                secrets: Secrets::Memory,
                rewriters: Vec::new(),
                mirror_fallback: true,
                version_sources: Vec::new(),
                loaders: Vec::new(),
            })),
        }))
    }

    /// Adds a Platform Profile written as TOML, such as one for an
    /// operating system or architecture the built-in profiles do not cover.
    ///
    /// The format is the one of coblet's built-in profiles; the text can be
    /// generated at run time, for example after probing system library paths.
    ///
    /// # Errors
    ///
    /// Fails for a malformed profile, an `id` that is already registered,
    /// and after `build`.
    pub fn add_platform_profile_toml(&self, toml: &str) -> Result<(), CobletError> {
        let profile = DataProfile::from_toml(toml, ProfileLayer::ThirdParty)
            .map_err(|error| invalid(error.to_string()))?;
        self.add_profile(Arc::new(profile))
    }

    /// Adds a Platform Profile written in this language, for adaptations
    /// that need code, such as probing the machine.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID, an ID that is already registered, and
    /// after `build`.
    pub fn add_platform_profile(
        &self,
        profile: Arc<dyn PlatformProfile>,
    ) -> Result<(), CobletError> {
        self.add_profile(Arc::new(ForeignProfile::new(profile)?))
    }

    /// Describes the platform instead of detecting it from the build target,
    /// for example to give the OS version or platform traits that profiles
    /// match on.
    ///
    /// # Errors
    ///
    /// Fails for a malformed name and after `build`.
    pub fn set_platform(&self, platform: PlatformInfo) -> Result<(), CobletError> {
        let platform = platform.into_platform()?;
        self.with(|settings| {
            settings.platform = Some(platform);
            Ok(())
        })
    }

    /// Adds a launch customizer; it runs only for launches that name its ID
    /// in `LaunchOptions.customizers`.
    ///
    /// # Errors
    ///
    /// Fails for a malformed or already registered ID, and after `build`.
    pub fn add_customizer(&self, customizer: Arc<dyn LaunchCustomizer>) -> Result<(), CobletError> {
        let adapter = ForeignCustomizer::new(customizer)?;
        self.with(|settings| {
            if settings
                .customizers
                .iter()
                .any(|known| known.id() == adapter.id())
            {
                return Err(invalid(format!(
                    "customizer `{}` is already registered",
                    adapter.id()
                )));
            }
            settings.customizers.push(adapter);
            Ok(())
        })
    }

    /// Lets the added customizer `id` add and remove authentication
    /// arguments, such as `--accessToken` or `${auth_uuid}`, for example
    /// one that points the game at a third-party login server.
    ///
    /// Customizers cannot do this by default: the launch fails with
    /// `ErrorKind::Launch` when one tries. Grant it only to customizers the
    /// launcher trusts with the account; their changes still appear in
    /// `GameHandle.report`.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID, one no customizer added with
    /// `add_customizer` has, and after `build`.
    pub fn allow_auth_changes(&self, id: &str) -> Result<(), CobletError> {
        let id =
            ExtensionId::new(id).map_err(|error| invalid(format!("customizer id: {error}")))?;
        self.with(|settings| {
            if !settings.customizers.iter().any(|known| known.id() == &id) {
                return Err(invalid(format!(
                    "customizer `{id}` was not added; add the customizer first"
                )));
            }
            if !settings.auth_grants.contains(&id) {
                settings.auth_grants.push(id);
            }
            Ok(())
        })
    }

    /// Enables Microsoft login with the launcher's Azure application
    /// Client ID, which Minecraft services must have approved. Without one,
    /// `begin_microsoft_login` fails with `Unsupported`.
    ///
    /// # Errors
    ///
    /// Fails for an empty or malformed ID, and after `build`.
    pub fn set_msa_client_id(&self, client_id: String) -> Result<(), CobletError> {
        let client_id = MsaClientId::new(client_id).map_err(|error| invalid(error.to_string()))?;
        self.with(|settings| {
            settings.msa_client_id = Some(client_id);
            Ok(())
        })
    }

    /// Keeps refresh tokens and other secrets in a store written in the
    /// host language, such as the system keychain. Replaces an earlier
    /// `set_encrypted_secret_file`. Without either, secrets live in memory
    /// and every Microsoft account needs a new login after a restart.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn set_secret_store(&self, store: Arc<dyn SecretStore>) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.secrets = Secrets::Foreign(store);
            Ok(())
        })
    }

    /// Keeps secrets in one file encrypted with `key`, 32 bytes such as
    /// from `generate_secret_key`. Keep the key somewhere safer than next to
    /// the file, such as a system keychain. Replaces an earlier
    /// `set_secret_store`.
    ///
    /// # Errors
    ///
    /// Fails for a key that is not 32 bytes, and after `build`.
    pub fn set_encrypted_secret_file(&self, path: String, key: Vec<u8>) -> Result<(), CobletError> {
        let key = encryption_key(key)?;
        self.with(|settings| {
            settings.secrets = Secrets::File(path, key);
            Ok(())
        })
    }

    /// Sends requests whose address starts with `from` to the same path
    /// under `to`, for a regional mirror or a proxy; with `kinds` empty the
    /// rule applies to every kind.
    ///
    /// Rules apply to game files, libraries, assets, Java runtimes and
    /// metadata such as the version manifest, in the order they were added.
    /// Hashes are still checked, so a mirror cannot change content that is
    /// listed with a hash; when the mirror fails or serves something that
    /// does not match, the original address is tried next (see
    /// `set_mirror_fallback`). Account and skin requests are not rewritten.
    ///
    /// With `trust_metadata`, metadata documents from this mirror (version
    /// files, runtime manifests, Loader indexes) that do not match their
    /// hash are used when the original address fails too or fallback is
    /// off, with a `catalog.unverified-mirror` warning event. This suits
    /// mirrors that reformat JSON. Downloaded files are always checked.
    ///
    /// # Errors
    ///
    /// Fails when either prefix is not an HTTP or HTTPS address, and after `build`.
    pub fn add_mirror(
        &self,
        from: String,
        to: String,
        kinds: Vec<ArtifactKind>,
        trust_metadata: bool,
    ) -> Result<(), CobletError> {
        let mut rule = coblet::runtime::PrefixRewriter::new(from, to)
            .map_err(|error| invalid(format!("mirror: {error}")))?;
        if !kinds.is_empty() {
            rule = rule.for_kinds(kinds.into_iter().map(Into::into));
        }
        if trust_metadata {
            rule = rule.trusting_metadata();
        }
        self.with(|settings| {
            settings.rewriters.push(Arc::new(rule));
            Ok(())
        })
    }

    /// Sets whether requests fall back to the original address when a
    /// mirror fails or serves something that does not match its hash; on by
    /// default. Turn it off when the original hosts must never be contacted.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn set_mirror_fallback(&self, enabled: bool) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.mirror_fallback = enabled;
            Ok(())
        })
    }

    /// Adds a rewriter written in this language, run in order with the
    /// mirror rules, for mirrors a prefix cannot describe. Its metadata is
    /// never used unverified.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn add_url_rewriter(&self, rewriter: Arc<dyn UrlRewriter>) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.rewriters.push(Arc::new(ForeignRewriter(rewriter)));
            Ok(())
        })
    }

    /// Adds a game version source that reads a manifest in the format of
    /// Mojang's `version_manifest_v2.json` from `url`, such as a list of
    /// experimental versions. It comes after the official manifest; when
    /// both list an ID, the official entry wins.
    ///
    /// # Errors
    ///
    /// Fails for a malformed or already used `id`, an address that is not
    /// HTTP or HTTPS, and after `build`.
    pub fn add_version_manifest(&self, id: &str, url: String) -> Result<(), CobletError> {
        let id = ExtensionId::new(id).map_err(|error| invalid(format!("source id: {error}")))?;
        let url = coblet::runtime::HttpUrl::new(url)
            .map_err(|error| invalid(format!("manifest address: {error}")))?;
        self.with(|settings| {
            let taken = id.as_str() == "coblet:mojang"
                || settings
                    .version_sources
                    .iter()
                    .any(|source| source.id() == &id);
            if taken {
                return Err(invalid(format!("version source `{id}` is already added")));
            }
            settings
                .version_sources
                .push(coblet::mojang::MojangVersions::other(id, url));
            Ok(())
        })
    }

    /// Registers a Mod Loader written in this language.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID, an ID that is already registered, and
    /// after `build`.
    pub fn add_loader(&self, loader: Arc<dyn ModLoader>) -> Result<(), CobletError> {
        self.add_mod_loader(Arc::new(ForeignLoader::new(loader)?), false)
    }

    /// Puts a Mod Loader written in this language in place of the one
    /// registered under the same ID, such as a patched copy of a built-in
    /// loader; registers it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_loader(&self, loader: Arc<dyn ModLoader>) -> Result<(), CobletError> {
        self.add_mod_loader(Arc::new(ForeignLoader::new(loader)?), true)
    }

    /// Registers a Mod Loader described in TOML: where its versions are
    /// listed and how a version becomes components, in the format of
    /// coblet's data loaders.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition, an ID that is already registered,
    /// and after `build`.
    pub fn add_loader_toml(&self, definition: &str) -> Result<(), CobletError> {
        let loader = Self::data_loader(coblet::loader::DataLoader::from_toml(definition))?;
        self.add_mod_loader(loader, false)
    }

    /// Registers a Mod Loader described in JSON, with the keys of the TOML
    /// form of [`add_loader_toml`](Self::add_loader_toml).
    ///
    /// # Errors
    ///
    /// Fails like [`add_loader_toml`](Self::add_loader_toml).
    pub fn add_loader_json(&self, definition: &str) -> Result<(), CobletError> {
        let loader = Self::data_loader(coblet::loader::DataLoader::from_json(definition))?;
        self.add_mod_loader(loader, false)
    }

    /// Puts a Mod Loader described in TOML in place of the one registered
    /// under the same ID; registers it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition and after `build`.
    pub fn replace_loader_toml(&self, definition: &str) -> Result<(), CobletError> {
        let loader = Self::data_loader(coblet::loader::DataLoader::from_toml(definition))?;
        self.add_mod_loader(loader, true)
    }

    /// Puts a Mod Loader described in JSON in place of the one registered
    /// under the same ID; registers it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition and after `build`.
    pub fn replace_loader_json(&self, definition: &str) -> Result<(), CobletError> {
        let loader = Self::data_loader(coblet::loader::DataLoader::from_json(definition))?;
        self.add_mod_loader(loader, true)
    }

    /// Creates the `Coblet`.
    ///
    /// # Errors
    ///
    /// Fails when called twice or when the transport cannot start.
    pub fn build(&self) -> Result<Arc<Coblet>, CobletError> {
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                CobletError::new(ErrorKind::InvalidState, "the builder was already built")
            })?;
        let mut builder = coblet::Coblet::builder()
            .data_root(settings.data_root)
            .user_agent(settings.user_agent);
        if let Some(platform) = settings.platform {
            builder = builder.platform(platform);
        }
        for profile in settings.profiles {
            builder = builder.platform_profile(profile);
        }
        for customizer in settings.customizers {
            builder = builder.customizer(Arc::new(customizer));
        }
        for id in settings.auth_grants {
            builder = builder.allow_auth_changes(id);
        }
        if let Some(client_id) = settings.msa_client_id {
            builder = builder.msa_client_id(client_id);
        }
        for rewriter in settings.rewriters {
            builder = builder.url_rewriter(rewriter);
        }
        builder = builder.mirror_fallback(settings.mirror_fallback);
        for source in settings.version_sources {
            builder = builder.version_source(Arc::new(source));
        }
        for (loader, replace) in settings.loaders {
            builder = if replace {
                builder.replace_loader(loader)
            } else {
                builder.loader(loader)
            };
        }
        match settings.secrets {
            Secrets::Memory => {}
            Secrets::Foreign(store) => {
                builder = builder.secret_store(Arc::new(ForeignSecrets(store)));
            }
            Secrets::File(path, key) => {
                let storage = Arc::new(coblet::runtime::TokioStorage::new());
                builder =
                    builder.secret_store(Arc::new(EncryptedFileStore::new(storage, path, key)));
            }
        }
        Ok(Arc::new(Coblet::from_inner(builder.build()?)))
    }
}

#[cfg(test)]
mod tests {
    use super::CobletBuilder;
    use crate::{CobletError, ErrorKind, PlatformInfo};

    const LOONGARCH: &str = r#"
id = "community:linux-loongarch64"

[match]
os = ["linux"]
arch = ["loongarch64"]
requires_component = { uid = "org.lwjgl3" }

[rules]
mojang_os = "linux"

[[natives]]
match = "org.lwjgl:*:*:natives-linux"
source = { system = "/usr/lib/lwjgl3" }

[java]
source = "system-only"
"#;

    fn builder() -> std::sync::Arc<CobletBuilder> {
        CobletBuilder::new(
            "/tmp/coblet-uniffi-profiles".to_owned(),
            "coblet-uniffi-test/1.0".to_owned(),
        )
        .unwrap()
    }

    fn kind(error: &CobletError) -> ErrorKind {
        let CobletError::Failed { kind, .. } = error;
        *kind
    }

    #[test]
    fn profiles_from_toml_reach_the_coblet() {
        let builder = builder();
        builder.add_platform_profile_toml(LOONGARCH).unwrap();
        let coblet = builder.build().unwrap();
        assert!(
            coblet
                .inner
                .profile_engine()
                .profiles()
                .iter()
                .any(
                    |profile| coblet::runtime::Extension::id(profile.as_ref()).as_str()
                        == "community:linux-loongarch64"
                )
        );
        assert_eq!(coblet.platform().os, std::env::consts::OS);

        let error = builder.build().err().unwrap();
        assert_eq!(kind(&error), ErrorKind::InvalidState);
        let error = builder.add_platform_profile_toml(LOONGARCH).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidState);
    }

    #[test]
    fn bad_profiles_are_rejected_when_added() {
        let builder = builder();
        let error = builder
            .add_platform_profile_toml("id = \"community:x\"\nunknown = 1\n")
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        builder.add_platform_profile_toml(LOONGARCH).unwrap();
        let error = builder.add_platform_profile_toml(LOONGARCH).unwrap_err();
        assert!(error.to_string().contains("already registered"), "{error}");
        let builtin = LOONGARCH.replace("community:linux-loongarch64", "coblet:linux-arm64");
        assert!(builder.add_platform_profile_toml(&builtin).is_err());
    }

    #[test]
    fn the_platform_can_be_described() {
        let builder = builder();
        builder
            .set_platform(PlatformInfo {
                os: "linux".to_owned(),
                arch: "loongarch64".to_owned(),
                os_version: Some("6.6".to_owned()),
                libc: Some("glibc".to_owned()),
                traits: vec!["acme-gpu".to_owned()],
            })
            .unwrap();
        let info = builder.build().unwrap().platform();
        assert_eq!(info.arch, "loongarch64");
        assert_eq!(info.os_version.as_deref(), Some("6.6"));
        assert_eq!(info.traits, ["acme-gpu"]);

        let error = CobletBuilder::new("/tmp/x".to_owned(), "coblet-test/1.0".to_owned())
            .unwrap()
            .set_platform(PlatformInfo {
                os: "Not An OS".to_owned(),
                arch: "x86_64".to_owned(),
                os_version: None,
                libc: None,
                traits: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    }

    /// Customizer that changes nothing, to name in grants.
    struct Login;

    impl crate::LaunchCustomizer for Login {
        fn id(&self) -> String {
            "acme:fix".to_owned()
        }

        fn display_name(&self) -> String {
            "Login".to_owned()
        }

        fn applies_to(&self, _traits: Vec<String>) -> bool {
            true
        }

        fn customize(
            &self,
            _editor: std::sync::Arc<crate::ProfileEditor>,
        ) -> Result<(), CobletError> {
            Ok(())
        }
    }

    #[test]
    fn auth_grants_name_added_customizers_and_reach_the_launcher() {
        let id = coblet::types::ExtensionId::new("acme:fix").unwrap();
        for grant in [false, true] {
            let builder = builder();
            let error = builder.allow_auth_changes("acme:fix").unwrap_err();
            assert_eq!(kind(&error), ErrorKind::InvalidArgument);
            assert_eq!(
                error.to_string(),
                "customizer `acme:fix` was not added; add the customizer first"
            );
            builder.add_customizer(std::sync::Arc::new(Login)).unwrap();
            let error = builder.allow_auth_changes("No Namespace").unwrap_err();
            assert_eq!(kind(&error), ErrorKind::InvalidArgument);
            if grant {
                builder.allow_auth_changes("acme:fix").unwrap();
                builder.allow_auth_changes("acme:fix").unwrap();
            }
            let coblet = builder.build().unwrap();
            assert_eq!(coblet.inner.launcher().allows_auth_changes(&id), grant);
            let error = builder.allow_auth_changes("acme:fix").unwrap_err();
            assert_eq!(kind(&error), ErrorKind::InvalidState);
        }
    }

    struct Proxy;

    impl crate::UrlRewriter for Proxy {
        fn rewrite(&self, url: String, kind: crate::ArtifactKind) -> Option<String> {
            if kind == crate::ArtifactKind::Asset {
                return Some("not a url".to_owned());
            }
            url.strip_prefix("https://mirror.example/")
                .map(|rest| format!("https://proxy.example/{rest}"))
        }
    }

    #[test]
    fn mirrors_and_rewriters_reach_downloads_and_metadata() {
        use coblet::runtime::{ArtifactKind, HttpUrl};

        let builder = builder();
        builder
            .add_mirror(
                "https://libraries.minecraft.net/".to_owned(),
                "https://mirror.example/libraries/".to_owned(),
                vec![crate::ArtifactKind::Library],
                true,
            )
            .unwrap();
        builder.set_mirror_fallback(false).unwrap();
        builder
            .add_url_rewriter(std::sync::Arc::new(Proxy))
            .unwrap();
        let error = builder
            .add_mirror(
                "file:///x".to_owned(),
                "https://a.example/".to_owned(),
                Vec::new(),
                false,
            )
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        let coblet = builder.build().unwrap();

        let library = HttpUrl::new("https://libraries.minecraft.net/a/b.jar").unwrap();
        for rewriters in [
            coblet.inner.meta().rewriters(),
            coblet.inner.downloader().rewriters(),
        ] {
            assert!(!rewriters.falls_back());
            assert!(
                rewriters
                    .route(&library, &ArtifactKind::Library)
                    .trust_metadata
            );
            assert_eq!(
                rewriters.apply(&library, &ArtifactKind::Library).as_str(),
                "https://proxy.example/libraries/a/b.jar"
            );
            assert_eq!(
                rewriters.apply(&library, &ArtifactKind::Metadata).as_str(),
                library.as_str()
            );
        }
        let asset = HttpUrl::new("https://resources.download.minecraft.net/ab/abc").unwrap();
        assert_eq!(
            coblet
                .inner
                .downloader()
                .rewriters()
                .apply(&asset, &ArtifactKind::Asset)
                .as_str(),
            asset.as_str()
        );
    }

    #[test]
    fn version_manifests_need_unique_ids() {
        let builder = builder();
        let url = "https://meta.example/experiments.json".to_owned();
        builder
            .add_version_manifest("acme:experiments", url.clone())
            .unwrap();
        for id in ["acme:experiments", "coblet:mojang", "no namespace"] {
            let error = builder.add_version_manifest(id, url.clone()).unwrap_err();
            assert_eq!(kind(&error), ErrorKind::InvalidArgument, "{id}");
        }
        let error = builder
            .add_version_manifest("acme:other", "ftp://x".to_owned())
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        builder.build().unwrap();
    }
}
