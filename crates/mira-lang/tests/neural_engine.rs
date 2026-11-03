mod common;
use common::*;
#[test]
fn f32_and_tensor_values_use_the_existing_behavior_ir() {
    let source = "type Matrix = tensor[f32, 2, 2]\ncalculate()->f32 =\n  m = Matrix(1.5, 2.0, -3.25, 4.0)\n  value = 0.0\n  for i in 0..4\n    value += m[i]\n  value / 2.0\nspace main:\n  result = calculate()\n";
    let p = mira_lang::syntax::parse(source).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&p)).unwrap(),
        p
    );
    let output = mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1;
    assert_eq!(f32::from_bits(output as u32), 2.125);
    assert!(mira_lang::syntax::parse("f()->f32 = 1.0 + 2\n").is_err());
    assert!(mira_lang::syntax::parse("type Bad = tensor[f32, 2, 0]\nf()->i64 = 1\n").is_err());
    let kernel=source.replace("space main:\n  result = calculate()\n", "boot()->unit =\n  if calculate() == 2.125\n    sys.out8(233, 70)\n  sys.halt()\nspace main:\n  kernel = boot()\n");
    std::fs::write(
        work().join("f32-tensor.elf"),
        mira_lang::kernel::compile(&mira_lang::syntax::parse(&kernel).unwrap()).unwrap(),
    )
    .unwrap();
}

#[test]
fn bulk_tensor_operations_and_nominal_relations_are_programmable() {
    let source = "type Nodes = tensor[f32,3,2]\ntype Messages = tensor[f32,2,2]\ntype Indices = [i64,2]\ntype Weights = tensor[f32,2,2]\nstruct Synapses:\n  sources: Nodes\n  targets: Nodes\n  source_indices: Indices\n  target_indices: Indices\nrun()->f32 =\n  nodes = Nodes(1.0,2.0,3.0,4.0,5.0,6.0)\n  sources = Indices(2,0)\n  targets = Indices(1,1)\n  links = Synapses(nodes,nodes,sources,targets)\n  messages = Messages()\n  transformed = Messages()\n  out = Nodes()\n  weights = Weights(2.0,0.0,0.0,2.0)\n  relation.messages_into(messages,links)\n  tensor.matmul_into(transformed,messages,weights)\n  relation.aggregate_into(out,links,transformed)\n  tensor.sum(out)\nspace main:\n  result = run()\n";
    let p = mira_lang::syntax::parse(source).unwrap();
    assert_eq!(
        mira_lang::syntax::parse(&mira_lang::syntax::format(&p)).unwrap(),
        p
    );
    let a = mira_lang::runtime::execute(&p, 1).unwrap();
    assert_eq!(f32::from_bits(a.terminals[0].1 as u32), 28.0);
    let kernel=source.replace("space main:\n  result = run()\n", "boot()->unit =\n  if run() == 28.0\n    sys.out8(233, 84)\n  sys.halt()\nspace main:\n  kernel = boot()\n");
    std::fs::write(
        work().join("bulk-tensor.elf"),
        mira_lang::kernel::compile(&mira_lang::syntax::parse(&kernel).unwrap()).unwrap(),
    )
    .unwrap();
    assert!(mira_lang::syntax::parse(&source.replace(
        "type Weights = tensor[f32,2,2]",
        "type Weights = tensor[f32,1,4]"
    ))
    .is_err());
}
#[test]
fn persistent_tensors_survive_execution_and_canonical_storage() {
    let source = "type State = tensor[f32,2]\nstate()->State = State.persist()\nstep()->f32 =\n  value = cells.memory()\n  value[0] += 1.5\n  value[1] = value[0] * 2.0\n  tensor.sum(value)\nspace main:\n  result @2 = step():\n    space internal @3:\n      memory @4 = state()\n";
    let mut s =
        mira_core::system::System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap();
    let a = mira_lang::runtime::execute_system(&mut s, 1).unwrap();
    assert_eq!(f32::from_bits(a.terminals[0].1 as u32), 4.5);
    let bytes = mira_lang::system::encode(&s).unwrap();
    assert_eq!(bytes[4], 3);
    let mut restored = mira_lang::system::decode(&bytes).unwrap();
    assert_eq!(restored, s);
    let a = mira_lang::runtime::execute_system(&mut restored, 2).unwrap();
    assert_eq!(f32::from_bits(a.terminals[0].1 as u32), 9.0);
    assert_eq!(restored.buffers[&4].words[0], 3.0f32.to_bits() as i64);
}

#[test]
fn neural_regions_commit_and_recover_without_publishing_partial_buffers() {
    let source = "type Weights = tensor[f32,2]\ntype Scratch = tensor[f32,8]\nweights()->Weights = Weights.persist()\ntrain()->unit =\n  w = cells.weights()\n  w[0] += 1.5\n  w[1] = w[0] * 2.0\nspin()->unit =\n  w = cells.weights()\n  w[0] = 99.0\n  while true\n    w[0] += 1.0\nmemory_failure()->unit =\n  w = cells.weights()\n  w[0] = 99.0\n  scratch = Scratch()\nbounds_failure()->unit =\n  w = cells.weights()\n  w[5] = 99.0\nstep()->bool =\n  held = cells.weights()\n  ok = sys.neural_try(train, 10000, 1024)\n  trained = ok and held[0] == 1.5 and held[1] == 3.0\n  bad = sys.neural_try(spin, 100, 1024)\n  bounded = not bad and sys.neural_status() == 1 and held[0] == 1.5\n  bad = sys.neural_try(memory_failure, 1000, 0)\n  memory_safe = not bad and sys.neural_status() == 2 and held[0] == 1.5\n  bad = sys.neural_try(bounds_failure, 1000, 1024)\n  bounded and trained and memory_safe and not bad and sys.neural_status() == 3 and held[0] == 1.5\nspace main:\n  result @2 = step():\n    space internal @3:\n      weights @4 = weights()\n";
    let p = mira_lang::syntax::parse(source).unwrap();
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let a = mira_lang::runtime::execute_system(&mut system, 1).unwrap();
    assert_eq!(a.terminals[0].1, 1);
    assert_eq!(
        system.buffers[&4].words,
        vec![1.5f32.to_bits() as i64, 3.0f32.to_bits() as i64]
    );
    let native=source.replace("space main:\n  result @2 = step():", "boot()->unit =\n  if step()\n    sys.out8(233, 84)\n  else\n    sys.out8(233, 70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel @2 = boot():");
    let p = mira_lang::syntax::parse(&native).unwrap();
    std::fs::write(
        work().join("neural-region.elf"),
        mira_lang::kernel::compile(&p).unwrap(),
    )
    .unwrap();
    let unsafe_source = source.replace("w[0] += 1.5", "sys.out8(233,65)");
    let p = mira_lang::syntax::parse(&unsafe_source).unwrap();
    assert!(mira_lang::runtime::execute(&p, 1)
        .unwrap_err()
        .contains("forbidden"));
    let unsafe_native = native.replace("w[0] += 1.5", "sys.out8(233,65)");
    let p = mira_lang::syntax::parse(&unsafe_native).unwrap();
    assert!(mira_lang::kernel::compile(&p)
        .unwrap_err()
        .contains("forbidden"));
}

#[test]
fn graph_network_learns_remembers_restores_and_compiles_the_same_mira_code() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/neural-graph");
    let source = std::fs::read_to_string(path).unwrap();
    let p = mira_lang::syntax::parse(&source).unwrap();
    let mut s = mira_core::system::System::from_program(p).unwrap();
    let first = mira_lang::runtime::execute_system(&mut s, 1).unwrap();
    let first = f32::from_bits(first.terminals[0].1 as u32);
    assert!((first - 1.28).abs() < 0.00001, "{first}");
    assert_eq!(
        s.buffers[&103].words,
        vec![
            1.1f32.to_bits() as i64,
            0.1f32.to_bits() as i64,
            0.1f32.to_bits() as i64,
            1.1f32.to_bits() as i64
        ]
    );
    let bytes = mira_lang::system::encode(&s).unwrap();
    let mut s = mira_lang::system::decode(&bytes).unwrap();
    let second = mira_lang::runtime::execute_system(&mut s, 2).unwrap();
    let second = f32::from_bits(second.terminals[0].1 as u32);
    assert!(second >= 0.0 && second < first, "{second}");
    let boot = "boot()->unit =\n  result = cycle()\n  if result > 1.279 and result < 1.281\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.halt()\n";
    let native = source
        .replace("space NeuralGraph:", &format!("{boot}space NeuralGraph:"))
        .replace("kernel @100 = cycle():", "kernel @100 = boot():");
    let p = mira_lang::syntax::parse(&native).unwrap();
    std::fs::write(
        work().join("graph-network.elf"),
        mira_lang::kernel::compile(&p).unwrap(),
    )
    .unwrap();
    mira_lang::uefi::write_esp(
        &mira_lang::uefi::compile(&p).unwrap(),
        &work().join("graph-network-esp"),
    )
    .unwrap();
}

#[test]
fn relation_neighbors_validate_endpoints_and_do_not_truncate() {
    let source = "type Nodes = tensor[f32,3,1]\ntype Edges = [i64,3]\nstruct Links:\n  sources: Nodes\n  targets: Nodes\n  source_indices: Edges\n  target_indices: Edges\nrun()->i64 =\n  nodes = Nodes()\n  source = Edges(2,0,1)\n  target = Edges(1,2,1)\n  relation = Links(nodes,nodes,source,target)\n  out = Edges()\n  found = relation.neighbors_into(out,relation,1)\n  found * 100 + out[0] * 10 + out[1]\nspace main:\n  result = run()\n";
    let p = mira_lang::syntax::parse(source).unwrap();
    assert_eq!(
        mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1,
        221
    );
    let p = mira_lang::syntax::parse(&source.replace("Edges(1,2,1)", "Edges(1,3,1)")).unwrap();
    assert!(mira_lang::runtime::execute(&p, 1).is_err());
    assert!(mira_lang::syntax::parse(&source.replace("out = Edges()", "out = Nodes()")).is_err());
}

// These tests execute generated Mira machine code in an actual bare-metal guest.
// Run explicitly with: cargo test -p mira-lang --test neural_engine -- --ignored
fn boot_source(source: &str, boot: &str) -> String {
    source
        .replace("space NeuralGraph:", &format!("{boot}\nspace NeuralGraph:"))
        .replace("kernel @100 = cycle():", "kernel @100 = boot():")
}
#[test]
#[ignore = "requires QEMU; tests real native IRQs and rollback"]
fn native_irq_captures_events_and_invalidates_stale_learning() {
    let source = include_str!("fixtures/neural_irq.mira");
    let image = native_image(source, "irq-neural");
    assert_eq!(qemu(&image, false, "irq-neural"), "T!");
}
#[test]
#[ignore = "requires QEMU; tests native budget, stack and numeric recovery"]
fn native_resource_failures_leave_live_buffers_intact() {
    let source = "type Weights = tensor[f32,2]\nweights()->Weights = Weights.persist()\nrecurse(n:i64)->unit = recurse(n+1)\nstack_failure()->unit =\n  cells.weights()[0] = 99.0\n  recurse(0)\nnumeric_failure()->unit =\n  cells.weights()[0] = 99.0\n  cells.weights()[1] = 1.0 / 0.0\nspin()->unit =\n  cells.weights()[0] = 99.0\n  while true\n    cells.weights()[1] += 1.0\nboot()->unit =\n  ok = sys.neural_try(spin,1,4096)\n  copy_safe = not ok and sys.neural_status() == 1 and cells.weights()[0] == 0.0\n  ok = sys.neural_try(stack_failure,10000000,4096)\n  stack_safe = not ok and sys.neural_status() == 2 and cells.weights()[0] == 0.0\n  ok = sys.neural_try(numeric_failure,10000,4096)\n  numeric_safe = not ok and sys.neural_status() == 4 and cells.weights()[0] == 0.0\n  ok = sys.neural_try(spin,100,4096)\n  if copy_safe and stack_safe and numeric_safe and not ok and sys.neural_status() == 1 and cells.weights()[0] == 0.0\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace Check:\n  kernel @100 = boot():\n    space internal @101:\n      weights @102 = weights()\n";
    let image = native_image(source, "native-failures");
    assert_eq!(qemu(&image, false, "native-failures"), "T!");
}
fn export_boot() -> String {
    let mut boot = String::from(
        "hex(value:i64)->unit =\n  for i in 0..8\n    digit = (value >> ((7-i)*4)) & 15\n    if digit < 10\n      sys.out8(233,48+digit)\n    else\n      sys.out8(233,87+digit)\nboot()->unit =\n  result = cycle()\n  hex(math.bits(result))\n  sys.out8(233,10)\n",
    );
    for (name, len, float) in [
        ("initialized", 1, false),
        ("weights", 4, true),
        ("memory", 6, true),
        ("output", 6, true),
        ("input", 6, true),
        ("sources", 2, false),
        ("targets", 2, false),
        ("features", 2, true),
        ("saved_weights", 4, true),
        ("saved_memory", 6, true),
    ] {
        boot += &format!(
            "  for i in 0..{len}\n    hex({})\n  sys.out8(233,10)\n",
            if float {
                format!("math.bits(cells.{name}()[i])")
            } else {
                format!("cells.{name}()[i]")
            }
        );
    }
    boot + "  sys.out8(233,33)\n  sys.halt()\n"
}
#[test]
#[ignore = "requires QEMU; serializes values trained inside MiraOS and boots them again"]
fn native_trained_graph_checkpoint_survives_canonical_save_and_reboot() {
    let source = include_str!("fixtures/neural-graph");
    let source = boot_source(source, &export_boot());
    let p = mira_lang::syntax::parse(&source).unwrap();
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let image = work().join("checkpoint.elf");
    std::fs::write(&image, mira_lang::kernel::compile_system(&system).unwrap()).unwrap();
    let output = qemu(&image, false, "checkpoint-first");
    let mut lines = output.lines();
    let first = f32::from_bits(u32::from_str_radix(lines.next().unwrap(), 16).unwrap());
    assert!((first - 1.28).abs() < 0.00001);
    // Transport only: the host copies guest-produced bits, never computes updates.
    for (id, name) in [
        (102, "Flag"),
        (103, "Weights"),
        (104, "Nodes"),
        (105, "Nodes"),
        (106, "Nodes"),
        (107, "Indices"),
        (108, "Indices"),
        (109, "EdgeFeatures"),
        (110, "Weights"),
        (111, "Nodes"),
    ] {
        let line = lines.next().unwrap();
        let words = line
            .as_bytes()
            .chunks_exact(8)
            .map(|chunk| i64::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
            .collect();
        let ty = system
            .program
            .data_types()
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .id;
        system
            .buffers
            .insert(id, mira_core::data::Buffer { ty, words });
    }
    let bytes = mira_lang::system::encode(&system).unwrap();
    let restored = mira_lang::system::decode(&bytes).unwrap();
    assert_eq!(restored, system);
    std::fs::write(
        &image,
        mira_lang::kernel::compile_system(&restored).unwrap(),
    )
    .unwrap();
    let output = qemu(&image, false, "checkpoint-second");
    let second = f32::from_bits(u32::from_str_radix(output.lines().next().unwrap(), 16).unwrap());
    assert!(second >= 0.0 && second < first, "{first} -> {second}");
}
#[test]
#[ignore = "requires QEMU and OVMF firmware; runs the same Mira graph after ExitBootServices"]
fn native_graph_learning_runs_on_uefi() {
    let source = include_str!("fixtures/neural-graph");
    let boot = "boot()->unit =\n  result = cycle()\n  if result > 1.279 and result < 1.281\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\n";
    let source = boot_source(source, boot);
    let p = mira_lang::syntax::parse(&source).unwrap();
    let esp = work().join("graph-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&p).unwrap(), &esp).unwrap();
    assert!(qemu(&esp, true, "graph-uefi").ends_with("T!"));
}

#[test]
#[ignore = "requires QEMU; checks stable shared handles, scratch and bounds rollback"]
fn native_region_handles_remain_valid_after_commit_and_recovery() {
    neural_regions_commit_and_recover_without_publishing_partial_buffers();
    assert_eq!(
        qemu(&work().join("neural-region.elf"), false, "region-handles"),
        "T!"
    );
}

#[test]
fn selected_execution_and_structural_history_preserve_learned_buffers() {
    let source = "type State = tensor[f32,1]\nstate()->State = State.persist()\nstep()->f32 =\n  cells.memory()[0] += 1.0\n  cells.memory()[0]\nspace main:\n  result @2 = step():\n    space internal @3:\n      memory @4 = state()\n";
    let mut s =
        mira_core::system::System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap();
    mira_lang::runtime::execute_cell_system(&mut s, 2, 1).unwrap();
    let mut h = mira_lang::system::History::default();
    h.commit(&mut s, "Rename graph", &Default::default(), |s| {
        s.program.root.name = "renamed".into();
        Ok(())
    })
    .unwrap();
    mira_lang::runtime::execute_cell_system(&mut s, 2, 2).unwrap();
    let trained = s.buffers.clone();
    h.undo(&mut s).unwrap();
    assert_eq!(s.program.root.name, "main");
    assert_eq!(s.buffers, trained);
    h.redo(&mut s).unwrap();
    assert_eq!(s.program.root.name, "renamed");
    assert_eq!(s.buffers, trained);
    h.checkpoint(&s, "Learned state".into(), true);
    mira_lang::runtime::execute_cell_system(&mut s, 2, 3).unwrap();
    assert_ne!(s.buffers, trained);
    h.rollback(&mut s, 0, true).unwrap();
    assert_eq!(s.buffers, trained);
}

#[test]
fn persistent_records_keep_typed_fields_and_transactional_rollback() {
    let source = "struct Record:\n  count: i64\n  enabled: bool\n  score: f32\nstate()->Record = Record.persist()\nlearn()->unit =\n  s = cells.state()\n  s.count += 1\n  s.enabled = true\n  s.score += 0.5\nfail()->unit =\n  cells.state().count = 999\n  invalid = 1 / 0\nrun()->bool =\n  ok = sys.neural_try(learn,10000,1024)\n  bad = sys.neural_try(fail,10000,1024)\n  s = cells.state()\n  ok and not bad and s.count == 1 and s.enabled and s.score == 0.5\nspace Main:\n  result = run():\n    space State:\n      state = state()\n";
    let mut system =
        mira_core::system::System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap();
    assert_eq!(
        mira_lang::runtime::execute_system(&mut system, 1)
            .unwrap()
            .terminals[0]
            .1,
        1
    );
    let bytes = mira_lang::system::encode(&system).unwrap();
    assert_eq!(mira_lang::system::decode(&bytes).unwrap(), system);
    let mut invalid = system.buffers.values().next().unwrap().clone();
    invalid.words[1] = 2;
    assert!(invalid
        .validate(&system.program.data_types())
        .unwrap_err()
        .contains("boolean"));
    invalid.words[1] = 1;
    invalid.words[2] = f32::NAN.to_bits() as i64;
    assert!(invalid.validate(&system.program.data_types()).is_err());
    let mut oversized = system.program.data_types();
    oversized[0].kind = mira_core::data::Kind::Array {
        element: mira_core::Type::I64,
        length: usize::MAX,
    };
    let invalid = mira_core::data::Buffer {
        ty: oversized[0].id,
        words: vec![],
    };
    assert!(invalid.validate(&oversized).is_err());
    for invalid in [
        source.replace("score: f32", "score: addr"),
        source.replace("count: i64", "count: Record"),
    ] {
        assert!(mira_lang::syntax::parse(&invalid).is_err());
    }
}
