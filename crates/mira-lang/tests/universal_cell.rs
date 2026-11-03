use mira_core::{Definition, system::System};
use std::collections::BTreeMap;

#[test]
fn one_cell_keeps_identity_connections_and_body_when_given_an_algorithm() {
    let mut program = mira_lang::syntax::parse("double(x:i64)->i64 = x * 2\nspace Main:\n  value @90 = 7\n  result @100 = graph():\n    space Internal @101:\n      in x: i64\n      out out: i64\n      worker @102 = double()\n      x -> worker.x\n      worker.out -> out\n  value.out -> result.x\n").unwrap();
    let original = program.clone();
    let draft = mira_lang::behavior::definition_draft(&program, 100).unwrap();
    assert_eq!(program, original, "opening code must not mutate the cell");
    mira_lang::behavior::edit_definition(&mut program, 100, &draft).unwrap();
    assert!(matches!(
        program.root.find_cell(100).unwrap().definition,
        Definition::Code(_)
    ));
    assert_eq!(program.root.links, original.root.links);
    assert_eq!(
        program.root.find_cell(100).unwrap().space,
        original.root.find_cell(100).unwrap().space
    );
    let mut system = System::from_program(program).unwrap();
    let result = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(result.deliveries[&(100, "out".into())], 14);
    system.program.cell_mut(100).unwrap().definition = Definition::Graph;
    let result = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(result.deliveries[&(100, "out".into())], 14);
}
