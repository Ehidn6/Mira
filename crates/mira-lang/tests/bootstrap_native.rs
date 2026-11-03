//! The normal native build must consume the shared Mira emitter, not merely
//! compile a demonstration compiler that is only called by a test harness.
use mira_lang::{compilation_cache, native, syntax};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};
fn run(source: &str, label: &str) -> Output {
    let program = syntax::parse(source).unwrap();
    let bytes = native::compile(&program).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "mira-bootstrap-native-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join("program");
    std::fs::write(&executable, bytes).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(&executable).output().unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    result
}
#[test]
fn ordinary_native_build_uses_mira_emission_and_reuses_its_results() {
    compilation_cache::clear();
    let source = include_str!("../../../language/examples/bootstrap");
    let first = run(source, "first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first.stdout, b"20\n");
    let before = compilation_cache::stats();
    assert!(
        before.scalar_code_misses >= 3,
        "normal backend did not use the Mira emitter"
    );
    let second = run(source, "second");
    assert_eq!(second.stdout, first.stdout);
    let after = compilation_cache::stats();
    assert_eq!(before.scalar_code_misses, after.scalar_code_misses);
    assert!(after.scalar_code_hits > before.scalar_code_hits);
}
#[test]
fn shared_emitter_keeps_native_traps_and_short_circuit_semantics() {
    for (label, source, status) in [
        (
            "overflow",
            "f(x:i64)->i64 = x + 1\nrun()->i64 = f(9223372036854775807)\n",
            70,
        ),
        ("division", "f(x:i64)->i64 = x / 0\nrun()->i64 = f(1)\n", 71),
        ("shift", "f(x:i64)->i64 = x << 64\nrun()->i64 = f(1)\n", 74),
    ] {
        let output = run(&format!("{source}space Main:\n  result = run()\n"), label);
        assert_eq!(
            output.status.code(),
            Some(status),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let source="guarded(flag:bool,value:i64)->bool = flag or value / 0 == 0\nrun()->bool = guarded(true,1)\nspace Main:\n  result = run()\n";
    let result = run(source, "short-circuit");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"1\n");
}

#[test]
fn ordinary_native_blocks_use_the_same_mira_emitter() {
    compilation_cache::clear();
    let result = run(
        include_str!("../../../language/examples/bootstrap-blocks"),
        "blocks",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"22\n");
    assert!(compilation_cache::stats().scalar_code_misses > 0);
    let unit=run("finish()->unit =\n  value:unit = ()\n  value\nrun()->i64 =\n  ignored = finish()\n  7\nspace Main:\n  result = run()\n","unit-local");
    assert!(
        unit.status.success(),
        "{}",
        String::from_utf8_lossy(&unit.stderr)
    );
    assert_eq!(unit.stdout, b"7\n");
    // The bounded Mira stage must not turn its local-table capacity into a
    // language restriction for programs compiled by the general active backend.
    let mut large = "run()->i64 =\n".to_string();
    for index in 0..52 {
        large.push_str(&format!("  local{index} = {index}\n"));
    }
    large.push_str("  local51\nspace Main:\n  result = run()\n");
    let large = run(&large, "many-locals");
    assert!(
        large.status.success(),
        "{}",
        String::from_utf8_lossy(&large.stderr)
    );
    assert_eq!(large.stdout, b"51\n");
}

#[test]
fn native_loops_use_mira_emission() {
    compilation_cache::clear();
    let output = run(
        include_str!("../../../language/examples/bootstrap-loops"),
        "loops",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"25\n");
    assert!(compilation_cache::stats().scalar_code_misses > 0);
    let nested = run(
        include_str!("../../../language/examples/bootstrap-nested-loops"),
        "nested-loops",
    );
    assert!(nested.status.success());
    assert_eq!(nested.stdout, b"145\n");
    let loop_only = "work()->unit =\n  while true\n    ()\nspace Main:\n  result = work()\n";
    assert_eq!(run(loop_only, "loop-budget").status.code(), Some(72));
}

#[test]
fn compiled_calls_loops_and_recursion_share_the_normal_linker() {
    compilation_cache::clear();
    let source = include_str!("../../../language/examples/bootstrap-calls");
    let output = run(source, "calls");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"88\n");
    let first = compilation_cache::stats();
    assert!(first.scalar_code_misses >= 4);
    assert_eq!(run(source, "calls-again").stdout, output.stdout);
    let second = compilation_cache::stats();
    assert_eq!(first.scalar_code_misses, second.scalar_code_misses);
    assert!(second.scalar_code_hits > first.scalar_code_hits);
    // Reusing source at different procedure IDs must rebind cached relocations.
    let shifted = format!("unrelated()->i64 = 9\n{source}");
    assert_eq!(run(&shifted, "calls-shifted").stdout, output.stdout);
}

#[test]
fn checked_data_lowering_preserves_place_evaluation_order_and_bounds() {
    compilation_cache::clear();
    let source = include_str!("../../../language/examples/bootstrap-data");
    let portable = mira_lang::runtime::execute(&syntax::parse(source).unwrap(), 1).unwrap();
    assert_eq!(portable.terminals[0].1, 1102);
    let output = run(source, "data");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"1102\n");
    assert!(compilation_cache::stats().scalar_code_misses >= 2);
    for index in [-1, 2] {
        let invalid = source.replace("values[next(counter)]", &format!("values[{index}]"));
        assert_eq!(run(&invalid, "data-bounds").status.code(), Some(76));
    }
}
#[test]
fn native_member_address_is_explicit_and_not_a_scratch_escape() {
    let source="type Buffer = [i64,2]\nrun()->i64 =\n  buffer = Buffer(4,7)\n  pointer = sys.address_of(buffer[1])\n  sys.address_distance(sys.data_address(buffer),pointer)\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert!(mira_lang::runtime::execute(&program, 1).is_err());
    let output = run(source, "member-address");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"8\n");
    let leak=syntax::parse("type Buffer = [i64,2]\nleak()->addr = sys.address_of(Buffer()[0])\nrun()->addr = sys.scratch(leak,1024)\n").unwrap();
    assert!(native::compile(&leak).is_err());
}
