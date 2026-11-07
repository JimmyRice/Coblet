use coblet_platform::ComponentRef;
use coblet_runtime::{BoxFuture, Extension};
use coblet_types::{ExtensionId, GameVersionId};

use crate::{InstallRecipe, LoaderContext, LoaderError, LoaderVersion};

/// A Mod Loader: lists its versions for a game version and describes what
/// installing one adds to an Instance.
///
/// Built-in loaders such as Fabric and Forge and third-party loaders
/// implement this same trait and live in the same
/// [`LoaderRegistry`](crate::LoaderRegistry); a third-party loader can even
/// replace a built-in one by registering under its ID.
pub trait ModLoader: Extension {
    /// Returns the UID of the component that identifies an installed copy,
    /// such as `net.fabricmc.fabric-loader`.
    fn component_uid(&self) -> &str;

    /// Lists the versions for a game version, setting
    /// [`LoaderVersion::recommended`] on the one to preselect; an empty list
    /// means the loader does not support the game version.
    fn versions<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
    ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>>;

    /// Describes what installing `version` for `game` adds to an Instance.
    ///
    /// The recipe must contain a component with [`component_uid`](Self::component_uid).
    fn install<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
        version: &'a LoaderVersion,
    ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>>;

    /// Returns the installed version when the stack contains this loader;
    /// looks for [`component_uid`](Self::component_uid) by default.
    fn detect(&self, components: &[ComponentRef]) -> Option<String> {
        components
            .iter()
            .find(|component| component.uid == self.component_uid())
            .map(|component| component.version.clone())
    }

    /// Returns the loaders whose content (Mods) this loader runs, itself
    /// included; Quilt, for example, also runs Fabric Mods.
    fn accepted_content_loaders(&self) -> Vec<ExtensionId> {
        vec![self.id().clone()]
    }
}
