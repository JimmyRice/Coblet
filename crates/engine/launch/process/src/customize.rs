use std::sync::{Arc, Mutex, PoisonError};

use coblet_mojang::VersionTraits;
use coblet_platform::EffectiveLaunchProfile;
use coblet_runtime::Extension;
use coblet_types::Platform;

use crate::{GameLayout, LaunchError, LaunchStep};

/// Downstream hook that edits the launch after the profile pipeline and
/// before the command is built.
///
/// A customizer can replace the main class, add libraries, add a
/// `-javaagent` or other JVM arguments, and register extra launch steps
/// through [`CustomizeCtx`]. coblet records every difference it makes to
/// the profile in [`EffectiveLaunchProfile::explain`]. Customizers run only
/// when a [`LaunchRequest`](crate::LaunchRequest) names them.
///
/// # Examples
///
/// ```
/// use coblet_launch::{CustomizeCtx, LaunchCustomizer, LaunchError};
/// use coblet_mojang::{VersionTrait, VersionTraits};
/// use coblet_platform::{EffectiveLaunchProfile, ProfileArg};
/// use coblet_runtime::Extension;
/// use coblet_types::ExtensionId;
///
/// struct Proxy(ExtensionId);
///
/// impl Extension for Proxy {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Legacy proxy" }
/// }
///
/// impl LaunchCustomizer for Proxy {
///     fn applies_to(&self, traits: &VersionTraits) -> bool {
///         traits.contains(&VersionTrait::LegacyOnlineServices)
///     }
///
///     fn customize(
///         &self,
///         profile: &mut EffectiveLaunchProfile,
///         _ctx: &CustomizeCtx,
///     ) -> Result<(), LaunchError> {
///         profile.jvm_args.push(ProfileArg::always("-Dhttp.proxyHost=127.0.0.1"));
///         Ok(())
///     }
/// }
/// ```
pub trait LaunchCustomizer: Extension {
    /// Reports whether the customizer handles a version with these traits.
    fn applies_to(&self, traits: &VersionTraits) -> bool;

    /// Edits the profile of one launch.
    ///
    /// # Errors
    ///
    /// Returns an error to stop the launch; it reaches the caller unchanged.
    fn customize(
        &self,
        profile: &mut EffectiveLaunchProfile,
        ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError>;
}

/// Where an extra step goes relative to its anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    Before,
    After,
}

/// Launch step registered by a customizer, placed next to a named step.
pub(crate) struct ExtraStep {
    pub(crate) anchor: String,
    pub(crate) placement: Placement,
    pub(crate) step: Arc<dyn LaunchStep>,
}

/// What a [`LaunchCustomizer`] can see and add besides the profile.
#[non_exhaustive]
pub struct CustomizeCtx {
    /// The machine the game runs on.
    pub platform: Platform,
    /// File locations of this launch.
    pub layout: GameLayout,
    /// Version traits as the profile pipeline left them.
    pub traits: VersionTraits,
    steps: Mutex<Vec<ExtraStep>>,
}

impl std::fmt::Debug for CustomizeCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let steps = self.lock();
        let names: Vec<String> = steps
            .iter()
            .map(|extra| {
                format!(
                    "{} {:?} {}",
                    extra.step.name(),
                    extra.placement,
                    extra.anchor
                )
            })
            .collect();
        f.debug_struct("CustomizeCtx")
            .field("platform", &self.platform)
            .field("layout", &self.layout)
            .field("traits", &self.traits)
            .field("steps", &names)
            .finish()
    }
}

impl CustomizeCtx {
    /// Creates a context with no extra steps.
    #[must_use]
    pub fn new(platform: Platform, layout: GameLayout, traits: VersionTraits) -> Self {
        Self {
            platform,
            layout,
            traits,
            steps: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ExtraStep>> {
        self.steps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, anchor: String, placement: Placement, step: Arc<dyn LaunchStep>) {
        self.lock().push(ExtraStep {
            anchor,
            placement,
            step,
        });
    }

    /// Runs `step` in this launch right before the step named `anchor`.
    ///
    /// The launch fails with [`LaunchError::UnknownStep`] when no step has
    /// that name.
    pub fn add_step_before(&self, anchor: impl Into<String>, step: Arc<dyn LaunchStep>) {
        self.push(anchor.into(), Placement::Before, step);
    }

    /// Runs `step` in this launch right after the step named `anchor`.
    ///
    /// The launch fails with [`LaunchError::UnknownStep`] when no step has
    /// that name.
    pub fn add_step_after(&self, anchor: impl Into<String>, step: Arc<dyn LaunchStep>) {
        self.push(anchor.into(), Placement::After, step);
    }

    /// Returns the names of the extra steps registered so far, in order.
    #[must_use]
    pub fn extra_steps(&self) -> Vec<String> {
        self.lock()
            .iter()
            .map(|extra| extra.step.name().to_owned())
            .collect()
    }

    pub(crate) fn into_steps(self) -> Vec<ExtraStep> {
        self.steps
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
