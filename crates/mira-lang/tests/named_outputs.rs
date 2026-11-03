mod common;
use mira_core::{Direction, system::System};
use std::collections::BTreeMap;
const SOURCE: &str = "type Count = [i64,1]\nstruct Result:\n  total: i64\n  positive: bool\ncount()->Count = Count.persist()\nsplit(x:i64)-> ports Result =\n  calls = cells.calls()\n  calls[0] += 1\n  Result(x * 2,x > 0)\nconsume(total:i64,positive:bool)->i64 = positive ? total : 0\nspace Main:\n  value @90 = 3\n  producer @100 = split():\n    space State @101:\n      calls @102 = count()\n  consumer @103 = consume()\n  value.out -> producer.x\n  producer.total -> consumer.total\n  producer.positive -> consumer.positive\n";

#[test]
fn record_results_deliver_named_ports_once_and_roundtrip() {
    let program = mira_lang::syntax::parse(SOURCE).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&program)).unwrap(),
        program
    );
    let ports = program.ports(program.root.find_cell(100).unwrap());
    assert_eq!(
        ports
            .iter()
            .filter(|p| p.direction == Direction::Out)
            .map(|p| p.name)
            .collect::<Vec<_>>(),
        vec!["total", "positive"]
    );
    let mut system = System::from_program(program).unwrap();
    let result = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(result.deliveries[&(100, "total".into())], 6);
    assert_eq!(result.deliveries[&(100, "positive".into())], 1);
    assert_eq!(result.deliveries[&(103, "out".into())], 6);
    assert_eq!(system.buffers[&102].words, vec![1]);
}

#[test]
#[ignore = "requires QEMU; native graph wrapper projects a record returned once"]
fn native_graph_projects_named_outputs() {
    let definitions = SOURCE.split("space Main:").next().unwrap();
    let source = format!(
        "{definitions}boot()->unit =\n  result = graph.out()\n  if result == 6\n    sys.out8(233,77)\n  sys.out8(233,33)\nspace Main:\n  kernel @200 = boot():\n    space Body @201:\n      out out: i64\n      value @90 = 3\n      producer @100 = split():\n        space State @101:\n          calls @102 = count()\n      consumer @103 = consume()\n      value.out -> producer.x\n      producer.total -> consumer.total\n      producer.positive -> consumer.positive\n      consumer.out -> out\n"
    );
    let image = common::native_image(&source, "named-outputs");
    assert_eq!(common::qemu(&image, false, "named-outputs"), "M!");
}
