use std::collections::BTreeSet;
use std::sync::Arc;

use coblet_runtime::{Event, EventBus, OperationOutcome, Registry, RegistryError};
use coblet_types::ExtensionId;

use crate::customize::Placement;
use crate::{
    CustomizeCtx, GameSession, LaunchContext, LaunchCustomizer, LaunchError, LaunchStep,
    builtin_steps, diff,
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
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launcher")
            .field("steps", &self.names())
            .field("customizers", &self.customizers)
            .field("auth_grants", &self.auth_grants)
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
        }
    }

    /// Creates a launcher with no steps.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            steps: Vec::new(),
            customizers: Registry::new(),
            auth_grants: BTreeSet::new(),
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
#[path = "../tests/launcher.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/launcher/customize.rs"]
mod customize_tests;
