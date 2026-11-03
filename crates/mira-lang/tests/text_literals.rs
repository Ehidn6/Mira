use mira_lang::{runtime, syntax};

#[test]
fn text_literals_are_utf8_word_arrays_with_zero_padding_and_source_roundtrip() {
    let source = r#"type Text = [i64,16]
run()->i64 =
  text = Text("é\"\n//()")
  text[0] == 195 and text[1] == 169 and text[2] == 34 and text[3] == 10 and text[8] == 0 and text[15] == 0 ? 1 : 0
space Main:
  result = run()
"#;
    let program = syntax::parse(source).unwrap();
    let formatted = syntax::format(&program);
    assert!(formatted.contains("Text(\""), "{formatted}");
    assert_eq!(syntax::parse(&formatted).unwrap(), program);
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 1);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/text-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let executable = dir.join(format!("text-{}", std::process::id()));
        std::fs::write(&executable, mira_lang::native::compile(&program).unwrap()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = std::process::Command::new(&executable).output();
        std::fs::remove_file(&executable).unwrap();
        let output = result.unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"1\n");
    }
}

#[test]
fn text_literals_validate_shape_and_do_not_become_implicit_function_arguments() {
    for source in [
        "type Text = [i64,2]\nrun()->Text = Text(\"éx\")\nspace Main:\n  result = run()\n",
        "type Text = [f32,4]\nrun()->Text = Text(\"abc\")\nspace Main:\n  result = run()\n",
        "run()->i64 = math.count_ones(\"abc\")\nspace Main:\n  result = run()\n",
        "type Text = [i64,4]\nrun()->Text = Text(\"unterminated)\n",
        "type Text = [i64,4]\nrun()->Text = Text(\"\\q\")\n",
    ] {
        assert!(syntax::parse(source).is_err(), "{source}");
    }
    let exact = "type Text = [i64,2]\nrun()->i64 =\n  value = Text(\"é\")\n  value[1]\nspace Main:\n  result = run()\n";
    assert_eq!(
        runtime::execute(&syntax::parse(exact).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        169
    );
}

#[test]
fn text_literal_storage_uses_existing_read_only_and_scratch_lifetimes() {
    let source = "type Text = [i64,8]\nwork()->i64 =\n  text = Text(\"hello\")\n  text[0]\nrun()->i64 = sys.read_only(work,64)\nspace Main:\n  result = run()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 104);
    let limited = syntax::parse(&source.replace("work,64", "work,63")).unwrap();
    assert!(runtime::execute(&limited, 1).is_err());
    let boot = source.replace(
        "space Main:\n  result = run()",
        "boot()->unit =\n  value = run()\nspace Main:\n  kernel = boot()",
    );
    let program = syntax::parse(&boot).unwrap();
    assert!(!mira_lang::uefi::compile(&program).unwrap().is_empty());
}
