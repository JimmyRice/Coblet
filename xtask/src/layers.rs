//! `check-layers`: dependencies between workspace crates may only point down
//! the layer stack described in spec section 3.2.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// The facade crate, the only workspace crate that bindings may depend on.
const FACADE: &str = "coblet";

/// The minimal command builder, which must stay free of IO and async code.
const LAUNCH_COMMAND: &str = "coblet-launch-command";

/// Workspace crates that `coblet-launch-command` may depend on.
const LAUNCH_COMMAND_WORKSPACE_DEPS: &[&str] = &["coblet-types"];

/// Third-party crates that would pull IO, async runtimes or HTTP into `coblet-launch-command`.
const LAUNCH_COMMAND_FORBIDDEN: &[&str] = &[
    "async-std",
    "futures",
    "futures-core",
    "futures-util",
    "hyper",
    "reqwest",
    "smol",
    "tokio",
    "ureq",
];

/// Where a workspace crate lives, derived from its manifest path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    /// `crates/engine/<group>/<leaf>/`.
    Engine { group: String, leaf: String },
    /// `crates/bindings/**`.
    Bindings,
    /// `xtask/` and `fuzz/`, which never ship and are not layered.
    Tooling,
}

/// Position of a crate in the layer stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Foundation,
    Base,
    Capability,
    Instance,
    Content,
    Facade,
    Bindings,
}

/// A workspace crate together with its location.
#[derive(Clone, Debug)]
pub(crate) struct Crate {
    pub(crate) name: String,
    pub(crate) location: Location,
}

/// The kind of a dependency edge, as reported by `cargo metadata`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DepKind {
    Normal,
    Dev,
    Build,
}

/// A dependency of a workspace crate.
#[derive(Clone, Debug)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    pub(crate) kind: DepKind,
}

/// A rule broken by one dependency edge.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) reason: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  {} -> {}: {}", self.from, self.to, self.reason)
    }
}

/// Reads the workspace graph from `cargo metadata` and checks every edge.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let metadata = cargo_metadata(root)?;
    let packages = parse_packages(root, &metadata)?;
    let crates: BTreeMap<String, Crate> = packages
        .iter()
        .map(|(krate, _)| (krate.name.clone(), krate.clone()))
        .collect();
    let violations: Vec<Violation> = packages
        .iter()
        .flat_map(|(krate, deps)| check_crate(krate, deps, &crates))
        .collect();
    if violations.is_empty() {
        println!("check-layers: {} workspace crates ok", crates.len());
        return Ok(());
    }
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    Err(format!(
        "{} layering violation(s):\n{}",
        violations.len(),
        report.join("\n")
    ))
}

/// Checks the dependencies of one crate against the layering rules.
pub(crate) fn check_crate(
    krate: &Crate,
    deps: &[Dependency],
    workspace: &BTreeMap<String, Crate>,
) -> Vec<Violation> {
    deps.iter()
        .filter_map(|dep| {
            let reason = match workspace.get(&dep.name) {
                Some(target) => internal_edge(krate, target),
                None => external_edge(krate, dep),
            }?;
            Some(Violation {
                from: krate.name.clone(),
                to: dep.name.clone(),
                reason,
            })
        })
        .collect()
}

fn internal_edge(from: &Crate, to: &Crate) -> Option<String> {
    if from.location == Location::Tooling {
        return None;
    }
    if to.location == Location::Tooling {
        return Some("tooling crates must not be dependencies".to_owned());
    }
    if from.location == Location::Bindings {
        return (to.name != FACADE)
            .then(|| format!("bindings may only depend on the facade crate `{FACADE}`"));
    }
    if from.name == LAUNCH_COMMAND {
        return (!LAUNCH_COMMAND_WORKSPACE_DEPS.contains(&to.name.as_str())).then(|| {
            format!(
                "{LAUNCH_COMMAND} may only depend on {}",
                LAUNCH_COMMAND_WORKSPACE_DEPS.join(", ")
            )
        });
    }
    if is_provider(to) && rank(from) != Some(Rank::Facade) {
        return Some("only the facade crate may depend on providers".to_owned());
    }
    if is_provider(from) {
        return (!(rank(to) == Some(Rank::Foundation) || is_api(to)))
            .then(|| "providers may only depend on foundation and api crates".to_owned());
    }
    match (rank(from), rank(to)) {
        (Some(from_rank), Some(to_rank)) if to_rank > from_rank => Some(format!(
            "{from_rank:?} layer must not depend on the higher {to_rank:?} layer"
        )),
        (Some(_), Some(_)) => None,
        (None, _) => Some(format!("{} sits in an unknown layer", from.name)),
        (_, None) => Some(format!("{} sits in an unknown layer", to.name)),
    }
}

fn external_edge(from: &Crate, dep: &Dependency) -> Option<String> {
    let forbidden = from.name == LAUNCH_COMMAND
        && dep.kind != DepKind::Dev
        && LAUNCH_COMMAND_FORBIDDEN.contains(&dep.name.as_str());
    forbidden.then(|| format!("{LAUNCH_COMMAND} must stay free of IO, async and HTTP crates"))
}

fn rank(krate: &Crate) -> Option<Rank> {
    match &krate.location {
        Location::Bindings => Some(Rank::Bindings),
        Location::Tooling => None,
        Location::Engine { group, .. } => match group.as_str() {
            "foundation" => Some(Rank::Foundation),
            "metadata" | "platform" | "transfer" => Some(Rank::Base),
            "launch" | "auth" | "loaders" | "server" => Some(Rank::Capability),
            "instance" => Some(Rank::Instance),
            // Providers implement content and packaging traits and are wired in by the facade.
            "content" | "packaging" | "providers" => Some(Rank::Content),
            "facade" => Some(Rank::Facade),
            _ => None,
        },
    }
}

fn is_provider(krate: &Crate) -> bool {
    matches!(&krate.location, Location::Engine { group, .. } if group == "providers")
}

fn is_api(krate: &Crate) -> bool {
    matches!(&krate.location, Location::Engine { leaf, .. } if leaf == "api")
}

/// Classifies a manifest path relative to the workspace root.
pub(crate) fn locate(relative_manifest: &Path) -> Option<Location> {
    let parts: Vec<&str> = relative_manifest
        .iter()
        .filter_map(|part| part.to_str())
        .collect();
    match parts.as_slice() {
        ["crates", "engine", group, leaf, "Cargo.toml"] => Some(Location::Engine {
            group: (*group).to_owned(),
            leaf: (*leaf).to_owned(),
        }),
        ["crates", "bindings", .., "Cargo.toml"] => Some(Location::Bindings),
        ["xtask" | "fuzz", ..] => Some(Location::Tooling),
        _ => None,
    }
}

fn cargo_metadata(root: &Path) -> Result<Value, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| format!("failed to run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("failed to parse cargo metadata output: {error}"))
}

fn parse_packages(root: &Path, metadata: &Value) -> Result<Vec<(Crate, Vec<Dependency>)>, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata output has no packages array")?;
    packages
        .iter()
        .map(|package| {
            let name = package["name"]
                .as_str()
                .ok_or("package without a name in cargo metadata output")?
                .to_owned();
            let manifest = package["manifest_path"]
                .as_str()
                .ok_or_else(|| format!("package {name} has no manifest_path"))?;
            let relative = Path::new(manifest).strip_prefix(root).unwrap_or(Path::new(manifest));
            let location = locate(relative).ok_or_else(|| {
                format!(
                    "package {name} at {} is outside crates/engine/<group>/<crate>, crates/bindings, xtask or fuzz",
                    relative.display()
                )
            })?;
            let deps = package["dependencies"]
                .as_array()
                .map(|deps| deps.iter().filter_map(parse_dependency).collect())
                .unwrap_or_default();
            Ok((Crate { name, location }, deps))
        })
        .collect()
}

fn parse_dependency(dep: &Value) -> Option<Dependency> {
    let name = dep["name"].as_str()?.to_owned();
    let kind = match dep["kind"].as_str() {
        Some("dev") => DepKind::Dev,
        Some("build") => DepKind::Build,
        _ => DepKind::Normal,
    };
    Some(Dependency { name, kind })
}

#[cfg(test)]
#[path = "../tests/layers.rs"]
mod tests;
