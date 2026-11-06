use super::{Violation, check_source};

#[test]
fn accepts_declarations_reexports_and_attributes() {
    let source = r"
            //! Crate docs.
            #![forbid(unsafe_code)]
            mod private;
            pub mod public;
            pub use public::Thing;
            extern crate alloc;
            uniffi::setup_scaffolding!();
        ";
    assert_eq!(check_source(source).unwrap(), Vec::new());
}

#[test]
fn rejects_definitions_with_their_line() {
    let source = "mod ok;\nfn helper() {}\npub struct Thing;\nimpl Thing {}\n";
    let found = check_source(source).unwrap();
    assert_eq!(
        found,
        vec![
            Violation {
                line: 2,
                what: "fn `helper`".to_owned()
            },
            Violation {
                line: 3,
                what: "struct `Thing`".to_owned()
            },
            Violation {
                line: 4,
                what: "impl block".to_owned()
            },
        ]
    );
}

#[test]
fn rejects_inline_modules_and_macro_definitions() {
    let source = "#[cfg(test)]\nmod tests { }\nmacro_rules! m { () => {} }\n";
    let found: Vec<String> = check_source(source)
        .unwrap()
        .into_iter()
        .map(|v| v.what)
        .collect();
    assert_eq!(found, ["inline module `tests`", "macro definition `m`"]);
}

#[test]
fn rejects_constants_statics_and_aliases() {
    let source = "const A: u8 = 1;\nstatic B: u8 = 2;\ntype C = u8;\n";
    let found: Vec<String> = check_source(source)
        .unwrap()
        .into_iter()
        .map(|v| v.what)
        .collect();
    assert_eq!(found, ["const `A`", "static `B`", "type alias `C`"]);
}
