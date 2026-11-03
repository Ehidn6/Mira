//! Execute the compiler produced from its typed Mira source, then rebuild that
//! same source without Rust parsing, aggregate lowering, linking or codegen.
use std::collections::BTreeMap;
#[allow(dead_code)]
mod native {
    use crate as mira_lang;
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/common/mod.rs"));
}
pub(super) fn regenerate_native(source: &str, code: &[u8], exports: &BTreeMap<String, usize>) {
    assert!(code.len() < 1048576 && source.len() < 262144);
    let mut model = crate::bootstrap_compiler::library()
        .unwrap()
        .program
        .clone();
    model.code_modules.clear();
    let types = crate::syntax::format(&model);
    let mut fixture = types[..types.rfind("\nspace ").unwrap()].to_owned();
    fixture.push_str(r#"
type VerifyContext = [i64,17]
type VerifyExecution = [i64,131072]
type VerifyMachine = [i64,4]
verify_input()->ScalarSource = ScalarSource.persist()
verify_parameters()->ScalarParameters = ScalarParameters.persist()
verify_nodes()->ScalarNodes = ScalarNodes.persist()
verify_output()->ScalarBytes = ScalarBytes.persist()
verify_body()->ScalarBytes = ScalarBytes.persist()
verify_symbols()->ModuleSymbols = ModuleSymbols.persist()
verify_fixups()->ScalarFixups = ScalarFixups.persist()
verify_relocations()->ScalarFixups = ScalarFixups.persist()
verify_execution()->VerifyExecution = VerifyExecution.persist()
verify_boot()->unit =
  info = sys.boot_info()
  assets = sys.addr(sys.load64(sys.offset(info,248)))
  payload = sys.addr(sys.load64(sys.offset(assets,24)))
  payload_size = sys.load64(sys.offset(assets,32))
  size = sys.load64(sys.offset(payload,8))
  length = sys.load64(sys.offset(payload,16))
  test_length = sys.load64(sys.offset(payload,72))
  ok = test_length > 0 and test_length <= ScalarSource.size_bytes() / 8 and sys.load64(payload) == 0x33504d434152494d and size > 0 and size <= VerifyExecution.size_bytes() and length > 0 and length <= ScalarSource.size_bytes() / 8 and size + length + test_length + 80 == payload_size
  if not ok
    sys.out8(233,70)
    sys.out8(233,48)
    sys.halt()
  executable = sys.data_address(cells.execution())
  for byte in 0..size
    sys.store8(sys.offset(executable,byte),sys.load8(sys.offset(payload,80 + byte)))
  for byte in 0..length
    cells.input()[byte] = sys.load8(sys.offset(payload,80 + size + byte))
  barrier = sys.cpuid_eax(0,0)
  sys.out8(233,67)
  context = VerifyContext()
  context_address = sys.data_address(context)
  state = ModuleState()
  work = ScalarWork()
  lexer = ScalarLexer(LexBytes(),LexState())
  literal = LiteralBytes()
  value = LiteralValue()
  emission = ScalarCode()
  // First regenerate the compiler; then use that regenerated compiler to
  // parse/check/emit/link the ordinary tensor example inside the guest.
  for phase in 0..2
    if ok
      if phase == 1
        for byte in 0..test_length
          cells.input()[byte] = sys.load8(sys.offset(payload,80 + size + length + byte))
      context[0] = sys.address_value(sys.data_address(cells.input()))
      context[1] = phase == 0 ? length : test_length
      context[2] = phase == 0 ? 258 : 120
      context[3] = sys.address_value(sys.data_address(state))
      context[4] = sys.address_value(sys.data_address(cells.symbols()))
      context[5] = sys.address_value(sys.data_address(work))
      context[6] = sys.address_value(sys.data_address(lexer))
      context[7] = sys.address_value(sys.data_address(literal))
      context[8] = sys.address_value(sys.data_address(value))
      context[9] = sys.address_value(sys.data_address(cells.parameters()))
      indexed = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,24))),context_address)
      ok = indexed == 0 and state.error == 0 and context[16] == 0
      sys.out8(233,ok ? 73 : 70)
      context[0] = sys.address_value(sys.data_address(state))
      context[1] = sys.address_value(sys.data_address(cells.symbols()))
      context[2] = sys.address_value(sys.data_address(cells.parameters()))
      prepared = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,32))),context_address)
      ok = ok and state.error == 0 and context[16] == 0
      context[0] = sys.address_value(sys.data_address(cells.input()))
      context[1] = sys.address_value(sys.data_address(state))
      context[2] = sys.address_value(sys.data_address(cells.symbols()))
      context[3] = sys.address_value(sys.data_address(cells.parameters()))
      context[4] = sys.address_value(sys.data_address(work))
      context[5] = sys.address_value(sys.data_address(lexer))
      context[6] = sys.address_value(sys.data_address(literal))
      context[7] = sys.address_value(sys.data_address(value))
      context[8] = sys.address_value(sys.data_address(cells.nodes()))
      context[9] = sys.address_value(sys.data_address(emission))
      context[10] = sys.address_value(sys.data_address(cells.body()))
      context[11] = sys.address_value(sys.data_address(cells.output()))
      context[12] = sys.address_value(sys.data_address(cells.fixups()))
      context[13] = sys.address_value(sys.data_address(cells.relocations()))
      while ok and state.emitted < state.count
        generated = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,40))),context_address)
        ok = generated > 0 and state.error == 0 and context[16] == 0
        sys.out8(233,ok ? 46 : 70)
      context[0] = sys.address_value(sys.data_address(state))
      context[1] = sys.address_value(sys.data_address(cells.symbols()))
      context[2] = sys.address_value(sys.data_address(cells.output()))
      context[3] = sys.address_value(sys.data_address(cells.relocations()))
      context[4] = 200000000
      if ok
        linked = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,48))),context_address)
        ok = (phase == 0 ? linked == size : linked > 0 and linked <= VerifyExecution.size_bytes()) and state.error == 0 and context[16] == 0
      sys.out8(233,ok ? (phase == 0 ? 76 : 84) : 70)
      if ok and phase == 0
        for byte in 0..size
          if cells.output()[byte] != sys.load8(sys.offset(payload,80 + byte))
            ok = false
        sys.out8(233,ok ? 61 : 70)
      // Execute the freshly regenerated compiler, not just compare its bytes.
      if ok and phase == 0
        for byte in 0..size
          sys.store8(sys.offset(executable,byte),cells.output()[byte])
        barrier = sys.cpuid_eax(0,0)
        machine = VerifyMachine.aligned(16)
        machine_address = sys.data_address(machine)
        context[0] = 0
        context[1] = sys.address_value(machine_address)
        context[2] = 32
        emitted = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,56))),context_address)
        barrier = sys.cpuid_eax(0,0)
        ok = emitted == 10 and context[16] == 0 and sys.call_sysv64(machine_address,7,9) == 16
        context[0] = -9223372036854775808
        emitted = sys.call_sysv64(sys.offset(executable,sys.load64(sys.offset(payload,64))),context_address)
        barrier = sys.cpuid_eax(0,0)
        ok = ok and emitted == 11 and context[16] == 0 and sys.call_sysv64(machine_address) == -9223372036854775808
      if ok and phase == 0
        natural_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 303
            natural_entry = cells.symbols()[index * 128 + 101]
        ok = natural_entry >= 0
        if ok
          largest = LiteralBytes("18446744073709551615")
          context[0] = sys.address_value(sys.data_address(largest))
          context[1] = 20
          context[2] = sys.address_value(sys.data_address(value))
          parsed = sys.call_sysv64(sys.offset(executable,natural_entry),context_address)
          ok = parsed == 0 and value[0] == -1 and context[16] == 0
          largest[19] = 54
          parsed = sys.call_sysv64(sys.offset(executable,natural_entry),context_address)
          ok = ok and parsed == 2 and value[0] == -1 and context[16] == 0
      if ok and phase == 0
        float_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 393
            float_entry = cells.symbols()[index * 128 + 101]
        ok = float_entry >= 0
        if ok
          decimal = LiteralBytes("1.000000059604644775390625")
          context[0] = sys.address_value(sys.data_address(decimal))
          context[1] = 26
          context[2] = sys.address_value(sys.data_address(value))
          parsed = sys.call_sysv64(sys.offset(executable,float_entry),context_address)
          ok = parsed == 0 and value[0] == 1065353216 and context[16] == 0
          decimal[25] = 54
          parsed = sys.call_sysv64(sys.offset(executable,float_entry),context_address)
          ok = ok and parsed == 0 and value[0] == 1065353217 and context[16] == 0
          decimal[1] = 101
          parsed = sys.call_sysv64(sys.offset(executable,float_entry),context_address)
          ok = ok and parsed == 2 and value[0] == 1065353217 and context[16] == 0
      if ok and phase == 1
        for byte in 0..state.size
          sys.store8(sys.offset(executable,byte),cells.output()[byte])
        barrier = sys.cpuid_eax(0,0)
        context[16] = 0
        result = sys.call_sysv64(sys.offset(executable,state.entry),context_address)
        ok = result == 41 and context[16] == 0
        // Public Data returns must preserve caller identity across independent
        // calls, temporary allocations and a trapped call using the same context.
        borrowed_entry = -1
        child_entry = -1
        update_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 185
            borrowed_entry = cells.symbols()[index * 128 + 101]
          if cells.symbols()[index * 128 + 19] == 184
            child_entry = cells.symbols()[index * 128 + 101]
          if cells.symbols()[index * 128 + 19] == 190
            update_entry = cells.symbols()[index * 128 + 101]
        ok = ok and borrowed_entry >= 0 and child_entry >= 0 and update_entry >= 0
        if ok
          left_loan = VerifyMachine(10,20,99,100)
          right_loan = VerifyMachine(30,40,97,98)
          left_address = sys.address_value(sys.data_address(left_loan))
          right_address = sys.address_value(sys.data_address(right_loan))
          for trial in 0..8
            context[0] = trial % 2 == 0 ? left_address : right_address
            context[1] = 3
            context[16] = 55
            returned = sys.call_sysv64(sys.offset(executable,borrowed_entry),context_address)
            ok = ok and returned == context[0] and context[16] == 0
          loan_view = VerifyMachine(left_address,0,0,0)
          context[0] = sys.address_value(sys.data_address(loan_view))
          returned = sys.call_sysv64(sys.offset(executable,child_entry),context_address)
          ok = ok and returned == left_address and context[16] == 0
          context[0] = left_address
          context[1] = 2
          rejected = sys.call_sysv64(sys.offset(executable,update_entry),context_address)
          ok = ok and context[16] == 6 and left_loan[0] == 10 and left_loan[1] == 20 and left_loan[2] == 99 and left_loan[3] == 100
          // No manual error reset: this is a new public invocation.
          context[1] = 1
          returned = sys.call_sysv64(sys.offset(executable,update_entry),context_address)
          ok = ok and returned == left_address and context[16] == 0 and left_loan[0] == 10 and left_loan[1] == 21 and left_loan[2] == 99 and left_loan[3] == 100 and right_loan[0] == 30 and right_loan[1] == 40 and right_loan[2] == 97 and right_loan[3] == 98
        // The same compiled multiply must reject aliased output before a write.
        alias_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 5
            alias_entry = cells.symbols()[index * 128 + 101]
        ok = ok and alias_entry >= 0
        if ok
          aliased = VerifyMachine(1065353216,1073741824,1077936128,1082130432)
          context[0] = sys.address_value(sys.data_address(aliased))
          context[1] = context[0]
          context[2] = context[0]
          context[16] = 0
          rejected = sys.call_sysv64(sys.offset(executable,alias_entry),context_address)
          ok = context[16] == 3 and aliased[0] == 1065353216 and aliased[1] == 1073741824 and aliased[2] == 1077936128 and aliased[3] == 1082130432
        // Check late corrupt source AND target indices before any message write.
        messages_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 109
            messages_entry = cells.symbols()[index * 128 + 101]
        ok = ok and messages_entry >= 0
        if ok
          source_nodes = VerifyContext()
          target_nodes = VerifyContext()
          edge_from = VerifyContext()
          edge_to = VerifyContext()
          edge_mask = VerifyContext()
          edge_features = VerifyContext()
          links = VerifyContext()
          messages = VerifyContext()
          for index in 0..8
            messages[index] = 1065353216
          edge_from[0] = 0
          edge_from[1] = 2
          edge_from[2] = 3
          edge_from[3] = 999
          edge_to[0] = 1
          edge_to[1] = 0
          edge_to[2] = 1
          edge_to[3] = -999
          edge_mask[0] = 1
          edge_mask[1] = 1
          edge_mask[2] = 1
          links[0] = sys.address_value(sys.data_address(edge_mask))
          links[1] = sys.address_value(sys.data_address(edge_to))
          links[2] = sys.address_value(sys.data_address(source_nodes))
          links[3] = sys.address_value(sys.data_address(edge_features))
          links[4] = sys.address_value(sys.data_address(edge_from))
          links[5] = sys.address_value(sys.data_address(target_nodes))
          context[0] = sys.address_value(sys.data_address(messages))
          context[1] = sys.address_value(sys.data_address(links))
          for trial in 0..2
            context[16] = 0
            rejected = sys.call_sysv64(sys.offset(executable,messages_entry),context_address)
            ok = ok and context[16] == 3
            for index in 0..8
              ok = ok and messages[index] == 1065353216
            edge_from[2] = 1
            edge_to[2] = 2
        memory_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 151
            memory_entry = cells.symbols()[index * 128 + 101]
        ok = ok and memory_entry >= 0
        if ok
          words = VerifyContext()
          words[2] = 99
          context[0] = sys.address_value(sys.data_address(words))
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,memory_entry),context_address)
          ok = ok and result == 73 and context[16] == 0 and words[0] == 0x789abcde345612ff and words[1] == -9223372036854775808 and words[2] == 99
        // Structural helpers use the very same compiled relation operations.
        add_entry = -1
        remove_entry = -1
        neighbors_entry = -1
        for index in 0..state.count
          if cells.symbols()[index * 128 + 19] == 132
            add_entry = cells.symbols()[index * 128 + 101]
          if cells.symbols()[index * 128 + 19] == 133
            remove_entry = cells.symbols()[index * 128 + 101]
          if cells.symbols()[index * 128 + 19] == 134
            neighbors_entry = cells.symbols()[index * 128 + 101]
        ok = ok and add_entry >= 0 and remove_entry >= 0 and neighbors_entry >= 0
        if ok
          from = VerifyContext()
          from[0] = 0
          from[1] = 2
          from[2] = 1
          from[3] = 999
          to = VerifyContext()
          to[0] = 1
          to[1] = 0
          to[2] = 1
          to[3] = -999
          mask = VerifyContext()
          mask[0] = 1
          mask[1] = 1
          mask[2] = 1
          mask[3] = 0
          sources = VerifyContext()
          targets = VerifyContext()
          features = VerifyContext()
          links = VerifyContext()
          output = VerifyContext()
          output[0] = 77
          output[1] = 77
          output[2] = 77
          output[3] = 77
          links[0] = sys.address_value(sys.data_address(mask))
          links[1] = sys.address_value(sys.data_address(to))
          links[2] = sys.address_value(sys.data_address(sources))
          links[3] = sys.address_value(sys.data_address(features))
          links[4] = sys.address_value(sys.data_address(from))
          links[5] = sys.address_value(sys.data_address(targets))
          context[0] = sys.address_value(sys.data_address(links))
          context[1] = 3
          context[2] = 0
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,add_entry),context_address)
          ok = context[16] == 3 and from[3] == 999 and to[3] == -999 and mask[3] == 0
          from[2] = 999
          context[1] = 2
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,remove_entry),context_address)
          ok = ok and result == 1 and context[16] == 0 and mask[2] == 0 and from[2] == 999
          context[1] = 1
          context[2] = 0
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,add_entry),context_address)
          ok = ok and result == 2 and context[16] == 0 and from[2] == 1 and to[2] == 0 and mask[2] == 1
          context[0] = sys.address_value(sys.data_address(output))
          context[1] = sys.address_value(sys.data_address(links))
          context[2] = 2
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,neighbors_entry),context_address)
          ok = ok and context[16] == 3 and output[0] == 77 and output[1] == 77 and output[2] == 77 and output[3] == 77
          context[2] = 0
          context[16] = 0
          result = sys.call_sysv64(sys.offset(executable,neighbors_entry),context_address)
          ok = ok and result == 2 and context[16] == 0 and output[0] == 2 and output[1] == 1 and output[2] == 77 and output[3] == 77
        sys.out8(233,ok ? 61 : 70)
  sys.out8(233,ok ? 83 : 70)
  sys.out8(233,33)
  sys.halt()
space CompilerSourceCheck @100001:
  kernel @100000 = verify_boot():
    space State @100002:
      input @100003 = verify_input()
      parameters @100004 = verify_parameters()
      nodes @100005 = verify_nodes()
      output @100006 = verify_output()
      body @100007 = verify_body()
      symbols @100008 = verify_symbols()
      fixups @100009 = verify_fixups()
      relocations @100010 = verify_relocations()
      execution @100011 = verify_execution()
"#);
    let program = crate::frontend::parse_seed(&fixture).unwrap();
    let directory = native::work().join("compiler-typed-self-host");
    let image = crate::uefi::compile(&program).unwrap();
    crate::uefi::write_esp(&image, &directory).unwrap();
    let tensor_source = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}\nverify @120()->i64 = run() == 41 and relation_run() == 53 and plastic_run() == 61 and indexed_run() == 67 and memory_run() == 73 and path_run() == 19 and loan_run() == 108 ? 41 : 0\n", include_str!("../../../language/examples/bootstrap-tensors"),include_str!("../../../language/examples/bootstrap-relations"),include_str!("../../../language/examples/bootstrap-plasticity"),include_str!("../../../language/examples/bootstrap-indexed"),include_str!("../../../language/examples/bootstrap-memory"),include_str!("../../../language/examples/bootstrap-paths"),include_str!("../../../language/examples/bootstrap-borrowed"));
    let mut asset = Vec::new();
    for word in [
        0x33504d434152494du64,
        code.len() as u64,
        source.len() as u64,
        exports["module_index"] as u64,
        exports["module_prepare"] as u64,
        exports["module_emit_next"] as u64,
        exports["module_link"] as u64,
        exports["compile_word_function"] as u64,
        exports["compile_constant_function"] as u64,
        tensor_source.len() as u64,
    ] {
        asset.extend(word.to_le_bytes());
    }
    asset.extend_from_slice(code);
    asset.extend_from_slice(source.as_bytes());
    asset.extend_from_slice(tensor_source.as_bytes());
    std::fs::write(directory.join("source.mira"), asset).unwrap();
    // The compiler's persistent byte-per-word work buffers need more than the
    // default tiny runtime test machine. User execution arenas stay unchanged.
    let result = native::qemu_timed_memory(&directory, true, "compiler-typed-self-host", 120, 256);
    assert!(
        result.ends_with("T=S!"),
        "typed compiler self-regeneration failed: {result}"
    );
}
