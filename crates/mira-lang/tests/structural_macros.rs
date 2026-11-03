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

#[test]
fn window_templates_are_real_editable_graphs_with_independent_copies() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../projects/MiraOS").canonicalize().unwrap();
    fn fragment(root: &std::path::Path, id: u64, indent: usize) -> String {
        let text = mira_lang::source_tree::read_selected(root,id).unwrap();
        let old = text.lines().find(|l| !l.trim().is_empty()).unwrap()
            .bytes().take_while(|b| *b == b' ').count();
        text.lines().map(|l| format!("{}{}\n"," ".repeat(indent),&l[old.min(l.len())..])).collect()
    }
    let mut source = String::from("identities 1000\n");
    for id in [2,29,440,445] { source.push_str(&fragment(&root,id,0)); }
    source.push_str("display()->unit =\n  ()\n");
    for id in [439,443,444,448,449,450] {source.push_str(&fragment(&root,id,2));}
    source.push_str("boot()->unit =\n  drawn = cells.window_template(Console(),WidgetView(0,0,12,3,0xffffff,0),Text(),Text())\nspace Main:\n  kernel = boot():\n    space UI:\n");
    source.push_str(&fragment(&root,454,6));
    source.push_str(include_str!("../../../language/examples/window-templates"));
    source.push_str("expand build_windows()\n");
    let program = mira_lang::syntax::parse(&source).unwrap();
    let ui = program.root.cells[0].space.as_ref().unwrap();
    assert_eq!(ui.cells.len(),3);
    let original = ui.cells[0].space.as_ref().unwrap();
    let first = ui.cells[1].space.as_ref().unwrap();
    let second = ui.cells[2].space.as_ref().unwrap();
    let parameter = |space: &mira_core::Space,name: &str| space.cells.iter().find(|c| c.name==name).unwrap().parameter;
    assert_eq!(parameter(original,"header"),0);
    assert_eq!(parameter(first,"header"),1);
    assert_eq!(parameter(second,"header"),2);
    assert_eq!(parameter(first,"ready"),0);
    assert_eq!(parameter(second,"ready"),1);
    assert_eq!(first.cells.len(),original.cells.len()+1);
    assert_eq!(second.cells.len(),original.cells.len());
    for window in &ui.cells {
        let inner = window.space.as_ref().unwrap();
        for link in &inner.links {
            assert!(link.from.cell==inner.id || inner.cells.iter().any(|c| c.id==link.from.cell));
            assert!(link.to.cell==inner.id || inner.cells.iter().any(|c| c.id==link.to.cell));
        }
    }
    assert_eq!(mira_lang::syntax::parse(&mira_lang::syntax::format(&program)).unwrap(),program);
    // The window/renderer graph is accepted by the real freestanding backend.
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    // A surface with video=false delivers a real false result, not an absent
    // signal. The same graph also executes without invoking hardware drawing.
    mira_lang::runtime::execute(&program,1).unwrap();
    // A failed template edit cannot publish the first, already performed clone.
    let bad = source.replace("display.templates.header_rows(second,2)","display.templates.header_rows(0,2)");
    let mut draft = mira_lang::syntax::parse(&bad.replace("expand build_windows()\n","")).unwrap();
    let before = draft.clone();
    assert!(mira_lang::metaprogram::expand(&mut draft,&[mira_lang::metaprogram::Invocation {
        name: "build_windows".into(), args: vec![],
    }]).is_err());
    assert_eq!(draft,before);
}
