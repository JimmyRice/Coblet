use std::collections::BTreeMap;
use std::path::Path;

use super::{Crate, DepKind, Dependency, Location, check_crate, locate};

const NONE: [&str; 0] = [];

fn engine(name: &str, group: &str, leaf: &str) -> Crate {
    Crate {
        name: name.to_owned(),
        location: Location::Engine {
            group: group.to_owned(),
            leaf: leaf.to_owned(),
        },
    }
}

fn workspace() -> BTreeMap<String, Crate> {
    [
        engine("coblet-types", "foundation", "types"),
        engine("coblet-runtime", "foundation", "runtime"),
        engine("coblet-mojang", "metadata", "mojang"),
        engine("coblet-launch-command", "launch", "command"),
        engine("coblet-launch", "launch", "process"),
        engine("coblet-instance", "instance", "model"),
        engine("coblet-content", "content", "api"),
        engine("coblet-provider-modrinth", "providers", "modrinth"),
        engine("coblet", "facade", "coblet"),
        Crate {
            name: "coblet-ffi".to_owned(),
            location: Location::Bindings,
        },
        Crate {
            name: "xtask".to_owned(),
            location: Location::Tooling,
        },
    ]
    .into_iter()
    .map(|krate| (krate.name.clone(), krate))
    .collect()
}

fn reasons(from: &str, to: &[&str]) -> Vec<String> {
    let ws = workspace();
    let deps: Vec<Dependency> = to
        .iter()
        .map(|name| Dependency {
            name: (*name).to_owned(),
            kind: DepKind::Normal,
        })
        .collect();
    check_crate(&ws[from], &deps, &ws)
        .into_iter()
        .map(|violation| format!("{} {}", violation.to, violation.reason))
        .collect()
}

#[test]
fn allows_downward_and_same_layer_edges() {
    assert_eq!(
        reasons(
            "coblet-launch",
            &["coblet-launch-command", "coblet-mojang", "coblet-types"]
        ),
        NONE
    );
    assert_eq!(
        reasons("coblet", &["coblet-provider-modrinth", "coblet-instance"]),
        NONE
    );
    assert_eq!(reasons("coblet-ffi", &["coblet", "libc"]), NONE);
}

#[test]
fn rejects_upward_edges() {
    assert_eq!(
        reasons("coblet-mojang", &["coblet-instance"]),
        ["coblet-instance Base layer must not depend on the higher Instance layer"]
    );
}

#[test]
fn keeps_launch_command_minimal() {
    assert_eq!(
        reasons(
            "coblet-launch-command",
            &["coblet-types", "coblet-runtime", "tokio", "serde"]
        ),
        [
            "coblet-runtime coblet-launch-command may only depend on coblet-types",
            "tokio coblet-launch-command must stay free of IO, async and HTTP crates",
        ]
    );
}

#[test]
fn isolates_providers() {
    assert_eq!(
        reasons("coblet-instance", &["coblet-provider-modrinth"]),
        ["coblet-provider-modrinth only the facade crate may depend on providers"]
    );
    assert_eq!(
        reasons(
            "coblet-provider-modrinth",
            &["coblet-content", "coblet-types"]
        ),
        NONE
    );
    assert_eq!(
        reasons("coblet-provider-modrinth", &["coblet-mojang"]),
        ["coblet-mojang providers may only depend on foundation and api crates"]
    );
}

#[test]
fn restricts_bindings_to_the_facade() {
    assert_eq!(
        reasons("coblet-ffi", &["coblet-types"]),
        ["coblet-types bindings may only depend on the facade crate `coblet`"]
    );
}

#[test]
fn locates_crates_by_manifest_path() {
    assert_eq!(
        locate(Path::new("crates/engine/foundation/types/Cargo.toml")),
        Some(Location::Engine {
            group: "foundation".to_owned(),
            leaf: "types".to_owned()
        })
    );
    assert_eq!(
        locate(Path::new("crates/bindings/ffi/c/Cargo.toml")),
        Some(Location::Bindings)
    );
    assert_eq!(
        locate(Path::new("xtask/Cargo.toml")),
        Some(Location::Tooling)
    );
    assert_eq!(locate(Path::new("src/Cargo.toml")), None);
}
