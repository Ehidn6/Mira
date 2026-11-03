//! A single nested compiler source object supplies both MiraOS and stage zero.
use mira_lang::{behavior, syntax};
#[test]
fn compiler_source_is_the_same_program_on_host_and_native_targets() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/MiraOS");
    let source = mira_lang::source_tree::read_selected(&path.canonicalize().unwrap(), 258).unwrap();
    let nested = syntax::parse(&source).unwrap();
    let view = syntax::parse(mira_lang::compiler_source().unwrap()).unwrap();
    assert_eq!(nested.code_modules[0].name, "compiler");
    assert_eq!(nested.code_modules[0].id, 258);
    fn compare(d: &mira_core::code::CodeModule, view: &mira_core::Program) {
        let exposed = mira_core::code::find(&view.code_modules, d.id).unwrap();
        let mut expected = d.clone();
        expected.locals.clear();
        expected.data_types.clear();
        assert_eq!(*exposed, expected, "bootstrap changed {}", d.name);
        for child in &d.locals {
            compare(child, view);
        }
    }
    compare(&nested.code_modules[0], &view);
    let entry = view
        .code_modules
        .iter()
        .find(|d| d.name == "operator_descriptor")
        .unwrap()
        .id;
    assert_eq!(behavior::execute(&view, entry, &[43, 1]).unwrap(), 2061);
    assert_eq!(nested.data_types(), view.data_types());
    assert_eq!(syntax::parse(&syntax::format(&nested)).unwrap(), nested);
    // The obsolete special inclusion is removed, not adapted to the new source.
    assert!(syntax::parse("use compiler.lexer\n").is_err());
    let named = syntax::parse("use()->i64 = 7\nspace Main:\n  result = use()\n").unwrap();
    assert_eq!(
        mira_lang::runtime::execute(&named, 1).unwrap().terminals[0].1,
        7
    );
}

#[test]
fn compiler_builder_preserves_a_foreign_destination() {
    let directory = std::env::temp_dir().join(format!("mira-foreign-compiler-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let index = b"{\"format\":\"mira-source-layout-v3\"}";
    std::fs::write(directory.join("index"),index).unwrap();
    assert!(mira_lang::build_compiler(&directory).is_err());
    assert_eq!(std::fs::read(directory.join("index")).unwrap(),index);
    assert!(!directory.join("code").exists());
    std::fs::remove_dir_all(directory).unwrap();
}
