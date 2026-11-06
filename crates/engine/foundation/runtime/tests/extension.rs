use std::sync::Arc;

use coblet_types::ExtensionId;

use super::{Extension, Registry, RegistryError};

trait Loader: Extension {
    fn kind(&self) -> &'static str;
}

struct Fake {
    id: ExtensionId,
    kind: &'static str,
}

impl Extension for Fake {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        self.kind
    }
}

impl Loader for Fake {
    fn kind(&self) -> &'static str {
        self.kind
    }
}

fn fake(id: &str, kind: &'static str) -> Arc<dyn Loader> {
    Arc::new(Fake {
        id: ExtensionId::new(id).unwrap(),
        kind,
    })
}

#[test]
fn rejects_duplicates_and_keeps_the_first() {
    let mut loaders: Registry<dyn Loader> = Registry::new();
    loaders.register(fake("coblet:fabric", "builtin")).unwrap();
    let id = ExtensionId::new("coblet:fabric").unwrap();
    assert_eq!(
        loaders.register(fake("coblet:fabric", "impostor")),
        Err(RegistryError::Duplicate(id.clone()))
    );
    assert_eq!(loaders.get(&id).unwrap().kind(), "builtin");
}

#[test]
fn iterates_in_id_order() {
    let mut loaders: Registry<dyn Loader> = Registry::default();
    for id in ["coblet:quilt", "acme:legacy", "coblet:fabric"] {
        loaders.register(fake(id, "x")).unwrap();
    }
    let ids: Vec<&str> = loaders.iter().map(|loader| loader.id().as_str()).collect();
    assert_eq!(ids, ["acme:legacy", "coblet:fabric", "coblet:quilt"]);
    assert_eq!(format!("{loaders:?}").matches("ExtensionId").count(), 3);
}
