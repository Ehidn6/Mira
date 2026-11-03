use mira_lang::{behavior, runtime, source_tree::SourceTree, syntax};

const SOURCE: &str = include_str!("../../../language/examples/structure");

#[test]
fn qualified_components_share_one_interpreter_and_definition_tree() {
    let p = syntax::parse(SOURCE).unwrap();
    let run = runtime::execute(&p, 1).unwrap();
    assert!(run.terminals.iter().any(|(name, value)| name == "Main/output" && *value == 15));
    assert!(run.terminals.iter().any(|(name, value)| name == "Main/nested" && *value == 7));
    assert_eq!(behavior::execute(&p, 6, &[5]).unwrap(), 15);
    assert_eq!(syntax::parse(&syntax::format(&p)).unwrap(), p);
    let tree = SourceTree::from_source(SOURCE).unwrap();
    assert_eq!(tree.list().unwrap().iter().find(|e| e.id == 4).unwrap().parent, Some(3));
}

#[test]
fn private_short_names_do_not_leak_and_lexical_shadowing_is_preserved() {
    assert!(syntax::parse(&SOURCE.replace("keyboard.transport(x) + keyboard.repeat(x)", "packet(x)")).is_err());
    let p = syntax::parse("outer(x:i64)->i64 =\n  shared(x:i64)->i64 = x + 2\n  branch(x:i64)->i64 =\n    shared(x:i64)->i64 = x + 7\n    shared(x)\n  branch(x) + shared(x)\nmain()->i64 = outer.branch(3) + outer.shared(3)\n").unwrap();
    let main = p.code_modules.iter().find(|d| d.name == "main").unwrap().id;
    assert_eq!(behavior::execute(&p, main, &[]).unwrap(), 15);
}

#[test]
fn qualified_handler_symbol_and_record_field_remain_distinct() {
    let p = syntax::parse("struct Pair:\n  value: i64\ncomponent()->unit =\n  work()->unit = ()\n  ()\nkernel()->i64 =\n  p = Pair(7)\n  sys.scratch(component.work,4096)\n  p.value\n").unwrap();
    assert_eq!(syntax::parse(&syntax::format(&p)).unwrap(), p);
    // Native compilation resolves the handler's lexical identity as well.
    assert!(!mira_lang::kernel::compile(&p).unwrap().is_empty());
}

#[test]
fn autodiff_resolves_nested_targets_and_their_qualified_helpers() {
    let p = syntax::parse("model(x:f32)->f32 =\n  square(x:f32)->f32 = x * x\n  loss(x:f32)->f32 = model.square(x - 3.0)\n  diff.derivative(loss,x)\nrun()->f32 = diff.derivative(model.loss,1.0)\nspace Main:\n  result = run()\n").unwrap();
    let run = runtime::execute(&p, 1).unwrap();
    assert_eq!(f32::from_bits(run.terminals[0].1 as u32), -4.0);
}

#[test]
fn nested_adaptive_definition_works_in_portable_and_native_execution() {
    let p = syntax::parse("component(x:i64)->i64 =\n  policy(x:i64,state_count:i64)->i64 = x + state_count\n  policy(x,0)\nprobe()->unit =\n  value = cells.policy(7)\nboot()->i64 =\n  accepted = sys.neural_try(probe,10000,4096)\n  if accepted\n    7\n  else\n    -1\nspace Main:\n  kernel = boot():\n    space Internal:\n      policy = adaptive(component.policy)\n").unwrap();
    assert_eq!(runtime::execute(&p,1).unwrap().terminals[0].1,7);
    assert_eq!(syntax::parse(&syntax::format(&p)).unwrap(),p);
    assert!(!mira_lang::kernel::compile(&p).unwrap().is_empty());
}
