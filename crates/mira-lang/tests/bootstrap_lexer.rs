#[allow(dead_code)]
mod common;
use mira_lang::{runtime, syntax};

#[test]
fn normal_frontend_streams_long_lines_and_preserves_lexical_diagnostics() {
    // The source representation exceeds a window, while decoded storage does
    // not. A source-buffer boundary must not become a text-literal size limit.
    let escaped = "\\u0061".repeat(1000);
    let comment = "` [ \" // ё".repeat(1500);
    let source = format!(
        "type Text = [i64,1000]\nrun()->i64 =\n  text = Text(\"{escaped}\")\n  result = 3 //{comment}\n  text[999] + result\nspace Main:\n  result = run()\n"
    );
    let program = syntax::parse(&source).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 100);
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);

    // Identifiers retain their existing limits rather than acquiring a window
    // limit. The final character is scanned in a later buffer.
    let name = format!("_{}", "a".repeat(8192));
    let source =
        format!("{name}()->i64 = 42\nrun()->i64 = {name}()\nspace Main:\n  result = run()\n");
    assert_eq!(
        runtime::execute(&syntax::parse(&source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        42
    );

    for (line, message) in [
        ("  text = Text(\"unfinished\\", "unclosed text literal"),
        ("  value = `unfinished", "closed quoted identifier"),
        ("  value = `bad name`", "closed quoted identifier"),
        ("  value = ё", "unsupported character"),
    ] {
        let source = format!(
            "type Text = [i64,8]\nrun()->i64 =\n{line}\n  0\nspace Main:\n  result = run()\n"
        );
        let error = syntax::parse(&source).unwrap_err();
        assert!(error.starts_with("3:"), "{error}");
        assert!(error.contains(message), "{error}");
    }
    // Delimiters inside text/quoted names remain data, not layout or comments.
    let source = r#"type Text = [i64,32]
`some-name`()->i64 = 1
run()->i64 =
  text = Text("é\"//()[]`\n")
  value = (
    `some-name`() +
    text[0]
  )
  value
space Main:
  result = run()
"#;
    assert_eq!(
        runtime::execute(&syntax::parse(source).unwrap(), 1)
            .unwrap()
            .terminals[0]
            .1,
        196
    );
}

#[test]
#[ignore = "one UEFI run of the streaming Mira scanner, with no host parser"]
fn native_scanner_keeps_state_and_lookahead_across_windows() {
    let definitions = mira_lang::compiler_source().unwrap();
    let source = format!(
        "{definitions}\n{}",
        r#"
type LexProbe = [i64,4]
struct LexCounter:
  value: i64
next_probe(counter:LexCounter)->i64 =
  counter.value += 1
  counter.value
check_padding()->unit =
  counter = LexCounter(0)
  probe = LexProbe(next_probe(counter),next_probe(counter),0,0)
  if counter.value != 2 or probe[0] != 1 or probe[1] != 2 or probe[2] != 0 or probe[3] != 0
    invalid = 1 / 0
  padded = LexBytes("hi")
  if padded[0] != 104 or padded[1] != 105 or padded[2] != 0 or padded[4095] != 0
    invalid = 1 / 0
boot()->unit =
  small_budget = sys.neural_try(check_padding,6000,40000)
  enough_budget = sys.neural_try(check_padding,20000,40000)
  state = LexState()
  first = LexBytes("\"a\\")
  status = compiler(first,0,3,false,state)
  ok = not small_budget and enough_budget and status == 0 and state[0] == 8 and state[1] == 2 and state[2] == 3
  second = LexBytes("\"b\"")
  status = compiler(second,0,3,true,state)
  ok = ok and status == 2 and state[0] == 0 and state[2] == 3
  decimal = LexBytes("12.")
  status = compiler(decimal,0,3,false,state)
  ok = ok and status == 0 and state[0] == 3 and state[2] == 2
  fraction = LexBytes(".5e-")
  status = compiler(fraction,0,4,false,state)
  ok = ok and status == 0 and state[0] == 6 and state[2] == 4
  exponent = LexBytes("2+")
  status = compiler(exponent,0,2,true,state)
  ok = ok and status == 1 and state[0] == 0 and state[2] == 1
  status = compiler(exponent,1,2,true,state)
  ok = ok and status == 1 and state[2] == 2
  zero = LexBytes("0")
  status = compiler(zero,0,1,false,state)
  ok = ok and status == 0 and state[0] == 0 and state[2] == 0
  hex = LexBytes("0xFF+")
  status = compiler(hex,0,5,true,state)
  ok = ok and status == 1 and state[2] == 4
  quoted = LexBytes("`some-")
  status = compiler(quoted,0,6,false,state)
  ok = ok and status == 0 and state[0] == 9 and state[2] == 6
  rest = LexBytes("name`")
  status = compiler(rest,0,5,true,state)
  ok = ok and status == 3 and state[2] == 5
  blank = LexBytes(" \t")
  status = compiler(blank,0,2,true,state)
  ok = ok and status == 4 and state[2] == 2
  comment = LexBytes("//[\"`")
  status = compiler(comment,0,5,true,state)
  ok = ok and status == 5 and state[2] == 5
  arrow = LexBytes("->")
  status = compiler(arrow,0,2,true,state)
  ok = ok and status == 1 and state[2] == 2
  unsupported = LexBytes(";")
  status = compiler(unsupported,0,1,true,state)
  ok = ok and status == -1 and state[0] == 0
  unfinished = LexBytes("\"a")
  status = compiler(unfinished,0,2,true,state)
  ok = ok and status == -2 and state[0] == 0
  status = compiler(quoted,0,6,true,state)
  ok = ok and status == -3 and state[0] == 0
  status = compiler(zero,0,0,true,state)
  ok = ok and status == 6 and state[2] == 0
  state[2] = 99
  status = compiler(zero,0,4097,true,state)
  ok = ok and status == -4 and state[2] == 99
  status = compiler(zero,-1,1,true,state)
  ok = ok and status == -4 and state[2] == 99
  if ok
    sys.out8(233,83)
  else
    sys.out8(233,70)
  sys.out8(233,33)
  sys.halt()
space NativeLexer:
  kernel = boot()
"#
    );
    let program = syntax::parse(&source).unwrap();
    let esp = common::work().join("native-scanner-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "native-scanner").ends_with("S!"));
}
