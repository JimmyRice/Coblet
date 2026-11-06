//! `check-test-layout`: unit tests live in each crate's `tests` directory,
//! mirroring the `src` path, and are pulled in with
//! `#[cfg(test)] #[path = "..."] mod tests;` so they keep access to private
//! items. Cargo would also build those files as integration tests, so the
//! crate sets `autotests = false`, and a file nothing pulls in would never
//! compile; this check catches both.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, Item, ItemMod, Lit, Meta};

use crate::workspace;

/// One `#[cfg(test)]` module declaration found in a source file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TestModule {
    pub(crate) line: usize,
    pub(crate) name: String,
    /// Value of its `#[path]` attribute.
    pub(crate) path: Option<String>,
    /// Whether the body is written inline instead of in a file.
    pub(crate) inline: bool,
    /// Whether it sits inside an inline module, where `#[path]` resolves differently.
    pub(crate) nested: bool,
}

/// Checks every crate under `root`.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let manifests = workspace::manifests(root)
        .map_err(|error| format!("failed to list manifests under {}: {error}", root.display()))?;
    let mut failures = Vec::new();
    let mut referenced_total = 0_usize;
    for manifest in &manifests {
        let toml = read(manifest)?;
        if !toml.contains("[package]") {
            continue;
        }
        let krate = manifest.parent().unwrap_or(root);
        let (referenced, mut problems) = check_crate(krate)?;
        referenced_total += referenced.len();
        problems.extend(orphans(krate, &referenced, &toml)?);
        if !referenced.is_empty() && !has_autotests_off(&toml) {
            problems.push(Failure::new(
                manifest,
                1,
                "has unit tests under tests/ but does not set `autotests = false`".to_owned(),
            ));
        }
        failures.extend(
            problems
                .into_iter()
                .map(|failure| failure.relative_to(root)),
        );
    }
    if failures.is_empty() {
        println!("check-test-layout: {referenced_total} test files ok");
        return Ok(());
    }
    let report: Vec<String> = failures.iter().map(ToString::to_string).collect();
    Err(format!(
        "{} test layout problem(s):\n{}",
        failures.len(),
        report.join("\n")
    ))
}

/// Parses one source file and returns its `#[cfg(test)]` module declarations.
pub(crate) fn test_modules(source: &str) -> syn::Result<Vec<TestModule>> {
    let file = syn::parse_file(source)?;
    let mut found = Vec::new();
    collect(&file.items, false, &mut found);
    Ok(found)
}

fn collect(items: &[Item], nested: bool, found: &mut Vec<TestModule>) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if is_cfg_test(&module.attrs) {
            found.push(TestModule {
                line: module.span().start().line,
                name: module.ident.to_string(),
                path: path_attr(&module.attrs),
                inline: module.content.is_some(),
                nested,
            });
        } else if let ItemMod {
            content: Some((_, inner)),
            ..
        } = module
        {
            collect(inner, true, found);
        }
    }
}

fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Ident>()
                .is_ok_and(|ident| ident == "test")
    })
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        Meta::NameValue(pair) if pair.path.is_ident("path") => match &pair.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) => Some(text.value()),
            _ => None,
        },
        _ => None,
    })
}

/// Checks the declarations in `<crate>/src` and returns the test files they reference.
fn check_crate(krate: &Path) -> Result<(BTreeSet<PathBuf>, Vec<Failure>), String> {
    let src = krate.join("src");
    let tests = krate.join("tests");
    let mut referenced = BTreeSet::new();
    let mut failures = Vec::new();
    if !src.is_dir() {
        return Ok((referenced, failures));
    }
    let files = workspace::rust_files(&src)
        .map_err(|error| format!("failed to list {}: {error}", src.display()))?;
    for file in &files {
        let modules = test_modules(&read(file)?)
            .map_err(|error| format!("failed to parse {}: {error}", file.display()))?;
        for module in modules {
            let problem = if module.inline {
                Some(format!(
                    "inline test module `{}`; move its body under tests/",
                    module.name
                ))
            } else if module.nested {
                Some(format!(
                    "test module `{}` inside an inline module; declare it at the top of the file",
                    module.name
                ))
            } else if let Some(path) = &module.path {
                let target = normalize(&file.parent().unwrap_or(krate).join(path));
                if !target.starts_with(&tests) {
                    Some(format!(
                        "test module `{}` points outside tests/: {path}",
                        module.name
                    ))
                } else if !target.is_file() {
                    Some(format!(
                        "test module `{}` points to a missing file: {path}",
                        module.name
                    ))
                } else {
                    referenced.insert(target);
                    None
                }
            } else {
                Some(format!(
                    "test module `{}` has no #[path] into tests/",
                    module.name
                ))
            };
            if let Some(what) = problem {
                failures.push(Failure::new(file, module.line, what));
            }
        }
    }
    Ok((referenced, failures))
}

/// Reports `.rs` files under `<crate>/tests` that no declaration pulls in
/// and the manifest does not name, since they would never compile.
fn orphans(
    krate: &Path,
    referenced: &BTreeSet<PathBuf>,
    toml: &str,
) -> Result<Vec<Failure>, String> {
    let tests = krate.join("tests");
    if !tests.is_dir() {
        return Ok(Vec::new());
    }
    let files = workspace::rust_files(&tests)
        .map_err(|error| format!("failed to list {}: {error}", tests.display()))?;
    Ok(files
        .into_iter()
        .filter(|file| !referenced.contains(file))
        .filter(|file| {
            let relative = workspace::relative(krate, file);
            !toml.contains(&format!("\"{}\"", relative.display()))
        })
        .map(|file| {
            Failure::new(
                &file,
                1,
                "no #[cfg(test)] module pulls this file in, so it never compiles".to_owned(),
            )
        })
        .collect())
}

/// Reports whether a manifest switches off integration test discovery.
pub(crate) fn has_autotests_off(toml: &str) -> bool {
    toml.lines().any(|line| {
        let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        line == "autotests=false"
    })
}

/// Resolves `.` and `..` without touching the file system.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

struct Failure {
    path: PathBuf,
    line: usize,
    what: String,
}

impl Failure {
    fn new(path: &Path, line: usize, what: String) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            what,
        }
    }

    fn relative_to(self, root: &Path) -> Self {
        Self {
            path: workspace::relative(root, &self.path),
            ..self
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  {}:{}: {}", self.path.display(), self.line, self.what)
    }
}

#[cfg(test)]
#[path = "../tests/test_layout.rs"]
mod tests;
