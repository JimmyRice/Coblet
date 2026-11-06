//! `check-test-layout`: unit tests sit inline at the end of the file they
//! test, in `#[cfg(test)] mod tests { ... }`, the layout the Rust book
//! describes. Test helpers such as mocks may be ordinary module files, but
//! no test module points elsewhere with `#[path]`.

use std::fmt;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::{Attribute, Item, ItemMod, Meta};

use crate::workspace;

/// One test module that breaks the layout.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    pub(crate) line: usize,
    pub(crate) what: String,
}

/// Scans every source file under `root`.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let files = workspace::rust_files(root).map_err(|error| {
        format!(
            "failed to list source files under {}: {error}",
            root.display()
        )
    })?;
    let mut failures = Vec::new();
    let mut modules = 0_usize;
    for path in files.iter().filter(|path| in_src(path)) {
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let (count, violations) = check_source(&source)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        modules += count;
        failures.extend(violations.into_iter().map(|violation| Failure {
            path: workspace::relative(root, path),
            violation,
        }));
    }
    if failures.is_empty() {
        println!("check-test-layout: {modules} test modules ok");
        return Ok(());
    }
    let report: Vec<String> = failures.iter().map(ToString::to_string).collect();
    Err(format!(
        "{} test module(s) outside the inline layout:\n{}",
        failures.len(),
        report.join("\n")
    ))
}

/// Parses one source file and returns how many test modules it declares
/// and the ones that break the layout.
pub(crate) fn check_source(source: &str) -> syn::Result<(usize, Vec<Violation>)> {
    let file = syn::parse_file(source)?;
    let mut count = 0;
    let mut violations = Vec::new();
    collect(&file.items, &mut count, &mut violations);
    Ok((count, violations))
}

fn collect(items: &[Item], count: &mut usize, violations: &mut Vec<Violation>) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if is_cfg_test(&module.attrs) {
            *count += 1;
            if let Some(what) = problem(module) {
                violations.push(Violation {
                    line: module.span().start().line,
                    what,
                });
            }
        } else if let Some((_, inner)) = &module.content {
            collect(inner, count, violations);
        }
    }
}

fn problem(module: &ItemMod) -> Option<String> {
    let name = &module.ident;
    if module.attrs.iter().any(|attr| attr.path().is_ident("path")) {
        Some(format!(
            "test module `{name}` uses #[path]; keep tests next to the code they test"
        ))
    } else if name == "tests" && module.content.is_none() {
        Some(
            "`mod tests;` lives in another file; write it inline at the end of this file"
                .to_owned(),
        )
    } else {
        None
    }
}

/// Reports whether a `#[cfg]` attribute requires `test`, such as `cfg(test)`
/// or `cfg(all(test, unix))`; `test` under `not(..)` does not count.
fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| match &attr.meta {
        Meta::List(list) if list.path.is_ident("cfg") => mentions_test(list.tokens.clone()),
        _ => false,
    })
}

fn mentions_test(tokens: TokenStream) -> bool {
    let mut negated = false;
    for token in tokens {
        match token {
            TokenTree::Ident(ident) if ident == "test" => return true,
            TokenTree::Ident(ident) => negated = ident == "not",
            TokenTree::Group(group) => {
                if !negated && mentions_test(group.stream()) {
                    return true;
                }
                negated = false;
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => negated = false,
        }
    }
    false
}

/// Reports whether a file belongs to a crate's `src`, where unit tests live;
/// integration tests, examples and build scripts are left alone.
fn in_src(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "src")
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
mod tests {
    use super::check_source;

    #[test]
    fn accepts_inline_tests_and_helper_files() {
        let source = r"
mod plain;

#[cfg(test)]
mod mock;

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn works() {}
}

#[cfg(not(test))]
mod release;
";
        assert_eq!(check_source(source).unwrap(), (2, Vec::new()));
    }

    #[test]
    fn rejects_path_attributes_and_out_of_line_tests() {
        let source = "#[cfg(test)]\n#[path = \"../tests/a.rs\"]\nmod tests;\n\n\
                      mod outer {\n    #[cfg(test)]\n    mod tests;\n}\n";
        let (count, violations) = check_source(source).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            violations.iter().map(|v| v.line).collect::<Vec<_>>(),
            [1, 6]
        );
        assert!(violations[0].what.contains("#[path]"), "{violations:?}");
        assert!(violations[1].what.contains("inline"), "{violations:?}");
    }
}
