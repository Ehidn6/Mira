#[allow(dead_code)]
mod common;
fn source() -> &'static str {
    mira_lang::compiler_source().unwrap()
}

#[test]
fn mira_compiler_component_preserves_the_x86_word_contract() {
    let program = mira_lang::syntax::parse(source()).unwrap();
    let selector = program
        .code_modules
        .iter()
        .find(|m| m.name == "word_encoding")
        .unwrap()
        .id;
    let expected: &[&[u8]] = &[
        &[0x48, 0x01, 0xc8],
        &[0x48, 0x29, 0xc8],
        &[0x48, 0x0f, 0xaf, 0xc1],
        &[0x48, 0x21, 0xc8],
        &[0x48, 0x09, 0xc8],
        &[0x48, 0x31, 0xc8],
        &[0x48, 0xd3, 0xe0],
        &[0x48, 0xd3, 0xe8],
        &[0x48, 0xd3, 0xc0],
        &[0x48, 0xd3, 0xc8],
        &[0x48, 0x0f, 0xc8],
    ];
    for (operation, bytes) in expected.iter().enumerate() {
        let encoded =
            mira_lang::behavior::execute(&program, selector, &[operation as i64]).unwrap();
        assert_eq!(encoded >> 32, bytes.len() as i64);
        assert_eq!(&(encoded as u32).to_le_bytes()[..bytes.len()], *bytes);
    }
    assert_eq!(
        mira_lang::behavior::execute(&program, selector, &[-1]).unwrap(),
        -1
    );
    assert_eq!(
        mira_lang::behavior::execute(&program, selector, &[11]).unwrap(),
        -1
    );
    let emit = program
        .code_modules
        .iter()
        .find(|m| m.name == "compile_word_function")
        .unwrap()
        .id;
    // Invalid operations and insufficient capacity must not dereference target.
    assert_eq!(
        mira_lang::behavior::execute(&program, emit, &[-1, 0, 32]).unwrap(),
        -1
    );
    assert_eq!(
        mira_lang::behavior::execute(&program, emit, &[2, 0, 10]).unwrap(),
        -1
    );
    let prefix = program
        .code_modules
        .iter()
        .find(|m| m.name == "immediate_prefix")
        .unwrap()
        .id;
    for register in 0..16 {
        let encoded = mira_lang::behavior::execute(&program, prefix, &[register]).unwrap();
        assert_eq!(encoded >> 32, 2);
        assert_eq!(encoded & 255, if register < 8 { 0x48 } else { 0x49 });
        assert_eq!((encoded >> 8) & 255, 0xb8 + register % 8);
    }
    assert_eq!(
        mira_lang::behavior::execute(&program, prefix, &[16]).unwrap(),
        -1
    );
    let constant = program
        .code_modules
        .iter()
        .find(|m| m.name == "compile_constant_function")
        .unwrap()
        .id;
    assert_eq!(
        mira_lang::behavior::execute(&program, constant, &[123, 0, 10]).unwrap(),
        -1
    );
    let frame = program
        .code_modules
        .iter()
        .find(|m| m.name == "frame_access_prefix")
        .unwrap()
        .id;
    for write in [0, 1] {
        for register in 0..16 {
            let encoded =
                mira_lang::behavior::execute(&program, frame, &[write, register]).unwrap();
            assert_eq!(encoded >> 32, 3);
            assert_eq!(
                (encoded as u32).to_le_bytes()[..3],
                [
                    if register < 8 { 0x48 } else { 0x4c },
                    if write == 1 { 0x89 } else { 0x8b },
                    0x85 + (register as u8 % 8) * 8,
                ]
            );
        }
    }
}

#[test]
#[ignore = "one UEFI run: native Mira compiler component emits and executes code"]
fn native_mira_compiler_emits_machine_code_inside_the_guest() {
    let definitions = source();
    let source = format!(
        "{definitions}\ntype Buffer = [i64,4]\nboot()->unit =\n  buffer = Buffer.aligned(16)\n  address = sys.data_address(buffer)\n  count = compile_word_function(0,address,32)\n  barrier = sys.cpuid_eax(0,0)\n  ok = count == 10 and sys.call_sysv64(address,7,9) == 16\n  count = compile_word_function(2,address,32)\n  barrier = sys.cpuid_eax(0,0)\n  ok = ok and count == 11 and sys.call_sysv64(address,-7,9) == -63\n  count = compile_word_function(7,address,32)\n  barrier = sys.cpuid_eax(0,0)\n  ok = ok and sys.call_sysv64(address,-1,1) == 9223372036854775807\n  rejected = compile_word_function(100,address,32)\n  ok = ok and rejected == -1 and sys.load8(address) == 72\n  if ok\n    sys.out8(233,67)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace NativeCompiler:\n  kernel = boot()\n"
    );
    let source = source.replace("  if ok\n", "  count = compile_constant_function(-9223372036854775808,address,32)\n  barrier = sys.cpuid_eax(0,0)\n  ok = ok and count == 11 and sys.call_sysv64(address) == -9223372036854775808\n  if ok\n");
    let program = mira_lang::syntax::parse(&source).unwrap();
    let esp = common::work().join("native-compiler-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "native-compiler").ends_with("C!"));
}
