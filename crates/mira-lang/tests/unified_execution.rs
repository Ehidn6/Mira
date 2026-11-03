//! Regression contracts prepared for the separate verification stage.
//! These tests have not been run during the architecture rework.
use mira_core::system::System;
use std::collections::BTreeMap;

fn system(source: &str) -> System {
    System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap()
}

#[test]
fn delay_and_persistent_tensor_share_one_tick() {
    let mut system = system(
        "type Value = tensor[f32,1]\nvalue()->Value = Value.persist()\nupdate(x:i64)->f32 =\n  v = cells.memory()\n  v[0] += math.to_f32(x)\n  v[0]\nspace Main:\n  previous @100 = delay(2)\n  one @101 = 1\n  result @102 = update():\n    space Body @103:\n      memory @104 = value()\n  one.out -> previous.in\n  previous.out -> result.x\n",
    );
    let first = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(
        f32::from_bits(first.deliveries[&(102, "out".into())] as u32),
        2.0
    );
    let second = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(
        f32::from_bits(second.deliveries[&(102, "out".into())] as u32),
        3.0
    );
    assert_eq!(system.tick, 2);
    assert_eq!(system.buffers[&104].words, vec![3.0f32.to_bits() as i64]);
}

#[test]
fn failed_tick_rolls_back_tensor_and_delay_state_together() {
    let mut system = system(
        "type Value = tensor[f32,1]\nvalue()->Value = Value.persist()\nupdate(x:i64)->f32 =\n  v = cells.memory()\n  v[0] += 1.0\n  broken = 1 / x\n  v[0]\nspace Main:\n  previous @100 = delay(0)\n  one @101 = 1\n  result @102 = update():\n    space Body @103:\n      memory @104 = value()\n  one.out -> previous.in\n  previous.out -> result.x\n",
    );
    let before = system.clone();
    assert!(mira_lang::system::step(&mut system, &BTreeMap::new()).is_err());
    assert_eq!(system, before);
}

#[test]
fn contextual_delay_calls_keep_state_between_ticks() {
    let mut system = system(
        "run()->i64 = cells.previous(9)\nspace Main:\n  result @100 = run():\n    space Body @101:\n      previous @102 = delay(3)\n",
    );
    let first = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    let second = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(first.deliveries[&(100, "out".into())], 3);
    assert_eq!(second.deliveries[&(100, "out".into())], 9);
}

#[test]
fn read_only_graph_call_cannot_publish_hidden_buffer_writes() {
    let mut system = system(
        "type Value = tensor[f32,1]\nvalue()->Value = Value.persist()\nmutate()->f32 =\n  w = cells.weights()\n  w[0] += 1.0\n  w[0]\npeek()->f32 = graph.out()\nrun()->f32 = sys.read_only(peek,4096)\nspace Main:\n  model @100 = run():\n    space Internal @101:\n      out out: f32\n      writer @102 = mutate():\n        space Memory @103:\n          weights @104 = value()\n      writer.out -> out\n",
    );
    let before = system.clone();
    assert!(mira_lang::system::step(&mut system, &BTreeMap::new()).is_err());
    assert_eq!(system, before);
}
