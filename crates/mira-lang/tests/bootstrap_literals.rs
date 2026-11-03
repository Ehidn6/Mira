#[allow(dead_code)]
mod common;
use mira_lang::{runtime, syntax};

#[test]
fn frontend_uses_mira_integer_decoding_at_signed_boundaries() {
    for (text, expected) in [
        ("0", 0),
        ("-0", 0),
        ("0000123", 123),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("0x7fffffffffffffff", i64::MAX),
        ("-0x8000000000000000", i64::MIN),
        ("0xAbCdEf", 11259375),
        ("0b101010", 42),
        (
            "-0b1000000000000000000000000000000000000000000000000000000000000000",
            i64::MIN,
        ),
    ] {
        let program = syntax::parse(&format!(
            "run()->i64 = {text}\nspace Main:\n  result = run()\n"
        ))
        .unwrap();
        assert_eq!(
            runtime::execute(&program, 1).unwrap().terminals[0].1,
            expected,
            "{text}"
        );
    }
    let source = format!(
        "run()->i64 = {}1\nspace Main:\n  result = run()\n",
        "0".repeat(127)
    );
    assert_eq!(
        runtime::execute(&syntax::parse(&source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        1
    );
    for id in [9223372036854775808u64, u64::MAX - 1] {
        let program = syntax::parse(&format!("space Main @{id}:\n")).unwrap();
        assert_eq!(program.root.id, id);
        assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    }
}

#[test]
fn frontend_rejects_invalid_or_unrepresentable_literals_without_fallback() {
    for text in [
        "9223372036854775808",
        "-9223372036854775809",
        "0x8000000000000000",
        "-0x8000000000000001",
        "0xGG",
        "0x",
        "0b2",
    ] {
        assert!(
            syntax::parse(&format!(
                "run()->i64 = {text}\nspace Main:\n  result = run()\n"
            ))
            .is_err(),
            "{text}"
        );
    }
    let source = format!(
        "run()->i64 = {}\nspace Main:\n  result = run()\n",
        "0".repeat(129)
    );
    assert!(syntax::parse(&source).unwrap_err().contains("128-byte"));
    let huge = "type Huge = [i64,18446744073709551615]\nspace Main:\n";
    assert!(syntax::parse(huge).unwrap_err().contains("array length"));
}

#[test]
#[ignore = "one native UEFI check of the same Mira integer decoder"]
fn native_integer_decoder_runs_without_rust_parsing_services() {
    let definitions = mira_lang::compiler_source().unwrap();
    let source = format!(
        "{definitions}\nboot()->unit =\n  text = LiteralBytes(\"9223372036854775808\")\n  result = LiteralValue()\n  status = parse_integer(text,19,true,result)\n  ok = status == 0 and result[0] == -9223372036854775808\n  status = parse_integer(text,19,false,result)\n  ok = ok and status == 2 and result[0] == -9223372036854775808\n  binary = LiteralBytes(\"0b101010\")\n  status = parse_integer(binary,8,false,result)\n  ok = ok and status == 0 and result[0] == 42\n  invalid = LiteralBytes(\"0b2\")\n  status = parse_integer(invalid,3,false,result)\n  ok = ok and status == 1 and result[0] == 42\n  if ok\n    sys.out8(233,76)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace NativeLiteralCompiler:\n  kernel = boot()\n"
    );
    let source = source.replace("  if ok\n", "  largest = LiteralBytes(\"18446744073709551615\")\n  status = parse_natural(largest,20,result)\n  ok = ok and status == 0 and result[0] == -1\n  excessive = LiteralBytes(\"18446744073709551616\")\n  status = parse_natural(excessive,20,result)\n  ok = ok and status == 2 and result[0] == -1\n  if ok\n");
    let program = syntax::parse(&source).unwrap();
    let esp = common::work().join("native-literal-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "native-literal").ends_with("L!"));
}

#[test]
fn frontend_uses_mira_float_decoding_for_expressions_and_coordinates() {
    for text in ["-0.0", "0.1", "1e-45", "3.4028234663852886e38", "1.000000059604644775390625"] {
        let program = syntax::parse(&format!("run()->i64 = math.bits({text})\nspace Main:\n  result = run()\n")).unwrap();
        let expected = text.parse::<f32>().unwrap().to_bits() as i64;
        assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, expected, "{text}");
    }
    let program = syntax::parse("space Main at (0.1,-1e-45,2.0):\n").unwrap();
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    for text in ["1e999", "1e+", "1.2.3"] {
        assert!(syntax::parse(&format!("run()->f32 = {text}\nspace Main:\n  result = run()\n")).is_err(), "{text}");
    }
    let long = format!("{}1.0", "0".repeat(128));
    assert!(syntax::parse(&format!("run()->f32 = {long}\nspace Main:\n")).unwrap_err().contains("128-byte"));
}
