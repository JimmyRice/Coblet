use std::sync::Arc;

use crate::{GameSession, LaunchContext, LaunchError, LaunchStep, builtin_steps};

/// Ordered chain of [`LaunchStep`]s that prepares and starts the game.
///
/// [`Launcher::new`] holds the built-in steps; steps can be inserted,
/// replaced or removed by name.
#[derive(Clone)]
pub struct Launcher {
    steps: Vec<Arc<dyn LaunchStep>>,
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
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
        }
    }

    /// Creates a launcher with no steps.
    #[must_use]
    pub fn empty() -> Self {
        Self { steps: Vec::new() }
    }

    /// Returns the step names in order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.steps.iter().map(|step| step.name()).collect()
    }

    fn position(&self, name: &str) -> Result<usize, LaunchError> {
        self.steps
            .iter()
            .position(|step| step.name() == name)
            .ok_or_else(|| LaunchError::UnknownStep(name.to_owned()))
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

    /// Runs every step on `cx`, stopping at the first failure or cancellation.
    ///
    /// # Errors
    ///
    /// Returns the failure of the first step that fails, or
    /// [`LaunchError::Cancelled`].
    pub async fn run(&self, cx: &mut LaunchContext) -> Result<(), LaunchError> {
        for step in &self.steps {
            if cx.cancel.is_cancelled() {
                return Err(LaunchError::Cancelled);
            }
            tracing::debug!(step = step.name(), "running launch step");
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

#[cfg(test)]
#[cfg(unix)]
#[path = "launcher_tests.rs"]
mod tests;
