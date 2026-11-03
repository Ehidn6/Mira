mod common;
use mira_core::system::System;
use std::collections::BTreeMap;

const SOURCE: &str = "type Weight = tensor[f32,1]\nmemory()->Weight = Weight.persist()\ntouch()->i64 =\n  w = cells.memory()\n  w[0] += 1.0\n  reflect.root()\ninspect()->i64 =\n  root = reflect.root()\n  owner = reflect.self()\n  inner = reflect.inner(owner)\n  link = reflect.link_at(root,0)\n  ok = reflect.cell_count(root) == 2 and reflect.cell_at(root,0) == 10\n  ok = ok and reflect.cell_at(root,1) == owner and reflect.cell_count(inner) == 1\n  ok = ok and reflect.cell_at(inner,0) == 22 and reflect.inner(22) == 0\n  ok = ok and reflect.link_count(root) == 1 and reflect.link_source(link) == 10\n  ok = ok and reflect.link_target(link) == owner and reflect.link_source_port(link) == 0\n  ok = ok and reflect.link_target_port(link) == 0 and reflect.input_count(owner) == 1\n  ok = ok and reflect.output_count(owner) == 1 and reflect.port_type(owner,0,0) == 1\n  ok = ok and reflect.port_type(22,1,0) == 7 and reflect.port_data_type(22,1,0) > 0\n  ok = ok and reflect.port_data_type(owner,0,0) == 0 and reflect.behavior(owner) > 0\n  ok = ok and reflect.behavior(10) == 0 and reflect.name_byte(owner,0) == 114\n  ok = ok and reflect.port_name_byte(owner,0,0,0) == 118\n  ok = ok and reflect.cell_at(root,-1) == -1 and reflect.cell_at(root,99) == -1\n  ok = ok and reflect.port_type(owner,7,0) == -1 and reflect.name_byte(owner,999) == -1\n  ok ? 1 : 0\nrun(value:i64)->i64 =\n  good = sys.read_only(inspect,4096)\n  count = reflect.cell_count(touch())\n  valid = good == 1 and count == 2 and cells.memory()[0] == 1.0\n  valid ? value : 0\nspace Main @1:\n  source @10 = 7\n  result @20 = run():\n    space State @21:\n      memory @22 = memory()\n  source.out -> result.value\n";

#[test]
fn source_program_inspects_its_structure_with_single_evaluation_and_read_only_scope() {
    let program = mira_lang::syntax::parse(SOURCE).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&program)).unwrap(),
        program
    );
    let mut system = System::from_program(program).unwrap();
    let result = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(result.deliveries[&(20, "out".into())], 7);
    assert_eq!(system.buffers[&22].words, vec![1.0f32.to_bits() as i64]);
    assert!(
        mira_lang::syntax::parse(&SOURCE.replace("reflect.root()", "reflect.root(1)")).is_err()
    );
    assert!(
        mira_lang::syntax::parse(&SOURCE.replace("reflect.inner(owner)", "reflect.unknown(owner)"))
            .is_err()
    );
}

#[test]
#[ignore = "requires QEMU; real source-level reflection in an autonomous payload"]
fn native_graph_reflection_uses_the_same_structure_and_ports() {
    let source = "type Weight = tensor[f32,1]\nmemory()->Weight = Weight.persist()\nidentity(x:i64)->i64 = x\ntouch()->i64 =\n  cells.memory()[0] += 1.0\n  reflect.inner(reflect.self())\ninspect()->i64 =\n  owner = reflect.self()\n  inner = reflect.inner(owner)\n  link = reflect.link_at(inner,0)\n  ok = reflect.cell_count(reflect.root()) == 1\n  ok = ok and reflect.cell_at(reflect.root(),0) == owner\n  ok = ok and reflect.cell_count(inner) == 3 and reflect.link_count(inner) == 2\n  ok = ok and reflect.link_source(link) == 23 and reflect.link_target(link) == 24\n  ok = ok and reflect.link_source_port(link) == 0 and reflect.link_target_port(link) == 0\n  ok = ok and reflect.port_type(22,1,0) == 7 and reflect.port_data_type(22,1,0) > 0\n  ok = ok and reflect.name_byte(owner,0) == 107 and reflect.port_name_byte(24,0,0,0) == 120\n  ok ? 1 : 0\nboot()->unit =\n  good = sys.read_only(inspect,4096)\n  count = reflect.cell_count(touch())\n  if good == 1 and count == 3 and cells.memory()[0] == 1.0 and graph.answer() == 7\n    sys.out8(233,82)\n  sys.out8(233,33)\n  sys.halt()\nspace Main @1:\n  kernel @20 = boot():\n    space State @21:\n      out answer: i64\n      memory @22 = memory()\n      source @23 = 7\n      receiver @24 = identity()\n      source.out -> receiver.x\n      receiver.out -> answer\n";
    let image = common::native_image(&source, "reflection");
    assert_eq!(common::qemu(&image, false, "reflection"), "R!");
}

#[test]
fn containment_queries_keep_spaces_cells_and_wires_distinct() {
    let source = r#"nested(x:i64)->i64 = x
inspect_parent()->i64 =
  link = reflect.link_at(1,0)
  ok = reflect.parent(1) == 0 and reflect.parent(10) == 1
  ok = ok and reflect.parent(20) == 1 and reflect.parent(21) == 20
  ok = ok and reflect.parent(22) == 21 and reflect.parent(link) == 1
  ok = ok and reflect.parent(-1) == -1 and reflect.parent(999999) == -1
  ok ? 1 : 0
space Main @1:
  source @10 = 7
  box @20 = nested():
    space Inner @21:
      value @22 = 9
  result @30 = inspect_parent()
  source.out -> box.x
"#;
    let program = mira_lang::syntax::parse(source).unwrap();
    let result = mira_lang::runtime::execute(&program, 1).unwrap();
    assert!(
        result
            .terminals
            .iter()
            .any(|(name, value)| name == "Main/result" && *value == 1),
        "{:?}",
        result.terminals
    );
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&program)).unwrap(),
        program
    );
}

#[test]
fn behavior_names_resolve_in_the_existing_program_identity_space() {
    let source = r#"algorithm(x:i64)->i64 = x
inspect_names()->bool =
  definition = reflect.behavior(20)
  reflect.name_byte(definition,0) == 97 and reflect.name_byte(definition,8) == 109 and reflect.name_byte(definition,9) == -1 and reflect.name_byte(999999,0) == -1
space Main @1:
  source @10 = 7
  result @20 = algorithm()
  proof @30 = inspect_names()
  source.out -> result.x
"#;
    let program = mira_lang::syntax::parse(source).unwrap();
    let result = mira_lang::runtime::execute(&program, 1).unwrap();
    assert!(
        result
            .terminals
            .iter()
            .any(|(name, value)| name == "Main/proof" && *value == 1)
    );
}
