mod common;
use common::*;
const SCOPED: &str = "type Chunk = [i64,1024]\nsmall()->i64 =\n  tmp = Chunk()\n  tmp[1000] = 7\n  tmp[1000]\nouter()->i64 =\n  held = Chunk()\n  held[0] = 11\n  inner = sys.scratch(small,8192)\n  held[0] + inner\nrun()->i64 =\n  held = Chunk()\n  held[0] = 23\n  sum = 0\n  for i in 0..200\n    sum += sys.scratch(outer,16384)\n  sum + held[0]\nspace main:\n  result = run()\n";
#[test]
fn scratch_scopes_reuse_memory_preserve_outer_objects_and_return_scalars() {
    let p = mira_lang::syntax::parse(SCOPED).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&p)).unwrap(),
        p
    );
    assert_eq!(
        mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1,
        3623
    );
    let no_scope = SCOPED.replace("sum += sys.scratch(outer,16384)", "sum += outer()");
    assert!(
        mira_lang::runtime::execute(&mira_lang::syntax::parse(&no_scope).unwrap(), 1)
            .unwrap_err()
            .contains("arena")
    );
    let small = SCOPED.replace("sys.scratch(small,8192)", "sys.scratch(small,0)");
    assert!(
        mira_lang::runtime::execute(&mira_lang::syntax::parse(&small).unwrap(), 1)
            .unwrap_err()
            .contains("arena")
    );
    let reference = SCOPED
        .replace("small()->i64", "small()->Chunk")
        .replace("  tmp[1000]\nouter", "  tmp\nouter");
    assert!(
        mira_lang::syntax::parse(&reference)
            .unwrap_err()
            .contains("temporary reference")
    );
    let view = SCOPED.replace("tmp = Chunk()", "tmp = Chunk.at(sys.addr(8388608))");
    let p = mira_lang::syntax::parse(&view).unwrap();
    assert!(
        mira_lang::runtime::execute(&p, 1)
            .unwrap_err()
            .contains("raw aggregate views")
    );
    let boot = "boot()->unit = run()
space main:
  kernel = boot()
";
    // Use a unit entry that discards the scalar result explicitly.
    let boot = boot.replace("boot()->unit = run()", "boot()->unit =\n  value = run()");
    let native = view.replace("space main:\n  result = run()\n", &boot);
    let p = mira_lang::syntax::parse(&native).unwrap();
    assert!(
        mira_lang::kernel::compile(&p)
            .unwrap_err()
            .contains("raw aggregate views")
    );
}
#[test]
fn deterministic_scopes_can_host_the_existing_neural_transaction() {
    let source = include_str!("fixtures/neural-graph")
        .replace(
            "space NeuralGraph:",
            "scoped_cycle()->f32 = sys.scratch(cycle,8192)\nspace NeuralGraph:",
        )
        .replace("kernel @100 = cycle():", "kernel @100 = scoped_cycle():");
    let p = mira_lang::syntax::parse(&source).unwrap();
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let first = mira_lang::runtime::execute_system(&mut system, 1).unwrap();
    let first = f32::from_bits(first.terminals[0].1 as u32);
    let second = mira_lang::runtime::execute_system(&mut system, 2).unwrap();
    let second = f32::from_bits(second.terminals[0].1 as u32);
    assert!((first - 1.28).abs() < 0.00001 && second < first && second >= 0.0);
}
#[test]
#[ignore = "requires QEMU; verifies bounded temporary memory in native machine code"]
fn native_scratch_reuses_memory_for_a_long_running_program() {
    let boot = "boot()->unit =\n  if run() == 3623\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel = boot()\n";
    let source = SCOPED.replace("space main:\n  result = run()\n", boot);
    let image = native_image(&source, "scoped-memory");
    assert_eq!(qemu(&image, false, "scoped-memory"), "T!");
}

const ATOMICS: &str = "boot()->unit =\n  address = sys.addr(8388608)\n  sys.atomic_store64(address,7)\n  old = sys.atomic_compare_exchange64(address,7,11)\n  success = old == 7 and sys.atomic_load64(address) == 11\n  old = sys.atomic_compare_exchange64(address,7,99)\n  rejected = old == 11 and sys.atomic_load64(address) == 11\n  old = sys.atomic_fetch_add64(address,4)\n  added = old == 11 and sys.atomic_load64(address) == 15\n  old = sys.atomic_exchange64(address,23)\n  exchanged = old == 15 and sys.atomic_load64(address) == 23\n  sys.atomic_store64(address,-1)\n  old = sys.atomic_fetch_add64(address,1)\n  wrapped = old == -1 and sys.atomic_load64(address) == 0\n  sys.memory_fence()\n  if success and rejected and added and exchanged and wrapped\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel = boot()\n";
#[test]
fn atomic_operations_are_typed_and_require_the_native_system_target() {
    let p = mira_lang::syntax::parse(ATOMICS).unwrap();
    assert!(
        mira_lang::runtime::execute(&p, 1)
            .unwrap_err()
            .contains("system operation")
    );
    assert!(
        mira_lang::syntax::parse(&ATOMICS.replace(
            "sys.atomic_store64(address,7)",
            "sys.atomic_store64(7,address)"
        ))
        .is_err()
    );
    let image = mira_lang::kernel::compile(&p).unwrap();
    assert!(
        image
            .windows(5)
            .any(|b| b == [0xf0, 0x48, 0x0f, 0xb1, 0x0a])
    );
    assert!(
        image
            .windows(5)
            .any(|b| b == [0xf0, 0x48, 0x0f, 0xc1, 0x02])
    );
}
#[test]
#[ignore = "requires QEMU; checks atomic values and compare-exchange success/failure"]
fn native_atomics_return_old_values_and_update_memory() {
    let image = native_image(ATOMICS, "atomic-words");
    assert_eq!(qemu(&image, false, "atomic-words"), "T!");
}

#[test]
fn raw_atomic_effects_cannot_escape_the_neural_admission_policy() {
    let source = "worker()->unit = sys.atomic_store64(sys.addr(8388608),7)\nboot()->unit =\n  ok = sys.neural_try(worker,1000,0)\nspace main:\n  kernel = boot()\n";
    let p = mira_lang::syntax::parse(source).unwrap();
    assert!(
        mira_lang::kernel::compile(&p)
            .unwrap_err()
            .contains("forbidden")
    );
}
#[test]
#[ignore = "requires QEMU and OVMF; verifies persistent learning across nested scratch scopes"]
fn native_scoped_neural_state_survives_reclamation_and_runs_on_uefi() {
    let boot = "scoped_cycle()->f32 = sys.scratch(cycle,8192)\nboot()->unit =\n  first = scoped_cycle()\n  second = scoped_cycle()\n  if first > 1.279 and first < 1.281 and second > 0.540 and second < 0.541\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace NeuralGraph:";
    let source = include_str!("fixtures/neural-graph")
        .replace("space NeuralGraph:", boot)
        .replace("kernel @100 = cycle():", "kernel @100 = boot():");
    let image = native_image(&source, "scoped-neural");
    assert_eq!(qemu(&image, false, "scoped-neural"), "T!");
    let p = mira_lang::syntax::parse(&source).unwrap();
    let esp = work().join("scoped-neural-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&p).unwrap(), &esp).unwrap();
    assert!(qemu(&esp, true, "scoped-neural-uefi").ends_with("T!"));
}

#[test]
#[ignore = "requires QEMU; checks source-level typed views after parsing reserved .at"]
fn native_raw_views_read_and_write_the_declared_word_layout() {
    let source = "struct Counters:\n  flags: i64\n  count: i64\nboot()->unit =\n  view = Counters.at(sys.addr(8388608))\n  view.flags = 3\n  view.count = 17\n  if view.flags == 3 and sys.load64(sys.addr(8388616)) == 17\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel = boot()\n";
    let image = native_image(source, "typed-views");
    assert_eq!(qemu(&image, false, "typed-views"), "T!");
}
