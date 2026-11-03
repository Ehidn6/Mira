use mira_core::Definition;
use mira_lang::syntax;

#[test]
fn macro_is_an_explicit_definition_stage_without_an_automatic_runtime_cell() {
    let program = syntax::parse("macro build() -> i64 = 1\nspace main:\n").unwrap();
    assert!(program.code_modules[0].compile_time);
    assert!(program.root.cells.is_empty());
    let formatted = syntax::format(&program);
    assert!(formatted.contains("macro build"));
    assert_eq!(syntax::parse(&formatted).unwrap(), program);
}

#[test]
fn core_refuses_binding_a_macro_as_a_runtime_cell() {
    let mut program = syntax::parse("run() -> i64 = 1\nspace main:\n  result = run()\n").unwrap();
    program.code_modules[0].compile_time = true;
    assert!(matches!(program.root.cells[0].definition, Definition::Code(_)));
    let error = program.validate().unwrap_err();
    assert!(error.contains("cannot be a runtime cell"), "{error}");
}
