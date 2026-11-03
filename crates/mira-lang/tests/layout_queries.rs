use mira_lang::{runtime, syntax};

const SOURCE: &str = "struct Vector:\n  x: f32\n  y: f32\nstruct Header:\n  flags: bool\n  vector: Vector\n  count: i64\nrun()->i64 =\n  count = 999\n  Header.offset_of(flags) + Header.offset_of(vector) + Header.offset_of(count) + Header.size_bytes() + Vector.size_bytes() + Header.alignment()\nspace Main:\n  result = run()\n";

#[test]
fn field_offsets_follow_the_word_abi_and_survive_source_roundtrip() {
    let program = syntax::parse(SOURCE).unwrap();
    // Nested aggregates occupy reference words. A same-named local is irrelevant.
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 72);
    let formatted = syntax::format(&program);
    assert!(formatted.contains("Header.offset_of(count)"), "{formatted}");
    let reparsed = syntax::parse(&formatted).unwrap();
    assert_eq!(program, reparsed);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("mira-layout-{}", std::process::id()));
        std::fs::write(&path, mira_lang::native::compile(&program).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = std::process::Command::new(&path).output();
        std::fs::remove_file(&path).unwrap();
        let output = result.unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"72\n");
    }
    // Ensure both boot profiles accept the same lowered source-level queries.
    let boot = SOURCE.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  value = run()\nspace Main:\n  kernel = boot()",
    );
    let kernel = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::kernel::compile(&kernel).unwrap().is_empty());
    assert!(!mira_lang::uefi::compile(&kernel).unwrap().is_empty());
}

#[test]
fn layout_metadata_is_available_in_structural_macros() {
    let source = "struct Header:\n  first: i64\n  count: i64\nmacro build()->unit =\n  root = reflect.root()\n  cell = reflect.cell_at(root,0)\n  meta.set_parameter(cell,Header.offset_of(count) + Header.size_bytes() + Header.alignment())\nspace Main:\n  template = 0\nexpand build()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 32);
}

#[test]
fn invalid_field_queries_report_source_errors() {
    for (source, expected) in [
        (SOURCE.replace("offset_of(count)", "offset_of(missing)"), "declared record field"),
        (SOURCE.replace("offset_of(count)", "offset_of(count + 1)"), "expected )"),
        (SOURCE.replace("offset_of(count)", "offset_of()"), "name"),
        ("type Words = [i64,4]\nrun()->i64 = Words.offset_of(count)\nspace Main:\n  result = run()\n".into(), "declared record field"),
    ] {
        let error = syntax::parse(&source).unwrap_err();
        assert!(error.contains(expected), "expected {expected}, got {error}");
    }
}
