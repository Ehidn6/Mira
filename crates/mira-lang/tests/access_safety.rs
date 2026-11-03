mod common;
use common::*;
const PROGRAM: &str = "type Values = tensor[f32,2]\nvalues()->Values = Values.persist()\ninitialize()->unit =\n  cells.weights()[0] = 3.0\n  cells.weights()[1] = 4.0\ntransform(input:Values)->Values =\n  out = Values()\n  for i in 0..2\n    out[i] = input[i] * 2.0\n  out\ninspect()->f32 =\n  alias = cells.weights()\n  tmp = transform(alias)\n  tmp[0] += 1.0\n  tensor.sum(tmp)\nrun()->f32 =\n  initialize()\n  value = sys.read_only(inspect,4096)\n  value + tensor.sum(cells.weights())\nspace main:\n  result @100 = run():\n    space internal @101:\n      weights @102 = values()\n";
fn assert_rejected(source: &str, fragment: &str) {
    let p = mira_lang::syntax::parse(source).unwrap();
    let error = mira_lang::runtime::execute(&p, 1).unwrap_err();
    assert!(error.contains(fragment), "{error}");
    let native = source.replace(
        "space main:\n  result @100 = run():",
        "boot()->unit =\n  result = run()\nspace main:\n  kernel @100 = boot():",
    );
    let p = mira_lang::syntax::parse(&native).unwrap();
    let error = mira_lang::kernel::compile(&p).unwrap_err();
    assert!(error.contains(fragment), "{error}");
}
#[test]
fn read_only_scopes_allow_local_writes_and_read_shared_parameters() {
    let p = mira_lang::syntax::parse(PROGRAM).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&p)).unwrap(),
        p
    );
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let a = mira_lang::runtime::execute_system(&mut system, 1).unwrap();
    assert_eq!(f32::from_bits(a.terminals[0].1 as u32), 22.0);
    assert_eq!(
        system.buffers[&102].words,
        vec![3.0f32.to_bits() as i64, 4.0f32.to_bits() as i64]
    );
}
#[test]
fn write_permissions_follow_aliases_helper_returns_and_control_flow() {
    for mutator in [
        "alias[0] += 1.0",
        "write(alias)",
        "returned = identity(alias)\n  returned[1] = 5.0",
        "record = Holder(alias)\n  record.value[0] = 5.0",
        "chosen = Values()\n  if true\n    chosen = alias\n  chosen[0] = 5.0",
        "chosen = Values()\n  for i in 0..2\n    chosen[0] = 1.0\n    chosen = alias",
        "chosen = recursive(alias,2)\n  chosen[0] = 5.0",
        "tensor.fill(alias,0.0)",
        "nested = sys.scratch(mutate,32)",
    ] {
        let helpers = "struct Holder:\n  value: Values\nwrite(input:Values)->unit =\n  input[0] = 5.0\nidentity(input:Values)->Values = input\nrecursive(input:Values,n:i64)->Values =\n  if n == 0\n    input\n  else\n    recursive(input,n-1)\nmutate()->unit = cells.weights()[0] = 5.0\n";
        // Assignments use a multiline unit body.
        let helpers = helpers.replace(
            "mutate()->unit = cells.weights()[0] = 5.0",
            "mutate()->unit =\n  cells.weights()[0] = 5.0",
        );
        let source = PROGRAM
            .replace("inspect()->f32 =", &format!("{helpers}inspect()->f32 ="))
            .replace(
                "  tmp = transform(alias)",
                &format!("  {mutator}\n  tmp = transform(alias)"),
            );
        assert_rejected(&source, "modify shared data");
    }
}
#[test]
fn read_only_scopes_reject_hardware_effects_and_reference_rewiring() {
    assert_rejected(
        &PROGRAM.replace(
            "  tmp = transform(alias)",
            "  sys.out8(233,65)\n  tmp = transform(alias)",
        ),
        "forbids effect",
    );
    let source=PROGRAM.replace("inspect()->f32 =","struct Holder:\n  value: Values\ninspect()->f32 =")
        .replace("  tmp = transform(alias)","  first = Holder(Values())\n  second = first\n  second.value = alias\n  tmp = transform(first.value)");
    assert_rejected(&source, "reference reassignment");
}
#[test]
fn arena_keys_are_retired_when_slots_are_reused() {
    let mut arena = mira_lang::data_runtime::Arena::default();
    let permanent = arena.allocate(1, vec![7]).unwrap();
    let mark = arena.mark();
    let old = arena.allocate(1, vec![11]).unwrap();
    arena.reclaim(mark);
    let new = arena.allocate(1, vec![23]).unwrap();
    assert_ne!(old, new);
    assert!(arena.get(old, 0).is_err());
    assert!(arena.set(old, 0, 99).is_err());
    assert_eq!(arena.get(new, 0).unwrap(), 23);
    assert_eq!(arena.get(permanent, 0).unwrap(), 7);
}
#[test]
#[ignore = "requires QEMU and OVMF; verifies checked shared reads and local writes natively"]
fn native_read_only_graph_computation_preserves_shared_weights() {
    let boot = "boot()->unit =\n  if run() == 22.0\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel @100 = boot():";
    let source = PROGRAM.replace("space main:\n  result @100 = run():", boot);
    let image = native_image(&source, "read-only");
    assert_eq!(qemu(&image, false, "read-only"), "T!");
    let p = mira_lang::syntax::parse(&source).unwrap();
    let esp = work().join("read-only-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&p).unwrap(), &esp).unwrap();
    assert!(qemu(&esp, true, "read-only-uefi").ends_with("T!"));
}

#[test]
fn serialized_source_cannot_forge_private_buffer_or_pointer_operations() {
    let mut p = mira_lang::syntax::parse(PROGRAM).unwrap();
    let ty = p
        .data_types()
        .iter()
        .find(|t| t.name == "Values")
        .unwrap()
        .id;
    let inspect = p
        .code_modules
        .iter_mut()
        .find(|d| d.name == "inspect")
        .unwrap();
    inspect.body = vec![mira_core::code::Statement::Expr(
        mira_core::code::Expr::Call(
            format!("#data.identity.{ty}"),
            vec![mira_core::code::Expr::Construct(ty, vec![])],
        ),
    )];
    assert!(
        mira_lang::runtime::execute(&p, 1)
            .unwrap_err()
            .contains("compiler-private")
    );
    assert!(
        mira_lang::kernel::compile(&p)
            .unwrap_err()
            .contains("compiler-private")
    );
    let system = mira_core::system::System::from_program(p).unwrap();
    assert!(
        mira_lang::system::encode(&system)
            .unwrap_err()
            .contains("compiler-private")
    );
}

fn bounded_source() -> String {
    PROGRAM.replace("inspect()->f32 =","bounded()->unit =\n  cells.result()[0] = sys.read_only(inspect,4096)\nspin()->i64 =\n  while true\n    value = 1\n  0\nfail()->unit =\n  value = sys.read_only(spin,32)\n  cells.result()[0] = 99.0\ninspect()->f32 =")
        .replace("  value = sys.read_only(inspect,4096)","  ok = sys.neural_try(bounded,20000,8192)\n  bad = sys.neural_try(fail,100,64)\n  if not ok or bad or sys.neural_status() != 1\n    cells.result()[0] = -99.0\n  value = cells.result()[0]")
        .replace("      weights @102 = values()","      weights @102 = values()\n      result @103 = values()")
}
#[test]
fn read_only_inside_neural_execution_retains_budget_and_rollback() {
    let p = mira_lang::syntax::parse(&bounded_source()).unwrap();
    let a = mira_lang::runtime::execute(&p, 1).unwrap();
    assert_eq!(f32::from_bits(a.terminals[0].1 as u32), 22.0);
}
#[test]
#[ignore = "requires QEMU; tests read permission composed with neural fuel and recovery"]
fn native_bounded_read_only_execution_recovers_from_budget_exhaustion() {
    let source=bounded_source().replace("space main:\n  result @100 = run():","boot()->unit =\n  if run() == 22.0\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel @100 = boot():");
    let image = native_image(&source, "bounded-read-only");
    assert_eq!(qemu(&image, false, "bounded-read-only"), "T!");
}
