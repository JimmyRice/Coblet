use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use coblet::launch::{LaunchError, PlaceholderCtx};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::ExtensionId;

use crate::customize::trait_names;
use crate::java::extension_id;
use crate::{Coblet, CobletError, ErrorKind};

/// What happens to a `${name}` placeholder nothing fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum UnknownPlaceholders {
    /// The launch fails.
    Error,
    /// `${name}` stays as written.
    Keep,
    /// It is replaced with nothing.
    Empty,
}

impl From<UnknownPlaceholders> for coblet::launch_command::UnknownPlaceholder {
    fn from(policy: UnknownPlaceholders) -> Self {
        match policy {
            UnknownPlaceholders::Error => Self::Error,
            UnknownPlaceholders::Keep => Self::Keep,
            UnknownPlaceholders::Empty => Self::Empty,
        }
    }
}

/// What a [`PlaceholderSource`] can see of the launch.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PlaceholderInfo {
    /// The game version ID.
    pub game_version: String,
    /// Its traits, such as `applet`.
    pub traits: Vec<String>,
    /// Operating system, such as `macos`.
    pub os: String,
    /// Architecture, such as `aarch64`.
    pub arch: String,
    /// Directory for shared game files.
    pub data_root: String,
    /// Working directory of the game.
    pub game_dir: String,
    /// Major version of the chosen Java, once `select-java` ran.
    pub java_major: Option<u32>,
}

impl From<&PlaceholderCtx> for PlaceholderInfo {
    fn from(cx: &PlaceholderCtx) -> Self {
        Self {
            game_version: cx.game_version.as_str().to_owned(),
            traits: trait_names(&cx.traits),
            os: cx.platform.os().as_str().to_owned(),
            arch: cx.platform.arch().as_str().to_owned(),
            data_root: cx.layout.data_root().to_string_lossy().into_owned(),
            game_dir: cx.layout.game_directory().to_string_lossy().into_owned(),
            java_major: cx.java_major,
        }
    }
}

/// Source of `${name}` placeholder values written in Swift, Kotlin or
/// Python, asked for placeholders nothing else in a launch set.
///
/// Sources are asked in order, each only for the names still missing.
/// `resolve` runs on a worker thread and may block.
#[uniffi::export(foreign)]
pub trait PlaceholderSource: Send + Sync {
    /// Returns the ID, such as `acme:server`; read once at registration.
    fn id(&self) -> String;

    /// Returns the name shown to users; read once at registration.
    fn display_name(&self) -> String;

    /// Returns values for those of `names` the source knows and leaves out
    /// the others.
    ///
    /// # Errors
    ///
    /// An error stops the launch.
    fn resolve(
        &self,
        names: Vec<String>,
        info: PlaceholderInfo,
    ) -> Result<HashMap<String, String>, CobletError>;
}

/// Placeholder source known to a [`Coblet`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PlaceholderSourceInfo {
    /// ID, such as `acme:server`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

/// Runs a foreign placeholder source as a Rust one.
pub(crate) struct ForeignPlaceholderSource {
    id: ExtensionId,
    display_name: String,
    inner: Arc<dyn PlaceholderSource>,
}

impl ForeignPlaceholderSource {
    pub(crate) fn new(inner: Arc<dyn PlaceholderSource>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("placeholder source", &inner.id())?,
            display_name: inner.display_name(),
            inner,
        })
    }
}

impl Extension for ForeignPlaceholderSource {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl coblet::launch::PlaceholderSource for ForeignPlaceholderSource {
    fn resolve<'a>(
        &'a self,
        names: &'a [String],
        cx: &'a PlaceholderCtx,
    ) -> BoxFuture<'a, Result<BTreeMap<String, String>, LaunchError>> {
        let inner = Arc::clone(&self.inner);
        let names = names.to_vec();
        let info = PlaceholderInfo::from(cx);
        Box::pin(async move {
            let failed = |message: String| LaunchError::PlaceholderSource {
                source_id: self.id.clone(),
                message,
            };
            let answers = tokio::task::spawn_blocking(move || inner.resolve(names, info))
                .await
                .map_err(|error| failed(format!("the source stopped: {error}")))?
                .map_err(|error| failed(error.to_string()))?;
            Ok(answers.into_iter().collect())
        })
    }
}

/// Checks a placeholder name the way the launcher will.
pub(crate) fn placeholder_name(name: &str) -> Result<(), CobletError> {
    coblet::launch::check_placeholder_name(name)
        .map_err(|error| CobletError::new(ErrorKind::InvalidArgument, error.to_string()))
}

#[uniffi::export]
impl Coblet {
    /// Lists the placeholder sources, in the order they are asked.
    #[must_use]
    pub fn placeholder_sources(&self) -> Vec<PlaceholderSourceInfo> {
        self.inner
            .placeholder_sources()
            .iter()
            .map(|source| PlaceholderSourceInfo {
                id: source.id().as_str().to_owned(),
                name: source.display_name().to_owned(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use coblet::launch::{
        LaunchError, LaunchRequest, Launcher, PlaceholderOrigin, PlaceholderSources,
        ResolvePlaceholders,
    };
    use coblet::launch_command::LaunchAuth;
    use coblet::types::{ExtensionId, PlayerName};

    use super::{ForeignPlaceholderSource, PlaceholderInfo, PlaceholderSource, placeholder_name};
    use crate::customize::ForeignCustomizer;
    use crate::testing::launch_context;
    use crate::{CobletError, ErrorKind, LaunchCustomizer, ProfileEditor};

    /// Behaves like a source written in Python.
    struct Server {
        asked: Mutex<Vec<(Vec<String>, PlaceholderInfo)>>,
        fail: bool,
    }

    impl PlaceholderSource for Server {
        fn id(&self) -> String {
            "acme:server".to_owned()
        }

        fn display_name(&self) -> String {
            "Server".to_owned()
        }

        fn resolve(
            &self,
            names: Vec<String>,
            info: PlaceholderInfo,
        ) -> Result<HashMap<String, String>, CobletError> {
            self.asked.lock().unwrap().push((names, info));
            if self.fail {
                return Err(CobletError::new(ErrorKind::Other, "server is down"));
            }
            Ok(HashMap::from([
                ("acme_server".to_owned(), "play.acme.net".to_owned()),
                ("acme_unasked".to_owned(), "x".to_owned()),
            ]))
        }
    }

    /// Customizer that adds an argument with placeholders and sets one.
    struct Uses;

    impl LaunchCustomizer for Uses {
        fn id(&self) -> String {
            "acme:uses".to_owned()
        }

        fn display_name(&self) -> String {
            "Uses".to_owned()
        }

        fn applies_to(&self, _traits: Vec<String>) -> bool {
            true
        }

        fn customize(&self, editor: Arc<ProfileEditor>) -> Result<(), CobletError> {
            editor.add_jvm_arg("-Dacme=${acme_server}/${acme_user}".to_owned(), false)?;
            editor.set_placeholder("acme_user".to_owned(), "Steve".to_owned())?;
            let error = editor
                .set_placeholder("auth_access_token".to_owned(), "x".to_owned())
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidArgument);
            Ok(())
        }
    }

    async fn run(server: Arc<Server>) -> Result<coblet::launch::LaunchContext, LaunchError> {
        let mut launcher = Launcher::empty().with_step(Arc::new(ResolvePlaceholders));
        launcher
            .register_customizer(Arc::new(ForeignCustomizer::new(Arc::new(Uses)).unwrap()))
            .unwrap();
        let mut sources = PlaceholderSources::new();
        sources
            .register(Arc::new(ForeignPlaceholderSource::new(server).unwrap()))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
            .with_customizer(ExtensionId::new("acme:uses").unwrap());
        let mut cx = launch_context(dir.path(), "1.20.1", request);
        cx.services.placeholder_sources = sources;
        launcher.run(&mut cx).await?;
        Ok(cx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_sources_fill_what_customizers_left_missing() {
        let server = Arc::new(Server {
            asked: Mutex::new(Vec::new()),
            fail: false,
        });
        let cx = run(Arc::clone(&server)).await.unwrap();
        assert_eq!(cx.placeholders.get("acme_server"), Some("play.acme.net"));
        assert_eq!(cx.placeholders.get("acme_unasked"), None);
        assert_eq!(
            cx.placeholders.origin("acme_user"),
            Some(&PlaceholderOrigin::Customizer(
                ExtensionId::new("acme:uses").unwrap()
            ))
        );
        let asked = server.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, ["acme_server"]);
        assert_eq!(asked[0].1.game_version, "1.20.1");
        assert_eq!(asked[0].1.os, "linux");
        assert_eq!(asked[0].1.java_major, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_foreign_source_stops_the_launch() {
        let server = Arc::new(Server {
            asked: Mutex::new(Vec::new()),
            fail: true,
        });
        let error = run(server).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "placeholder source `acme:server` failed: server is down"
        );
    }

    #[test]
    fn names_are_checked_before_the_launch() {
        assert!(placeholder_name("acme_server").is_ok());
        for name in ["", "classpath", "a{b"] {
            assert_eq!(
                placeholder_name(name).unwrap_err().kind(),
                ErrorKind::InvalidArgument
            );
        }
    }
}
