use mira_lang::{kernel, runtime, syntax};

#[test]
fn state_example_keeps_the_same_instances_state_across_activations() {
    let p = syntax::parse(include_str!("../../../language/examples/state")).unwrap();
    assert!(!kernel::compile(&p).unwrap().is_empty());
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let first = runtime::execute_system(&mut system, 1).unwrap();
    let second = runtime::execute_system(&mut system, 2).unwrap();
    assert_eq!(first.terminals[0].1, 1.0f32.to_bits() as i64);
    assert_eq!(second.terminals[0].1, 2.0f32.to_bits() as i64);
}

#[test]
fn system_example_produces_a_freestanding_image() {
    let p = syntax::parse(include_str!("../../../language/examples/system")).unwrap();
    assert!(!kernel::compile(&p).unwrap().is_empty());
}

#[test]
fn beginner_graph_example_runs_and_explains_misplaced_arguments() {
    let source = include_str!("../../../language/examples/first-graph");
    let program = syntax::parse(source).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 6);
    let error = syntax::parse("double(value) = value * 2\nspace Main:\n  result = double(3)\n").unwrap_err();
    assert!(error.contains("cell inputs are connected with ->"), "{error}");
}
