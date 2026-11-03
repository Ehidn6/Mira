//! Exact forward-AD contracts; no finite differences or external ML runtime.
mod common;
use mira_core::system::System;
use std::collections::BTreeMap;

const DEFINITIONS: &str = "type Weight = tensor[f32,1]\nweights()->Weight = Weight.persist()\nloss(w:f32)->f32 =\n  error = w - 3.0\n  error * error\nupdate()->unit =\n  w = cells.weight()\n  gradient = diff.derivative(loss,w[0])\n  w[0] -= 0.1 * gradient\ntrain()->f32 =\n  accepted = sys.neural_try(update,10000,4096)\n  w = cells.weight()\n  loss(w[0])\n";
fn hosted_source() -> String {
    format!(
        "{DEFINITIONS}space Main:\n  model @100 = train():\n    space State @101:\n      weight @102 = weights()\n"
    )
}
#[test]
fn exact_gradient_updates_a_real_persistent_parameter() {
    let program = mira_lang::syntax::parse(&hosted_source()).unwrap();
    let mut system = System::from_program(program).unwrap();
    mira_lang::compilation_cache::clear();
    let first = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    let first_loss = f32::from_bits(first.deliveries[&(100, "out".into())] as u32);
    assert!((first_loss - 5.76).abs() < 0.0001);
    assert!((f32::from_bits(system.buffers[&102].words[0] as u32) - 0.6).abs() < 0.0001);
    let compiled = mira_lang::compilation_cache::stats();
    let second = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    let reused = mira_lang::compilation_cache::stats();
    assert_eq!(compiled.executable_misses, reused.executable_misses);
    assert_eq!(compiled.validation_misses, reused.validation_misses);
    assert!(reused.executable_hits > compiled.executable_hits);
    assert!(f32::from_bits(second.deliveries[&(100, "out".into())] as u32) < first_loss);
}
#[test]
fn pure_helper_chain_rule_and_explicit_effect_rejection() {
    let source = "square(x:f32)->f32 = x * x\nloss(x:f32)->f32 = square(x - 3.0)\nrun()->f32 = diff.derivative(loss,1.0)\nspace Main:\n  result @100 = run()\n";
    let mut system = System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap();
    let result = mira_lang::system::step(&mut system, &BTreeMap::new()).unwrap();
    assert_eq!(
        f32::from_bits(result.deliveries[&(100, "out".into())] as u32),
        -4.0
    );
    let unsupported = "loss(x:f32)->f32 =\n  sys.out8(233,33)\n  x * x\nrun()->f32 = diff.derivative(loss,1.0)\nspace Main:\n  result @100 = run()\n";
    assert!(mira_lang::syntax::parse(unsupported).is_err());
    let domain = "loss(x:f32)->f32 =\n  unused = 1.0 / x\n  7.0\nrun()->f32 = diff.derivative(loss,0.0)\nspace Main:\n  result @100 = run()\n";
    let program = mira_lang::syntax::parse(domain).unwrap();
    assert!(mira_lang::runtime::execute(&program, 1).is_err());
}
#[test]
#[ignore = "requires QEMU; real AD gradient updates a persistent native weight"]
fn native_parameter_learning_uses_generated_derivative() {
    let source = format!(
        "{DEFINITIONS}boot()->unit =\n  result = cells.model()\n  if result > 5.7 and result < 5.8\n    sys.out8(233,77)\n  sys.out8(233,33)\nspace Main:\n  kernel @200 = boot():\n    space Body @201:\n      model @100 = train():\n        space State @101:\n          weight @102 = weights()\n"
    );
    let image = common::native_image(&source, "autodiff-gradient");
    assert_eq!(common::qemu(&image, false, "autodiff-gradient"), "M!");
}
