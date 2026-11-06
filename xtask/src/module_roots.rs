//! `check-module-roots`: `lib.rs` and `mod.rs` may only declare modules,
//! re-export items and carry crate-level attributes and docs (spec section 15.1).

use std::fmt;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::{Item, ItemMacro, ItemMod};

use crate::workspace;

/// One item that is not allowed in a module root.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    pub(crate) line: usize,
    pub(crate) what: String,
}

/// Scans every `lib.rs` and `mod.rs` under `root` and reports forbidden items.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let files = workspace::rust_files(root).map_err(|error| {
        format!(
            "failed to list source files under {}: {error}",
            root.display()
        )
    })?;
    let mut failures = Vec::new();
    let mut checked = 0_usize;
    for path in files.iter().filter(|path| is_module_root(path)) {
        checked += 1;
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let violations = check_source(&source)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        failures.extend(violations.into_iter().map(|violation| Failure {
            path: workspace::relative(root, path),
            violation,
        }));
    }
    if failures.is_empty() {
        println!("check-module-roots: {checked} module roots ok");
        return Ok(());
    }
    let report: Vec<String> = failures.iter().map(ToString::to_string).collect();
    Err(format!(
        "{} forbidden item(s) in module roots:\n{}",
        failures.len(),
        report.join("\n")
    ))
}

/// Parses one module root and returns its forbidden items in source order.
pub(crate) fn check_source(source: &str) -> syn::Result<Vec<Violation>> {
    let file = syn::parse_file(source)?;
    Ok(file
        .items
        .iter()
        .filter_map(|item| {
            forbidden_kind(item).map(|what| Violation {
                line: item.span().start().line,
                what,
            })
        })
        .collect())
}

fn is_module_root(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("lib.rs" | "mod.rs")
    )
}

fn is_allowed(item: &Item) -> bool {
    match item {
        Item::Use(_) | Item::ExternCrate(_) | Item::Mod(ItemMod { content: None, .. }) => true,
        // Invocations such as `uniffi::setup_scaffolding!()` have to sit at the crate root.
        Item::Macro(ItemMacro { mac, .. }) => !mac.path.is_ident("macro_rules"),
        _ => false,
    }
}

fn forbidden_kind(item: &Item) -> Option<String> {
    if is_allowed(item) {
        return None;
    }
    let what = match item {
        Item::Mod(item) => format!("inline module `{}`", item.ident),
        Item::Macro(item) => {
            let name = item
                .ident
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            format!("macro definition `{name}`")
        }
        Item::Const(item) => format!("const `{}`", item.ident),
        Item::Enum(item) => format!("enum `{}`", item.ident),
        Item::Fn(item) => format!("fn `{}`", item.sig.ident),
        Item::ForeignMod(_) => "extern block".to_owned(),
        Item::Impl(_) => "impl block".to_owned(),
        Item::Static(item) => format!("static `{}`", item.ident),
        Item::Struct(item) => format!("struct `{}`", item.ident),
        Item::Trait(item) => format!("trait `{}`", item.ident),
        Item::TraitAlias(item) => format!("trait alias `{}`", item.ident),
        Item::Type(item) => format!("type alias `{}`", item.ident),
        Item::Union(item) => format!("union `{}`", item.ident),
        _ => "unrecognized item".to_owned(),
    };
    Some(what)
}

struct Failure {
    path: PathBuf,
    violation: Violation,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "  {}:{}: {}",
            self.path.display(),
            self.violation.line,
            self.violation.what
        )
    }
}

#[cfg(test)]
#[path = "../tests/module_roots.rs"]
mod tests;
