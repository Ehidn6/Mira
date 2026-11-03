use mira_lang::syntax;

const BASIC: &str = "run()->i64 = sys.call_sysv64(sys.addr(4194304),7,true,sys.addr(8))\nspace Main:\n  result = run()\n";

#[test]
fn scalar_foreign_results_are_typed_and_keep_effect_boundaries() {
    for abi in ["sysv64", "win64"] {
        for (suffix, result) in [
            ("bool", "bool"),
            ("void", "unit"),
            ("i32", "i64"),
            ("u32", "i64"),
        ] {
            let source = format!(
                "run()->{result} = sys.call_{abi}_{suffix}(sys.addr(4194304))\nboot()->unit =\n  value = run()\nspace Main:\n  kernel = boot()\n"
            );
            let program = syntax::parse(&source).unwrap();
            assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
            assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
            assert!(
                mira_lang::runtime::execute(&program, 1)
                    .unwrap_err()
                    .contains("native machine-code")
            );
            let scoped = source.replace("value = run()", "value = sys.scratch(run,4096)");
            assert!(mira_lang::kernel::compile(&syntax::parse(&scoped).unwrap()).is_err());
        }
    }
    for expression in [
        "sys.call_sysv64_bool(sys.addr(1))",
        "sys.call_win64_void(sys.addr(1))",
        "sys.call_win64_u64(sys.addr(1))",
    ] {
        assert!(
            syntax::parse(&format!(
                "run()->i64 = {expression}\nspace Main:\n  result = run()\n"
            ))
            .is_err()
        );
    }
}

#[test]
fn aligned_aggregates_initialize_and_preserve_the_existing_layout() {
    let source = include_str!("../../../language/examples/aligned-data");
    let program = syntax::parse(source).unwrap();
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        1
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let native = source.replace("ok = true", "ok = (sys.address_value(sys.data_address(small)) & 63) == 0 and (sys.address_value(sys.data_address(page)) & 4095) == 0")
            .replace("sys.read_only(work,8192)","work()");
        let image = mira_lang::native::compile(&syntax::parse(&native).unwrap()).unwrap();
        let output = execute("aligned-addresses", &image);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
    }
    let boot = source.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
}

#[test]
fn aligned_aggregates_charge_padding_and_reclaim_scoped_memory() {
    let source = "type One = [i64,1]\nwork()->i64 =\n  value = One.aligned(64)\n  value[0] = 7\n  value[0]\nrun()->i64 =\n  total = 0\n  for i in 0..100\n    total += sys.scratch(work,64)\n  total\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    let neural = source
        .replace(
            "run()->i64 =",
            "train()->unit =\n  value = work()\nrun()->i64 =",
        )
        .replace(
            "total = 0",
            "ok = sys.neural_try(train,4096,64)\n  total = ok ? 0 : 1",
        );
    assert_eq!(
        mira_lang::runtime::execute(&syntax::parse(&neural).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        700
    );
    let boot = neural.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()",
    );
    assert!(
        !mira_lang::kernel::compile(&syntax::parse(&boot).unwrap())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        700
    );
    let failure = syntax::parse(&source.replace("scratch(work,64)", "scratch(work,63)")).unwrap();
    assert!(
        mira_lang::runtime::execute(&failure, 1)
            .unwrap_err()
            .contains("arena")
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let output = execute(
            "aligned-budget",
            &mira_lang::native::compile(&program).unwrap(),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"700\n");
        assert!(
            !execute(
                "aligned-budget-fail",
                &mira_lang::native::compile(&failure).unwrap()
            )
            .status
            .success()
        );
    }
}

#[test]
fn aligned_aggregates_validate_alignment_and_reference_initialization() {
    for alignment in [0, 4, 24, 8192, -1] {
        let source = format!(
            "type One = [i64,1]\nrun()->i64 =\n  value = One.aligned({alignment})\n  value[0]\nspace Main:\n  result = run()\n"
        );
        let program = syntax::parse(&source).unwrap();
        assert!(
            mira_lang::runtime::execute(&program, 1)
                .unwrap_err()
                .contains("alignment")
        );
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            assert_eq!(
                execute(
                    &format!("bad-alignment-{alignment}"),
                    &mira_lang::native::compile(&program).unwrap()
                )
                .status
                .code(),
                Some(74)
            );
        }
    }
    let source = "type Values = [i64,1]\nstruct Holder:\n  child: Values\nrun()->Holder = Holder.aligned(64)\nspace Main:\n  result = run()\n";
    assert!(
        syntax::parse(source)
            .unwrap_err()
            .contains("aggregate references")
    );
    let escaping = "type One = [i64,1]\nwork()->One = One.aligned(64)\nrun()->One = sys.scratch(work,64)\nspace Main:\n  result = run()\n";
    assert!(syntax::parse(escaping).is_err());
}

#[test]
fn integer_math_matches_native_machine_code_on_word_boundaries() {
    let mut source = String::from("run()->i64 =\n  ok = true\n");
    for value in [0i64, 1, -1, i64::MIN, i64::MAX, 0x0102030405060708] {
        for (operation, expected) in [
            ("count_ones", value.count_ones() as i64),
            ("leading_zeros", value.leading_zeros() as i64),
            ("trailing_zeros", value.trailing_zeros() as i64),
            ("byte_swap", value.swap_bytes()),
        ] {
            source.push_str(&format!(
                "  ok = ok and math.{operation}({value}) == {expected}\n"
            ));
        }
        for count in [0i64, 1, 63, 64, 65, -1] {
            for (operation, expected) in [
                ("rotate_left", value.rotate_left(count as u32)),
                ("rotate_right", value.rotate_right(count as u32)),
            ] {
                source.push_str(&format!(
                    "  ok = ok and math.{operation}({value},{count}) == {expected}\n"
                ));
            }
        }
        for other in [1i64, -1, i64::MAX] {
            for (operation, expected) in [
                ("wrapping_add", value.wrapping_add(other)),
                ("wrapping_sub", value.wrapping_sub(other)),
                ("wrapping_mul", value.wrapping_mul(other)),
            ] {
                source.push_str(&format!(
                    "  ok = ok and math.{operation}({value},{other}) == {expected}\n"
                ));
            }
        }
    }
    source.push_str("  ok ? 1 : 0\nspace Main:\n  result = run()\n");
    let program = syntax::parse(&source).unwrap();
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        1
    );
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let output = execute(
            "integer-math",
            &mira_lang::native::compile(&program).unwrap(),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
    }
}

#[test]
fn integer_math_is_pure_in_macros_read_only_and_neural_regions() {
    let source = "macro build()->unit =\n  value = math.wrapping_add(9223372036854775807,1)\n  count = math.leading_zeros(0) + math.count_ones(value)\n  meta.set_parameter(reflect.cell_at(reflect.root(),0),count)\nspace Main:\n  result = 0\nexpand build()\n";
    assert_eq!(
        mira_lang::runtime::execute(&syntax::parse(source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        65
    );
    let source = "struct Counter:\n  count: i64\nnext(counter:Counter)->i64 =\n  counter.count += 1\n  counter.count\ncheck()->i64 =\n  counter = Counter(0)\n  value = math.wrapping_add(next(counter),next(counter))\n  ok = value == 3 and counter.count == 2\n  ok = ok and math.rotate_left(1,-1) == -9223372036854775808\n  ok ? 1 : 0\nwork()->unit =\n  value = math.wrapping_mul(-9223372036854775808,-1)\nrun()->i64 =\n  ok = sys.neural_try(work,4096,4096)\n  ok ? sys.read_only(check,4096) : 0\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        1
    );
    let source = source.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&source).unwrap();
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
}

#[test]
fn integer_math_keeps_default_overflow_checks_and_rejects_other_types() {
    let source = "run()->i64 = 9223372036854775807 + 1\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert!(mira_lang::runtime::execute(&program, 1).is_err());
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        assert_eq!(
            execute(
                "checked-overflow",
                &mira_lang::native::compile(&program).unwrap()
            )
            .status
            .code(),
            Some(70)
        );
    }
    for operation in [
        "math.wrapping_add(1.0,2.0)",
        "math.count_ones(true)",
        "math.rotate_left(1)",
    ] {
        assert!(
            syntax::parse(&format!(
                "run()->i64 = {operation}\nspace Main:\n  result = run()\n"
            ))
            .is_err()
        );
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn narrow_atomics_preserve_neighbors_and_wrap_at_the_selected_width() {
    for width in [8, 16, 32] {
        let max = (1u64 << width) - 1;
        let source = format!(
            "type Data = [i64,2]\nrun()->i64 =\n  data = Data(-1,777)\n  p = sys.data_address(data)\n  sys.atomic_store{width}_release(p,7)\n  a = sys.atomic_compare_exchange{width}_acq_rel(p,7,9)\n  b = sys.atomic_compare_exchange{width}_relaxed(p,7,99)\n  c = sys.atomic_exchange{width}(p,{max})\n  d = sys.atomic_fetch_add{width}(p,1)\n  ok = a == 7 and b == 9 and c == 9 and d == {max}\n  ok = ok and sys.atomic_load{width}_acquire(p) == 0 and data[1] == 777\n  ok = ok and (data[0] >> {width}) == (-1 >> {width})\n  ok ? 1 : 0\nspace Main:\n  result = run()\n"
        );
        let program = syntax::parse(&source).unwrap();
        let output = execute(
            &format!("atomic-{width}"),
            &mira_lang::native::compile(&program).unwrap(),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
        let boot = source.replace(
            "space Main:\n  result = run()",
            "boot()->unit =\n  value = run()\nspace Main:\n  kernel = boot()",
        );
        let program = syntax::parse(&boot).unwrap();
        assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
        assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
    }
}

#[test]
fn narrow_atomics_reject_invalid_orders_and_remain_raw_effects() {
    for width in [8, 16, 32] {
        let source = format!(
            "work()->unit = sys.atomic_store{width}(sys.addr(0),7)\nboot()->unit =\n  ok = sys.neural_try(work,4096,4096)\nspace Main:\n  kernel = boot()\n"
        );
        let program = syntax::parse(&source).unwrap();
        assert!(
            mira_lang::kernel::compile(&program)
                .unwrap_err()
                .contains("forbidden")
        );
        let source = format!(
            "run()->i64 = sys.atomic_load{width}_release(sys.addr(0))\nspace Main:\n  result = run()\n"
        );
        assert!(syntax::parse(&source).is_err());
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn narrow_atomics_check_alignment_and_unsigned_operands() {
    for (label, op) in [
        ("alignment16", "sys.atomic_store16(sys.offset(p,1),1)"),
        ("alignment32", "sys.atomic_load32(sys.offset(p,2))"),
        ("negative8", "sys.atomic_store8(p,-1)"),
        ("range32", "sys.atomic_store32(p,4294967296)"),
        ("desired16", "sys.atomic_compare_exchange16(p,0,65536)"),
    ] {
        let source = format!(
            "type Data = [i64,2]\nrun()->i64 =\n  data = Data()\n  p = sys.data_address(data)\n  result = {op}\n  0\nspace Main:\n  result = run()\n"
        );
        let program = syntax::parse(&source).unwrap();
        assert_eq!(
            execute(label, &mira_lang::native::compile(&program).unwrap())
                .status
                .code(),
            Some(74)
        );
    }
}

const ADDRESS_CHECK: &str = r#"struct Counter:
  calls: i64
touch(counter:Counter,value:addr)->addr =
  counter.calls += 1
  value
inspect()->i64 =
  low = sys.addr(9223372036854775807)
  high = sys.addr(-9223372036854775808)
  top = sys.addr(-1)
  counter = Counter(0)
  ok = low < high and low <= high and high > low and high >= low
  ok = ok and high < top and high <= high and top >= top and not (top < high)
  ok = ok and not (top <= low) and not (low > high) and not (low >= high)
  ok = ok and sys.address_distance(low,high) == 1 and sys.address_distance(high,top) == 9223372036854775807
  ok = ok and sys.address_distance(high,high) == 0
  ok = ok and sys.offset(top,-2) == sys.addr(-3) and sys.align_up(high,4096) == high
  ok = ok and touch(counter,low) < touch(counter,high) and counter.calls == 2
  ok ? 1 : 0
run()->i64 = sys.read_only(inspect,4096)
space Main:
  result = run()
"#;

#[test]
fn address_semantics_unsigned_order_and_read_only_execution() {
    let program = syntax::parse(ADDRESS_CHECK).unwrap();
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        1
    );
    assert!(syntax::parse(&ADDRESS_CHECK.replace("low < high", "low < 3")).is_err());
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let output = execute(
            "address-order",
            &mira_lang::native::compile(&program).unwrap(),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
    }
    let boot = ADDRESS_CHECK.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
}

#[test]
fn address_semantics_distance_rejects_underflow_and_unrepresentable_lengths() {
    for (label, expression) in [
        ("reverse", "sys.address_distance(sys.addr(-1),sys.addr(1))"),
        ("too-far", "sys.address_distance(sys.addr(0),sys.addr(-1))"),
    ] {
        let source = format!("run()->i64 = {expression}\nspace Main:\n  result = run()\n");
        let program = syntax::parse(&source).unwrap();
        assert!(
            mira_lang::runtime::execute(&program, 1)
                .unwrap_err()
                .contains("address distance")
        );
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            assert_eq!(
                execute(label, &mira_lang::native::compile(&program).unwrap())
                    .status
                    .code(),
                Some(70)
            );
        }
    }
}

#[test]
fn address_semantics_pure_calculation_is_available_in_macros_and_neural_regions() {
    let source = "macro build()->unit =\n  start = sys.addr(9223372036854775807)\n  finish = sys.addr(-9223372036854775808)\n  if start < finish\n    meta.set_parameter(reflect.cell_at(reflect.root(),0),sys.address_distance(start,finish))\nspace Main:\n  result = 0\nexpand build()\n";
    assert_eq!(
        mira_lang::runtime::execute(&syntax::parse(source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        1
    );
    let source = "work()->unit =\n  distance = sys.address_distance(sys.addr(9223372036854775807),sys.addr(-9223372036854775808))\n  if distance != 1\n    invalid = 1 / 0\nrun()->i64 = sys.neural_try(work,4096,4096) ? 1 : 0\nspace Main:\n  result = run()\n";
    assert_eq!(
        mira_lang::runtime::execute(&syntax::parse(source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        1
    );
    let source = source.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&source).unwrap();
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    let bad = source.replace("distance = sys.address_distance(sys.addr(9223372036854775807),sys.addr(-9223372036854775808))", "distance = sys.load64(sys.addr(0))");
    assert!(
        mira_lang::kernel::compile(&syntax::parse(&bad).unwrap())
            .unwrap_err()
            .contains("forbidden")
    );
}

#[test]
fn native_boundaries_are_typed_and_cannot_escape_checked_scopes() {
    let program = syntax::parse(BASIC).unwrap();
    assert!(
        mira_lang::runtime::execute(&program, 1)
            .unwrap_err()
            .contains("native machine-code")
    );
    for source in [
        BASIC.replace("sys.addr(4194304)", "4194304"),
        BASIC
            .replace("sys.call_sysv64", "sys.call_mira64")
            .replace(",7,true,sys.addr(8)", ",1.0"),
        BASIC.replace(
            ",7,true,sys.addr(8)",
            &format!(",{}", vec!["1"; 17].join(",")),
        ),
        "run()->addr = sys.data_address(3)\nspace Main:\n  result = run()\n".into(),
    ] {
        assert!(syntax::parse(&source).is_err(), "{source}");
    }
    for scope in ["scratch", "read_only"] {
        let source = BASIC
            .replace(
                "space Main:",
                &format!("entry()->i64 = sys.{scope}(run,4096)\nspace Main:"),
            )
            .replace("result = run()", "result = entry()");
        let program = syntax::parse(&source).unwrap();
        assert!(mira_lang::runtime::execute(&program, 1).is_err());
        assert!(
            mira_lang::native::compile(&program)
                .unwrap_err()
                .contains("scratch entries")
        );
    }
    let address_escape = "type Buffer = [i64,2]\nget()->addr = sys.data_address(Buffer())\nrun()->addr = sys.scratch(get,4096)\nspace Main:\n  result = run()\n";
    assert!(
        mira_lang::native::compile(&syntax::parse(address_escape).unwrap())
            .unwrap_err()
            .contains("temporary addresses")
    );
    let neural = "work()->unit =\n  value = sys.call_win64(sys.addr(4194304))\nboot()->unit =\n  ok = sys.neural_try(work,4096,4096)\nspace Main:\n  kernel = boot()\n";
    assert!(
        mira_lang::kernel::compile(&syntax::parse(neural).unwrap())
            .unwrap_err()
            .contains("forbidden")
    );
    let readonly_bulk = "work()->unit = sys.memory_fill(sys.addr(0),0,0)\nrun()->unit = sys.read_only(work,4096)\nspace Main:\n  result = run()\n";
    assert!(mira_lang::native::compile(&syntax::parse(readonly_bulk).unwrap()).is_err());
    let boot = BASIC.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  value = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
    for name in ["sys.call_sysv64_f32", "sys.call_win64_f32"] {
        let source = format!(
            "run()->f32 = {name}(sys.addr(4194304),7,1.5,true)\nboot()->unit =\n  result = run()\nspace Main:\n  kernel = boot()\n"
        );
        let program = syntax::parse(&source).unwrap();
        assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
        assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
        let scoped = source.replace("result = run()", "result = sys.scratch(run,4096)");
        assert!(mira_lang::kernel::compile(&syntax::parse(&scoped).unwrap()).is_err());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn execute(label: &str, bytes: &[u8]) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("native-abi-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(label);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new(&path).output().unwrap();
    std::fs::remove_file(path).unwrap();
    output
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn native_byte_operations_preserve_overlap_and_check_bounds() {
    let source = "type Words = [i64,4]\nrun()->i64 =\n  data = Words(1,2,3,4)\n  address = sys.data_address(data)\n  sys.memory_copy(sys.offset(address,8),address,24)\n  backward = data[0] == 1 and data[1] == 1 and data[2] == 2 and data[3] == 3\n  sys.memory_copy(address,sys.offset(address,8),24)\n  forward = data[0] == 1 and data[1] == 2 and data[2] == 3 and data[3] == 3\n  sys.memory_fill(sys.offset(address,24),0,8)\n  sys.memory_copy(sys.addr(0),sys.addr(0),0)\n  sys.memory_fill(sys.addr(0),0,0)\n  backward and forward and data[3] == 0 ? 1 : 0\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    let output = execute("bulk", &mira_lang::native::compile(&program).unwrap());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"1\n");
    for (label, expression, status) in [
        ("negative", "sys.memory_fill(sys.addr(0),0,-1)", 76),
        ("fill-byte", "sys.memory_fill(sys.addr(0),256,0)", 74),
        ("wrap", "sys.memory_copy(sys.addr(-1),sys.addr(1),2)", 76),
        ("null", "sys.call_win64(sys.addr(0))", 74),
    ] {
        let source =
            format!("run()->i64 =\n  value = {expression}\n  0\nspace Main:\n  result = run()\n");
        let image = mira_lang::native::compile(&syntax::parse(&source).unwrap()).unwrap();
        assert_eq!(execute(label, &image).status.code(), Some(status));
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn mira_procedure_symbols_support_indirect_dispatch_without_numeric_addresses() {
    let parameters = (0..16)
        .map(|i| format!("a{i}:i64"))
        .collect::<Vec<_>>()
        .join(",");
    let sum = (0..16)
        .map(|i| format!("a{i}*{}", i + 1))
        .collect::<Vec<_>>()
        .join("+");
    let args = (1..=16)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        r#"type Table = [addr,2]
weighted({parameters})->i64 = {sum}
plus(x:i64)->i64 = x+1
noop()->unit = ()
choose(flag:bool,p:addr)->addr = flag ? p : sys.addr(0)
read()->i64 = cells.source()
run()->i64 =
  local(x:i64)->i64 = x*3
  table = Table(sys.procedure_address(plus),sys.procedure_address(local))
  left = sys.call_mira64(table[0],4)
  right = sys.call_mira64(table[1],left)
  weighted = sys.call_mira64(sys.procedure_address(weighted),{args})
  empty = sys.call_mira64(sys.procedure_address(noop))
  pointer = sys.call_mira64(sys.procedure_address(choose),true,sys.addr(17))
  child = sys.call_mira64(sys.procedure_address(read))
  weighted == 1496 and right == 15 and empty == 0 and pointer == 17 and child == 7 ? 1 : 0
space Main:
  result = run():
    space State:
      source = 7
"#
    );
    let program = syntax::parse(&source).unwrap();
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    let image = mira_lang::native::compile(&program).unwrap();
    let output = execute("mira-procedures", &image);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"1\n");
    let boot = source
        .replace(
            "space Main:",
            "boot()->unit =\n  value = run()\nspace Main:",
        )
        .replace("result = run():", "kernel = boot():");
    let kernel = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::kernel::compile(&kernel).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&kernel).unwrap().is_empty());
}

#[test]
fn procedure_addresses_require_runtime_word_signatures_and_checked_scope_boundaries() {
    for source in [
        "run()->addr = sys.procedure_address(missing)\nspace Main:\n  result = run()\n",
        "run()->addr =\n  x = 3\n  sys.procedure_address(x)\nspace Main:\n  result = run()\n",
        "float_value(x:f32)->f32 = x\nrun()->addr = sys.procedure_address(float_value)\nspace Main:\n  result = run()\n",
        "macro build()->i64 = 1\nrun()->addr = sys.procedure_address(build)\nspace Main:\n  result = run()\n",
    ] {
        assert!(syntax::parse(source).is_err(), "{source}");
    }
    let source = "value()->i64 = 7\nget()->addr = sys.procedure_address(value)\nrun()->addr = sys.scratch(get,4096)\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert!(
        mira_lang::native::compile(&program)
            .unwrap_err()
            .contains("scratch entries")
    );
    let source = "work()->unit =\n  value = sys.call_mira64(sys.addr(1))\nboot()->unit =\n  ok = sys.neural_try(work,4096,4096)\nspace Main:\n  kernel = boot()\n";
    assert!(
        mira_lang::kernel::compile(&syntax::parse(source).unwrap())
            .unwrap_err()
            .contains("forbidden")
    );
}

/// The C compiler is a test oracle only; Mira's production code generator does
/// not invoke it. The appended fixture is an RX function body, not a new loader.
#[test]
#[ignore = "independent ABI validation requires cc, objcopy and nm on Linux x86-64"]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn calls_agree_with_independently_compiled_sysv_and_win64_procedures() {
    use std::{collections::BTreeMap, fs, process::Command};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("native-abi-fixture-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let parameters = (0..16)
        .map(|i| format!("W a{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sum = (0..16)
        .map(|i| format!("a{i}*{}", i + 1))
        .collect::<Vec<_>>()
        .join("+");
    let mixed_float = |i: usize| i % 2 == 0 || i == 15;
    let mixed_parameters = (0..16)
        .map(|i| {
            format!(
                "{} a{i}",
                if mixed_float(i) { "float" } else { "long long" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mixed_sum = (0..16)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join("+");
    let mut result_fixtures = String::new();
    for (prefix, abi, pointer) in [
        ("sv", "", "%rdi"),
        ("win", "__attribute__((ms_abi))", "%rcx"),
    ] {
        result_fixtures.push_str(&format!(r#"
{abi} __attribute__((naked)) _Bool {prefix}_false(void) {{ __asm__("movabs $0x1122334400000000,%rax; ret"); }}
{abi} __attribute__((naked)) _Bool {prefix}_true(void) {{ __asm__("movabs $0x1122334400000001,%rax; ret"); }}
{abi} __attribute__((naked)) int {prefix}_signed(void) {{ __asm__("movabs $0x11223344fffffff9,%rax; ret"); }}
{abi} __attribute__((naked)) unsigned int {prefix}_unsigned(void) {{ __asm__("movabs $0x11223344fedcba98,%rax; ret"); }}
{abi} __attribute__((naked)) void {prefix}_void(W* p) {{ __asm__("movq $99,({pointer}); movabs $0x1122334455667788,%rax; ret"); }}
"#));
    }
    let source = format!(
        r#"
typedef unsigned long long W;
{result_fixtures}
float mix_sysv({mixed_parameters}) {{ return {mixed_sum}; }}
__attribute__((ms_abi)) float mix_win({mixed_parameters}) {{ return {mixed_sum}; }}
long long mix_word({mixed_parameters}) {{ return (long long)({mixed_sum}); }}
float fp9(float a,float b,float c,float d,float e,float f,float g,float h,float i) {{ return a-b*c-d*e-f*g-h*i; }}
float bad(float value) {{ return value/value; }}
W sysv16({parameters}) {{ W result={sum}; __asm__ volatile("xor %%r8d,%%r8d; xor %%r9d,%%r9d; xor %%r10d,%%r10d; xor %%r11d,%%r11d" ::: "r8","r9","r10","r11","cc"); return result; }}
__attribute__((ms_abi)) W win16({parameters}) {{ W result={sum}; __asm__ volatile("xor %%r8d,%%r8d; xor %%r9d,%%r9d; xor %%r10d,%%r10d; xor %%r11d,%%r11d" ::: "r8","r9","r10","r11","cc"); return result; }}
W sysv0(void) {{ return 37; }}
__attribute__((ms_abi)) W win0(void) {{ return 41; }}
W store(W* p, W value, W yes) {{ *p = yes ? value : 0; return *p; }}
__attribute__((naked)) W alignment(void) {{ __asm__("mov %rsp,%rax; and $15,%eax; ret"); }}
__attribute__((ms_abi,naked)) W shadow(void) {{ __asm__("movq $1,8(%rsp); movq $2,16(%rsp); movq $3,24(%rsp); movq $4,32(%rsp); mov %rsp,%rax; and $15,%eax; ret"); }}
"#
    );
    fs::write(dir.join("fixture.c"), source).unwrap();
    let cc = Command::new("cc")
        .current_dir(&dir)
        .args([
            "-c",
            "-O2",
            "-ffreestanding",
            "-fno-pie",
            "-fno-stack-protector",
            "-mno-red-zone",
            "-fno-asynchronous-unwind-tables",
            "-fno-unwind-tables",
            "fixture.c",
            "-o",
            "fixture.o",
        ])
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    assert!(
        Command::new("objcopy")
            .current_dir(&dir)
            .args([
                "-O",
                "binary",
                "--only-section=.text",
                "fixture.o",
                "fixture.bin"
            ])
            .status()
            .unwrap()
            .success()
    );
    let symbols = Command::new("nm")
        .current_dir(&dir)
        .args(["-n", "fixture.o"])
        .output()
        .unwrap();
    assert!(symbols.status.success());
    let offsets: BTreeMap<_, _> = String::from_utf8(symbols.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            (parts.len() == 3 && parts[1] == "T").then(|| {
                (
                    parts[2].to_owned(),
                    u64::from_str_radix(parts[0], 16).unwrap(),
                )
            })
        })
        .collect();
    const FIXTURE_OFFSET: usize = 0x100000;
    let address = |name: &str| 0x400000 + FIXTURE_OFFSET as u64 + offsets[name];
    let args = vec!["next(counter)"; 16].join(",");
    let source = format!(
        "struct Counter:\n  value: i64\nnext(c:Counter)->i64 =\n  c.value += 1\n  c.value\nrun()->i64 =\n  counter = Counter(0)\n  a = sys.call_sysv64(sys.addr({}),{args})\n  counter.value = 0\n  b = sys.call_win64(sys.addr({}),{args})\n  zero = sys.call_sysv64(sys.addr({})) + sys.call_win64(sys.addr({}))\n  pointer = sys.data_address(counter)\n  stored = sys.call_sysv64(sys.addr({}),pointer,99,true)\n  aligned = sys.call_sysv64(sys.addr({})) == 8\n  shadow = sys.call_win64(sys.addr({})) == 8\n  ok = a == 1496 and b == 1496 and zero == 78 and stored == 99 and counter.value == 99\n  ok and aligned and shadow ? 1 : 0\nspace Main:\n  result = run()\n",
        address("sysv16"),
        address("win16"),
        address("sysv0"),
        address("win0"),
        address("store"),
        address("alignment"),
        address("shadow")
    );
    let fixture = fs::read(dir.join("fixture.bin")).unwrap();
    let run_fixture = |label: &str, source: &str| {
        let program = syntax::parse(source).unwrap();
        let mut image = mira_lang::native::compile(&program).unwrap();
        assert!(image.len() <= FIXTURE_OFFSET);
        image.resize(FIXTURE_OFFSET, 0);
        image.extend(&fixture);
        let size = image.len() as u64;
        for at in [96, 104] {
            image[at..at + 8].copy_from_slice(&size.to_le_bytes());
        }
        execute(label, &image)
    };
    let output = run_fixture("foreign", &source);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"1\n", "{output:?}");
    let mixed_args = (0..16)
        .map(|i| {
            if mixed_float(i) {
                format!("{}.0", i + 1)
            } else {
                format!("{}", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "run()->i64 =\n  a = sys.call_sysv64_f32(sys.addr({}),{mixed_args})\n  b = sys.call_win64_f32(sys.addr({}),{mixed_args})\n  c = sys.call_sysv64(sys.addr({}),{mixed_args})\n  d = sys.call_sysv64_f32(sys.addr({}),1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0,9.0)\n  a == 136.0 and b == 136.0 and c == 136 and d == -139.0 ? 1 : 0\nspace Main:\n  result = run()\n",
        address("mix_sysv"),
        address("mix_win"),
        address("mix_word"),
        address("fp9")
    );
    let output = run_fixture("foreign-f32", &source);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"1\n", "{output:?}");
    let source = format!(
        "run()->f32 = sys.call_sysv64_f32(sys.addr({}),0.0)\nspace Main:\n  result = run()\n",
        address("bad")
    );
    assert_eq!(run_fixture("foreign-nan", &source).status.code(), Some(74));
    for (prefix, abi) in [("sv", "sysv64"), ("win", "win64")] {
        let source = format!(
            "type Data = [i64,1]\nrun()->i64 =\n  data = Data()\n  no = sys.call_{abi}_bool(sys.addr({}))\n  yes = sys.call_{abi}_bool(sys.addr({}))\n  signed = sys.call_{abi}_i32(sys.addr({}))\n  unsigned = sys.call_{abi}_u32(sys.addr({}))\n  sys.call_{abi}_void(sys.addr({}),sys.data_address(data))\n  not no and yes and signed == -7 and unsigned == 4275878552 and data[0] == 99 ? 1 : 0\nspace Main:\n  result = run()\n",
            address(&format!("{prefix}_false")),
            address(&format!("{prefix}_true")),
            address(&format!("{prefix}_signed")),
            address(&format!("{prefix}_unsigned")),
            address(&format!("{prefix}_void"))
        );
        let output = run_fixture(&format!("foreign-results-{prefix}"), &source);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ordinary_arrays_use_the_existing_buffer_word_limit() {
    let limit = mira_core::data::MAX_WORDS;
    let source = format!("type Bytes = [i64,{limit}]\nrun()->i64 = Bytes.size_bytes()\nspace Main:\n  result = run()\n");
    let program = syntax::parse(&source).unwrap();
    assert_eq!(program.data_types()[0].len(), mira_core::data::MAX_WORDS);
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    assert!(
        syntax::parse(&source.replace(&limit.to_string(), &(limit + 1).to_string()))
            .unwrap_err()
            .contains(&format!("array length exceeds {limit} words"))
    );
}
