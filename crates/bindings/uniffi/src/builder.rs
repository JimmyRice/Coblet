use std::sync::{Arc, Mutex, PoisonError};

use coblet::auth::{EncryptedFileStore, MsaClientId};
use coblet::platform::{DataProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{ExtensionId, Platform};

use crate::customize::ForeignCustomizer;
use crate::java::{ForeignJavaLocator, ForeignJavaProvider, extension_id};
use crate::loaders::{ForeignLoader, loader_id};
use crate::placeholders::{ForeignPlaceholderSource, placeholder_name};
use crate::profile::ForeignProfile;
use crate::secrets::{ForeignSecrets, encryption_key};
use crate::sources::ForeignRewriter;
use crate::traits::ForeignTraitSource;
use crate::version_sources::{
    ForeignGameVersions, ForeignLoaderVersions, GameVersionSource, LoaderVersionSource,
};
use crate::{
    ArtifactKind, Coblet, CobletError, ErrorKind, JavaLocator, JavaProvider, LaunchCustomizer,
    ModLoader, PlaceholderSource, PlatformInfo, PlatformProfile, SecretStore, TraitSource,
    UnknownPlaceholders, UrlRewriter,
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
    version_sources: Vec<VersionChange>,
    loaders: Vec<(Arc<dyn coblet::loader::ModLoader>, bool)>,
    loader_sources: Vec<LoaderSourceChange>,
    java: Vec<JavaChange>,
    traits: Vec<TraitChange>,
    launch: Vec<LaunchChange>,
}

/// One launch step or placeholder change, applied in order.
enum LaunchChange {
    Placeholder(String, String),
    Source(Arc<dyn coblet::launch::PlaceholderSource>, bool),
    RemoveSource(ExtensionId),
    Unknown(UnknownPlaceholders),
}

type GameSource = Arc<dyn coblet::catalog::CatalogSource<coblet::mojang::GameVersion>>;
type LoaderSource = Arc<dyn coblet::catalog::CatalogSource<coblet::loader::LoaderVersion>>;

/// One game version source change, applied in order on top of `coblet:mojang`.
enum VersionChange {
    Add(GameSource),
    Replace(GameSource),
    Remove(ExtensionId),
}

/// One Mod Loader version source change: the loader, then the change.
enum LoaderSourceChange {
    Add(ExtensionId, LoaderSource, bool),
    Remove(ExtensionId, ExtensionId),
}

/// One version trait source change, applied in order on top of the built-in list.
enum TraitChange {
    Add(Arc<dyn coblet::mojang::TraitSource>),
    Replace(Arc<dyn coblet::mojang::TraitSource>),
    Remove(ExtensionId),
}

/// One Java change, applied in order on top of the built-in lists.
enum JavaChange {
    Provider(Arc<dyn coblet::java::JavaProvider>, bool),
    RemoveProvider(ExtensionId),
    Locator(Arc<dyn coblet::java::JavaLocator>, bool),
    RemoveLocator(ExtensionId),
    Executable(String),
    Directory(String, String),
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

    fn version_change(&self, change: VersionChange) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.version_sources.push(change);
            Ok(())
        })
    }

    fn loader_source(
        &self,
        loader: &str,
        source: LoaderSource,
        replace: bool,
    ) -> Result<(), CobletError> {
        let loader = loader_id(loader)?;
        self.with(|settings| {
            settings
                .loader_sources
                .push(LoaderSourceChange::Add(loader, source, replace));
            Ok(())
        })
    }

    fn data_versions(
        parsed: Result<coblet::loader::DataLoaderVersions, coblet::loader::LoaderError>,
    ) -> Result<LoaderSource, CobletError> {
        Ok(Arc::new(
            parsed.map_err(|error| invalid(error.to_string()))?,
        ))
    }

    fn java_change(&self, change: JavaChange) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.java.push(change);
            Ok(())
        })
    }

    fn data_java_provider(
        parsed: Result<coblet::java::DataJavaProvider, coblet::java::JavaError>,
    ) -> Result<Arc<dyn coblet::java::JavaProvider>, CobletError> {
        Ok(Arc::new(
            parsed.map_err(|error| invalid(error.to_string()))?,
        ))
    }

    fn trait_change(&self, change: TraitChange) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.traits.push(change);
            Ok(())
        })
    }

    fn trait_table(
        parsed: Result<coblet::mojang::TraitTable, coblet::mojang::TraitError>,
    ) -> Result<Arc<dyn coblet::mojang::TraitSource>, CobletError> {
        Ok(Arc::new(
            parsed.map_err(|error| invalid(error.to_string()))?,
        ))
    }

    fn launch_change(&self, change: LaunchChange) -> Result<(), CobletError> {
        self.with(|settings| {
            settings.launch.push(change);
            Ok(())
        })
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
                loader_sources: Vec::new(),
                java: Vec::new(),
                traits: Vec::new(),
                launch: Vec::new(),
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
    /// experimental versions. It comes after the official manifest and the
    /// sources added before it; when two list an ID, the earlier one wins.
    ///
    /// # Errors
    ///
    /// Fails for a malformed `id`, an address that is not HTTP or HTTPS,
    /// and after `build`; `build` fails when the ID is already used.
    pub fn add_version_manifest(&self, id: &str, url: String) -> Result<(), CobletError> {
        let id = ExtensionId::new(id).map_err(|error| invalid(format!("source id: {error}")))?;
        let url = coblet::runtime::HttpUrl::new(url)
            .map_err(|error| invalid(format!("manifest address: {error}")))?;
        let source = Arc::new(coblet::mojang::MojangVersions::other(id, url));
        self.version_change(VersionChange::Add(source))
    }

    /// Adds a game version source written in this language after the
    /// Mojang manifest and the sources added before it.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when the
    /// ID is already used.
    pub fn add_version_source(
        &self,
        source: Arc<dyn GameVersionSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignGameVersions::new(source)?);
        self.version_change(VersionChange::Add(source))
    }

    /// Puts a game version source written in this language in place of the
    /// one with the same ID, such as `coblet:mojang`, keeping its place;
    /// appends it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_version_source(
        &self,
        source: Arc<dyn GameVersionSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignGameVersions::new(source)?);
        self.version_change(VersionChange::Replace(source))
    }

    /// Removes a game version source, such as `coblet:mojang` for a
    /// launcher that lists only its own versions; an unknown ID is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when no
    /// source is left.
    pub fn remove_version_source(&self, id: &str) -> Result<(), CobletError> {
        let id = extension_id("version source", id)?;
        self.version_change(VersionChange::Remove(id))
    }

    /// Adds a version source written in this language to the Mod Loader
    /// `loader`, asked after the loader itself and the sources added
    /// before it.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when no
    /// loader has the ID or the loader already has a source with the ID.
    pub fn add_loader_version_source(
        &self,
        loader: &str,
        source: Arc<dyn LoaderVersionSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignLoaderVersions::new(source)?);
        self.loader_source(loader, source, false)
    }

    /// Puts a version source written in this language in place of the one
    /// of `loader` with the same ID, keeping its place; appends it when the
    /// ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when no
    /// loader has the ID.
    pub fn replace_loader_version_source(
        &self,
        loader: &str,
        source: Arc<dyn LoaderVersionSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignLoaderVersions::new(source)?);
        self.loader_source(loader, source, true)
    }

    /// Adds a version source described in TOML to the Mod Loader `loader`:
    /// an `id`, a `name` and a `[versions]` table in the format of data
    /// loaders, such as a mirror's listing.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition or ID and after `build`; `build`
    /// fails like [`add_loader_version_source`](Self::add_loader_version_source).
    pub fn add_loader_version_source_toml(
        &self,
        loader: &str,
        definition: &str,
    ) -> Result<(), CobletError> {
        let source =
            Self::data_versions(coblet::loader::DataLoaderVersions::from_toml(definition))?;
        self.loader_source(loader, source, false)
    }

    /// Adds a version source described in JSON to the Mod Loader `loader`,
    /// with the keys of the TOML form.
    ///
    /// # Errors
    ///
    /// Fails like [`add_loader_version_source_toml`](Self::add_loader_version_source_toml).
    pub fn add_loader_version_source_json(
        &self,
        loader: &str,
        definition: &str,
    ) -> Result<(), CobletError> {
        let source =
            Self::data_versions(coblet::loader::DataLoaderVersions::from_json(definition))?;
        self.loader_source(loader, source, false)
    }

    /// Puts a version source described in TOML in place of the one of
    /// `loader` with the same ID, keeping its place; appends it when the ID
    /// is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition or ID and after `build`.
    pub fn replace_loader_version_source_toml(
        &self,
        loader: &str,
        definition: &str,
    ) -> Result<(), CobletError> {
        let source =
            Self::data_versions(coblet::loader::DataLoaderVersions::from_toml(definition))?;
        self.loader_source(loader, source, true)
    }

    /// Puts a version source described in JSON in place of the one of
    /// `loader` with the same ID, keeping its place; appends it when the ID
    /// is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition or ID and after `build`.
    pub fn replace_loader_version_source_json(
        &self,
        loader: &str,
        definition: &str,
    ) -> Result<(), CobletError> {
        let source =
            Self::data_versions(coblet::loader::DataLoaderVersions::from_json(definition))?;
        self.loader_source(loader, source, true)
    }

    /// Removes a version source of the Mod Loader `loader`; an unknown ID
    /// is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn remove_loader_version_source(&self, loader: &str, id: &str) -> Result<(), CobletError> {
        let loader = loader_id(loader)?;
        let id = extension_id("version source", id)?;
        self.with(|settings| {
            settings
                .loader_sources
                .push(LoaderSourceChange::Remove(loader, id));
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

    /// Adds a Java provider written in this language, asked after the
    /// built-in ones (Mojang, Adoptium, Azul Zulu) when no installed Java fits.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when the
    /// ID is already registered.
    pub fn add_java_provider(&self, provider: Arc<dyn JavaProvider>) -> Result<(), CobletError> {
        let provider = Arc::new(ForeignJavaProvider::new(provider)?);
        self.java_change(JavaChange::Provider(provider, false))
    }

    /// Puts a Java provider written in this language in place of the one
    /// with the same ID, keeping its place; appends it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_java_provider(
        &self,
        provider: Arc<dyn JavaProvider>,
    ) -> Result<(), CobletError> {
        let provider = Arc::new(ForeignJavaProvider::new(provider)?);
        self.java_change(JavaChange::Provider(provider, true))
    }

    /// Adds a Java provider described in TOML, in the format of coblet's
    /// data Java providers.
    ///
    /// # Errors
    ///
    /// Fails for a malformed definition and after `build`.
    pub fn add_java_provider_toml(&self, definition: &str) -> Result<(), CobletError> {
        let provider =
            Self::data_java_provider(coblet::java::DataJavaProvider::from_toml(definition))?;
        self.java_change(JavaChange::Provider(provider, false))
    }

    /// Adds a Java provider described in JSON, with the keys of the TOML
    /// form of [`add_java_provider_toml`](Self::add_java_provider_toml).
    ///
    /// # Errors
    ///
    /// Fails like [`add_java_provider_toml`](Self::add_java_provider_toml).
    pub fn add_java_provider_json(&self, definition: &str) -> Result<(), CobletError> {
        let provider =
            Self::data_java_provider(coblet::java::DataJavaProvider::from_json(definition))?;
        self.java_change(JavaChange::Provider(provider, false))
    }

    /// Removes a Java provider, such as `coblet:mojang`; an unknown ID is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn remove_java_provider(&self, id: &str) -> Result<(), CobletError> {
        let id = extension_id("java provider", id)?;
        self.java_change(JavaChange::RemoveProvider(id))
    }

    /// Adds a place to look for installed Java, written in this language,
    /// searched after the built-in places.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when the
    /// ID is already registered.
    pub fn add_java_locator(&self, locator: Arc<dyn JavaLocator>) -> Result<(), CobletError> {
        let locator = Arc::new(ForeignJavaLocator::new(locator)?);
        self.java_change(JavaChange::Locator(locator, false))
    }

    /// Puts a Java locator written in this language in place of the one
    /// with the same ID, keeping its place; appends it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_java_locator(&self, locator: Arc<dyn JavaLocator>) -> Result<(), CobletError> {
        let locator = Arc::new(ForeignJavaLocator::new(locator)?);
        self.java_change(JavaChange::Locator(locator, true))
    }

    /// Removes a Java locator, such as `coblet:path` to ignore `PATH`; an
    /// unknown ID is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn remove_java_locator(&self, id: &str) -> Result<(), CobletError> {
        let id = extension_id("java locator", id)?;
        self.java_change(JavaChange::RemoveLocator(id))
    }

    /// Adds a `java` executable chosen by the user, searched before every other place.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn add_java_executable(&self, executable: String) -> Result<(), CobletError> {
        self.java_change(JavaChange::Executable(executable))
    }

    /// Adds a folder holding one Java per child, such as another launcher's
    /// runtime folder; `home` leads from each child to its Java home, like
    /// `Contents/Home` on macOS or `""`.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn add_java_directory(&self, dir: String, home: String) -> Result<(), CobletError> {
        self.java_change(JavaChange::Directory(dir, home))
    }

    /// Adds a version trait source written in this language, run after the
    /// built-in table and the sources already added, so it can remove
    /// traits they gave.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when the
    /// ID is taken.
    pub fn add_trait_source(&self, source: Arc<dyn TraitSource>) -> Result<(), CobletError> {
        let source = Arc::new(ForeignTraitSource::new(source)?);
        self.trait_change(TraitChange::Add(source))
    }

    /// Puts a trait source written in this language in place of the one
    /// with the same ID, keeping its place; appends it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_trait_source(&self, source: Arc<dyn TraitSource>) -> Result<(), CobletError> {
        let source = Arc::new(ForeignTraitSource::new(source)?);
        self.trait_change(TraitChange::Replace(source))
    }

    /// Adds a version trait table written in TOML, in the format of
    /// coblet's built-in table (see `builtin_version_traits_toml`).
    ///
    /// # Errors
    ///
    /// Fails for a malformed table and after `build`; `build` fails when
    /// the ID is taken.
    pub fn add_trait_table_toml(&self, table: &str) -> Result<(), CobletError> {
        let source = Self::trait_table(coblet::mojang::TraitTable::from_toml(table))?;
        self.trait_change(TraitChange::Add(source))
    }

    /// Adds a version trait table written in JSON, with the keys of the
    /// TOML form of [`add_trait_table_toml`](Self::add_trait_table_toml).
    ///
    /// # Errors
    ///
    /// Fails like [`add_trait_table_toml`](Self::add_trait_table_toml).
    pub fn add_trait_table_json(&self, table: &str) -> Result<(), CobletError> {
        let source = Self::trait_table(coblet::mojang::TraitTable::from_json(table))?;
        self.trait_change(TraitChange::Add(source))
    }

    /// Puts a trait table written in TOML in place of the source with the
    /// same ID, such as the built-in `coblet:version-traits`.
    ///
    /// # Errors
    ///
    /// Fails for a malformed table and after `build`.
    pub fn replace_trait_table_toml(&self, table: &str) -> Result<(), CobletError> {
        let source = Self::trait_table(coblet::mojang::TraitTable::from_toml(table))?;
        self.trait_change(TraitChange::Replace(source))
    }

    /// Puts a trait table written in JSON in place of the source with the
    /// same ID.
    ///
    /// # Errors
    ///
    /// Fails like [`replace_trait_table_toml`](Self::replace_trait_table_toml).
    pub fn replace_trait_table_json(&self, table: &str) -> Result<(), CobletError> {
        let source = Self::trait_table(coblet::mojang::TraitTable::from_json(table))?;
        self.trait_change(TraitChange::Replace(source))
    }

    /// Removes a version trait source, such as the built-in table
    /// `coblet:version-traits`; an unknown ID is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn remove_trait_source(&self, id: &str) -> Result<(), CobletError> {
        let id = extension_id("trait source", id)?;
        self.trait_change(TraitChange::Remove(id))
    }

    /// Sets `${name}` to `value` in every launch; `LaunchOptions.placeholders`,
    /// customizers and steps can replace it.
    ///
    /// # Errors
    ///
    /// Fails for a name coblet fills itself, such as `auth_access_token`,
    /// an empty name or one with `$`, `{` or `}`, and after `build`.
    pub fn add_placeholder(&self, name: String, value: String) -> Result<(), CobletError> {
        placeholder_name(&name)?;
        self.launch_change(LaunchChange::Placeholder(name, value))
    }

    /// Adds a source asked for placeholders nothing else set, after those
    /// already added.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`; `build` fails when the
    /// ID is taken.
    pub fn add_placeholder_source(
        &self,
        source: Arc<dyn PlaceholderSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignPlaceholderSource::new(source)?);
        self.launch_change(LaunchChange::Source(source, false))
    }

    /// Puts a placeholder source in place of the one with the same ID,
    /// keeping its place; appends it when the ID is free.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn replace_placeholder_source(
        &self,
        source: Arc<dyn PlaceholderSource>,
    ) -> Result<(), CobletError> {
        let source = Arc::new(ForeignPlaceholderSource::new(source)?);
        self.launch_change(LaunchChange::Source(source, true))
    }

    /// Removes a placeholder source; an unknown ID is ignored.
    ///
    /// # Errors
    ///
    /// Fails for a malformed ID and after `build`.
    pub fn remove_placeholder_source(&self, id: &str) -> Result<(), CobletError> {
        let id = extension_id("placeholder source", id)?;
        self.launch_change(LaunchChange::RemoveSource(id))
    }

    /// Sets what happens to placeholders nothing fills; the launch fails by default.
    ///
    /// # Errors
    ///
    /// Fails after `build`.
    pub fn set_unknown_placeholders(&self, policy: UnknownPlaceholders) -> Result<(), CobletError> {
        self.launch_change(LaunchChange::Unknown(policy))
    }

    /// Creates the `Coblet`.
    ///
    /// # Errors
    ///
    /// Fails when called twice, when the transport cannot start, and when
    /// two Java providers, two Java locators or two version trait sources
    /// share an ID.
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
        for change in settings.version_sources {
            builder = match change {
                VersionChange::Add(source) => builder.version_source(source),
                VersionChange::Replace(source) => builder.replace_version_source(source),
                VersionChange::Remove(id) => builder.without_version_source(id),
            };
        }
        for (loader, replace) in settings.loaders {
            builder = if replace {
                builder.replace_loader(loader)
            } else {
                builder.loader(loader)
            };
        }
        for change in settings.loader_sources {
            builder = match change {
                LoaderSourceChange::Add(loader, source, false) => {
                    builder.loader_version_source(loader, source)
                }
                LoaderSourceChange::Add(loader, source, true) => {
                    builder.replace_loader_version_source(loader, source)
                }
                LoaderSourceChange::Remove(loader, id) => {
                    builder.without_loader_version_source(loader, id)
                }
            };
        }
        for change in settings.java {
            builder = match change {
                JavaChange::Provider(provider, false) => builder.java_provider(provider),
                JavaChange::Provider(provider, true) => builder.replace_java_provider(provider),
                JavaChange::RemoveProvider(id) => builder.without_java_provider(id),
                JavaChange::Locator(locator, false) => builder.java_locator(locator),
                JavaChange::Locator(locator, true) => builder.replace_java_locator(locator),
                JavaChange::RemoveLocator(id) => builder.without_java_locator(id),
                JavaChange::Executable(path) => builder.java_executable(path),
                JavaChange::Directory(dir, home) => builder.java_directory(dir, home),
            };
        }
        for change in settings.traits {
            builder = match change {
                TraitChange::Add(source) => builder.trait_source(source),
                TraitChange::Replace(source) => builder.replace_trait_source(source),
                TraitChange::Remove(id) => builder.without_trait_source(id),
            };
        }
        for change in settings.launch {
            builder = match change {
                LaunchChange::Placeholder(name, value) => builder.placeholder(name, value),
                LaunchChange::Source(source, false) => builder.placeholder_source(source),
                LaunchChange::Source(source, true) => builder.replace_placeholder_source(source),
                LaunchChange::RemoveSource(id) => builder.without_placeholder_source(id),
                LaunchChange::Unknown(policy) => builder.unknown_placeholders(policy.into()),
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
        let error = builder
            .add_version_manifest("no namespace", url.clone())
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        let error = builder
            .add_version_manifest("acme:other", "ftp://x".to_owned())
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        builder.build().unwrap();

        for id in ["acme:experiments", "coblet:mojang"] {
            let builder = self::builder();
            builder
                .add_version_manifest("acme:experiments", url.clone())
                .unwrap();
            builder.add_version_manifest(id, url.clone()).unwrap();
            let error = builder.build().unwrap_err();
            assert_eq!(kind(&error), ErrorKind::InvalidArgument, "{id}");
        }
    }
}
