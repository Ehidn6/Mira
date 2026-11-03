//! Source staging edits Core graph objects; the formatted result is the expansion.
use mira_core::{Definition, system::System};
use std::collections::BTreeMap;

#[test]
fn source_macro_clones_and_connects_using_live_reflection() {
    let source = "macro build(n:i64)->unit =\n  root = reflect.root()\n  template = reflect.cell_at(root,0)\n  for i in 0..n\n    copy = meta.clone_cell(template,root)\n    meta.set_parameter(copy,i + 2)\n    count = reflect.cell_count(root)\n    if count > 1\n      previous = reflect.cell_at(root,count - 2)\n      link = meta.connect(root,previous,0,copy,0)\nspace Main:\n  template @100 = add(1)\nexpand build(3)\n";
    let program = mira_lang::syntax::parse(source).unwrap();
    assert_eq!(program.root.cells.len(), 4);
    assert_eq!(program.root.links.len(), 3);
    assert_eq!(
        program
            .root
            .cells
            .iter()
            .map(|c| c.parameter)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let canonical = mira_lang::syntax::format(&program);
    assert!(!canonical.contains("expand build("));
    let roundtrip = mira_lang::syntax::parse(&canonical).unwrap();
    assert_eq!(roundtrip, program);
}
#[test]
fn cloned_z_graphs_have_fresh_identities_and_remapped_interfaces() {
    let source = "macro duplicate()->unit =\n  root = reflect.root()\n  template = reflect.cell_at(root,0)\n  copy = meta.clone_cell(template,root)\n  ()\nspace Main:\n  component @100 = graph():\n    space Body @101:\n      out out: i64\n      value @102 = 7\n      value.out -> out\nexpand duplicate()\n";
    let program = mira_lang::syntax::parse(source).unwrap();
    assert_eq!(program.root.cells.len(), 2);
    let a = &program.root.cells[0];
    let b = &program.root.cells[1];
    assert_eq!(a.definition, Definition::Graph);
    assert_ne!(a.id, b.id);
    let x = a.space.as_ref().unwrap();
    let y = b.space.as_ref().unwrap();
    assert_ne!(x.id, y.id);
    assert_ne!(x.cells[0].id, y.cells[0].id);
    assert_eq!(y.links[0].from.cell, y.cells[0].id);
    assert_eq!(y.links[0].to.cell, y.id);
    program.validate().unwrap();
    let copy_id = b.id;
    let mut system = System::from_program(program).unwrap();
    let output = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(output.deliveries[&(100, "out".into())], 7);
    assert_eq!(output.deliveries[&(copy_id, "out".into())], 7);
}
#[test]
fn failed_expansion_does_not_publish_earlier_mutations() {
    let source = "macro broken()->unit =\n  root = reflect.root()\n  first = reflect.cell_at(root,0)\n  meta.set_parameter(first,99)\n  link = meta.connect(root,first,100,first,0)\nspace Main:\n  value @100 = 7\n";
    let mut program = mira_lang::syntax::parse(source).unwrap();
    let before = program.clone();
    let result = mira_lang::metaprogram::expand(
        &mut program,
        &[mira_lang::metaprogram::Invocation {
            name: "broken".into(),
            args: vec![],
        }],
    );
    assert!(result.is_err());
    assert_eq!(program, before);
}
#[test]
fn macros_cannot_use_hardware_or_runtime_context_effects() {
    let source = "macro forbidden()->unit = sys.out8(233,33)\nspace Main:\nexpand forbidden()\n";
    assert!(mira_lang::syntax::parse(source).is_err());
    let source = "macro infinite()->unit =\n  while true\n    ()\nspace Main:\nexpand infinite()\n";
    assert!(mira_lang::syntax::parse(source).is_err());
}

#[test]
fn identical_port_types_remain_distinct_and_stage_excludes_runtime_bodies() {
    let source = "struct Pair:\n  total: i64\n  difference: i64\ncombine(a:i64,b:i64)-> ports Pair = Pair(a+b,a-b)\nidentity(x:i64)->i64 = x\nread_cpu()->i64 = sys.cpuid_eax(0,0)\nmacro unchanged()->unit = ()\nspace Main:\n  first @90 = 7\n  second @91 = 3\n  operation @100 = combine()\n  total @101 = identity()\n  difference @102 = identity()\n  duplicate @103 = identity()\n  first.out -> operation.a\n  second.out -> operation.b\n  operation.total -> total.x\n  operation.difference -> difference.x\n  operation.total -> duplicate.x\nexpand unchanged()\n";
    let program = mira_lang::syntax::parse(source).unwrap();
    let ambiguous = source.replace("first.out -> operation.a", "first.out -> operation");
    assert!(mira_lang::syntax::parse(&ambiguous).is_err());
    let mut system = System::from_program(program).unwrap();
    let output = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(output.deliveries[&(101, "out".into())], 10);
    assert_eq!(output.deliveries[&(102, "out".into())], 4);
    assert_eq!(output.deliveries[&(103, "out".into())], 10);
}
