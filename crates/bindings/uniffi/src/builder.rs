use std::sync::{Arc, Mutex, PoisonError};

use coblet::platform::{DataProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{ExtensionId, Platform};

use crate::customize::ForeignCustomizer;
use crate::profile::ForeignProfile;
use crate::{Coblet, CobletError, ErrorKind, LaunchCustomizer, PlatformInfo, PlatformProfile};

struct Settings {
    data_root: String,
    user_agent: UserAgent,
    customizers: Vec<ForeignCustomizer>,
    auth_grants: Vec<ExtensionId>,
    profiles: Vec<Arc<dyn coblet::platform::PlatformProfile>>,
    platform: Option<Platform>,
}

/// Settings of a [`Coblet`] before it is built: Platform Profiles and
/// launch customizers.
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
        Ok(Arc::new(Coblet::from_inner(builder.build()?)))
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
