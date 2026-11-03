#[allow(dead_code)]
mod common;
use mira_lang::{behavior, runtime, syntax};
fn source() -> &'static str {
    mira_lang::compiler_source().unwrap()
}

fn packed(text: &str) -> i64 {
    text.bytes()
        .enumerate()
        .fold(0, |n, (i, b)| n | (i64::from(b) << (i * 8)))
}

#[test]
fn mira_operator_component_recognizes_tokens_and_rejects_near_misses() {
    let program = syntax::parse(source()).unwrap();
    let entry = |name: &str| {
        program
            .code_modules
            .iter()
            .find(|m| m.name == name)
            .unwrap()
            .id
    };
    let binary = entry("operator_descriptor");
    for (tag, (text, precedence)) in [
        ("or", 1),
        ("and", 2),
        ("==", 3),
        ("!=", 3),
        ("<", 3),
        ("<=", 3),
        (">", 3),
        (">=", 3),
        ("|", 4),
        ("^", 5),
        ("&", 6),
        ("<<", 7),
        (">>", 7),
        ("+", 8),
        ("-", 8),
        ("*", 9),
        ("/", 9),
        ("%", 9),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            behavior::execute(&program, binary, &[packed(text), text.len() as i64]).unwrap(),
            precedence * 256 + tag as i64,
            "{text}"
        );
    }
    for (text, length) in [
        ("xor", 3),
        ("ori", 3),
        ("an", 2),
        ("", 0),
        ("and", 2),
        ("+", 2),
        ("=", 1),
        ("->", 2),
    ] {
        assert_eq!(
            behavior::execute(&program, binary, &[packed(text), length]).unwrap(),
            -1,
            "{text}"
        );
    }
    let punctuation = entry("punctuation_width");
    let unary = entry("unary_descriptor");
    for (tag, text) in ["-", "+", "~", "not"].iter().enumerate() {
        assert_eq!(
            behavior::execute(&program, unary, &[packed(text), text.len() as i64]).unwrap(),
            2560 + tag as i64
        );
    }
    for text in ["!", "no", "nor", "and", "*"] {
        assert_eq!(
            behavior::execute(&program, unary, &[packed(text), text.len() as i64]).unwrap(),
            -1
        );
    }
    for text in [
        "<<", ">>", "->", "..", "+=", "-=", "*=", "/=", "%=", "==", "!=", "<=", ">=",
    ] {
        let bytes = text.as_bytes();
        assert_eq!(
            behavior::execute(&program, punctuation, &[bytes[0] as i64, bytes[1] as i64]).unwrap(),
            2,
            "{text}"
        );
    }
    for (first, second, width) in [
        (b'+', -1, 1),
        (b'+', 43, 1),
        (b'!', -1, 0),
        (b'!', 33, 0),
        (b'(', 41, 1),
        (b';', 0, 0),
        (255, 0, 0),
    ] {
        assert_eq!(
            behavior::execute(&program, punctuation, &[first as i64, second]).unwrap(),
            width
        );
    }
    let assignment = entry("assignment_descriptor");
    for (tag, text) in ["=", "+=", "-=", "*=", "/=", "%="].iter().enumerate() {
        assert_eq!(
            behavior::execute(&program, assignment, &[packed(text), text.len() as i64]).unwrap(),
            tag as i64
        );
    }
    for text in ["==", "<=", ">=", "!=", "&=", "+", "==="] {
        assert_eq!(
            behavior::execute(&program, assignment, &[packed(text), text.len() as i64]).unwrap(),
            -1
        );
    }
}

// This same program is run by the host graph executor and as native x86-64.
// Its aggregate mutation, ranges and nested calls exercise token boundaries as
// well as the precedence table; trap expressions test lazy boolean evaluation.
fn frontend_program(native: bool) -> String {
    let mut source = String::from("type Words = [i64,3]\nrun()->bool =\n  ok = true\n");
    for (expression, expected) in [
        ("2 + 3 * 4", "14"),
        ("20 - 8 - 3", "9"),
        ("24 / 4 / 2", "3"),
        ("19 % 5", "4"),
        ("1 << 2 + 1", "8"),
        ("32 >> 1 + 2", "4"),
        ("1 | 2 ^ 3 & 6", "1"),
        ("1 + 2 << 2 & 15 ^ 3 | 16", "31"),
        ("1 << 3 << 1", "16"),
        ("~0 & 255", "255"),
        ("-2 * +3", "-6"),
        ("-(-9223372036854775807)", "9223372036854775807"),
        ("not not true", "true"),
        ("not false and false or true", "true"),
        ("false and 1 / 0 == 0", "false"),
        ("true or 1 / 0 == 0", "true"),
        ("3 <= 3 and 4 >= 4 and 1 < 2 and 3 > 2 and 1 != 2", "true"),
        ("2 == 2 ? 3 + 4 * 2 : 1 / 0", "11"),
        ("1.0 + 2.0 * 3.0", "7.0"),
    ] {
        source.push_str(&format!("  ok = ok and (({expression}) == {expected})\n"));
    }
    source.push_str("  value = 20\n  value += 4\n  value -= 6\n  value *= 3\n  value /= 2\n  value %= 5\n  values = Words()\n  for index in 0..3\n    values[index] = index + 1\n  values[1] *= 4\n  ok and value == 2 and values[0] + values[1] + values[2] == 12\n");
    if native {
        source.push_str("boot()->unit =\n  if run()\n    sys.out8(233,79)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace NativeOperators:\n  kernel = boot()\n");
    } else {
        source.push_str("space Operators:\n  result = run()\n");
    }
    source
}

#[test]
fn normal_frontend_preserves_precedence_associativity_and_diagnostics() {
    let program = syntax::parse(&frontend_program(false)).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 1);
    let formatted = syntax::format(&program);
    assert_eq!(syntax::parse(&formatted).unwrap(), program);
    for (expression, message) in [
        ("1 < 2 < 3", "comparison chains"),
        ("1 ! 2", "unsupported character"),
        ("1;2", "unsupported character"),
        ("1 >>", "expected"),
        ("(1 + 2]", "expected"),
    ] {
        let error = syntax::parse(&format!(
            "run()->i64 = {expression}\nspace Main:\n  result = run()\n"
        ))
        .unwrap_err();
        assert!(error.starts_with("1:"), "{error}");
        assert!(error.contains(message), "{expression}: {error}");
    }
}

#[test]
#[ignore = "one UEFI run of the Mira compiler component and expressions"]
fn native_operator_component_and_precedence_run_without_rust_services() {
    let definitions = source();
    let mut source = format!("{definitions}\n{}", frontend_program(true));
    // Assertions execute the compiler functions themselves in the native guest.
    source = source.replace("  if run()\n", "  ok = run()\n  ok = ok and operator_descriptor(6581857,3) == 513\n  ok = ok and operator_descriptor(15420,2) == 1803\n  ok = ok and operator_descriptor(43,1) == 2061\n  ok = ok and operator_descriptor(43,2) == -1\n  ok = ok and unary_descriptor(7630702,3) == 2563\n  ok = ok and unary_descriptor(33,1) == -1\n  ok = ok and punctuation_width(45,62) == 2\n  ok = ok and punctuation_width(33,-1) == 0\n  ok = ok and assignment_descriptor(15659,2) == 1\n  ok = ok and assignment_descriptor(15677,2) == -1\n  if ok\n");
    let program = syntax::parse(&source).unwrap();
    let esp = common::work().join("native-operators-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "native-operators").ends_with("O!"));
}
