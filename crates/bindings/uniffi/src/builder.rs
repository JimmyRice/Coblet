use std::sync::{Arc, Mutex, PoisonError};

use coblet::auth::{EncryptedFileStore, MsaClientId};
use coblet::platform::{DataProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{ExtensionId, Platform};

use crate::customize::ForeignCustomizer;
use crate::profile::ForeignProfile;
use crate::secrets::{ForeignSecrets, encryption_key};
use crate::sources::ForeignRewriter;
use crate::{
    ArtifactKind, Coblet, CobletError, ErrorKind, LaunchCustomizer, PlatformInfo, PlatformProfile,
    SecretStore, UrlRewriter,
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
#[path = "../tests/builder.rs"]
mod tests;
