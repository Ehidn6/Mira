fn arguments(values: &[i64]) -> String {
    let mut slots = [0i64; 16];
    slots[..values.len()].copy_from_slice(values);
    slots
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}
fn definition(_source: &str, name: &str) -> String {
    static PROGRAM: std::sync::OnceLock<mira_core::Program> = std::sync::OnceLock::new();
    let program = PROGRAM
        .get_or_init(|| mira_lang::syntax::parse(mira_lang::compiler_source().unwrap()).unwrap());
    mira_lang::behavior::format(
        program
            .code_modules
            .iter()
            .find(|d| d.name == name)
            .unwrap(),
    )
}
#[allow(dead_code)]
mod common;
fn definitions() -> String {
    mira_lang::compiler_source().unwrap().to_string()
}
#[test]
#[ignore = "one finite QEMU run: actual shared Mira parser and x86 emitter"]
fn source_functions_compile_and_execute_with_checked_failures() {
    let mut source = definitions();
    source.push_str(r#"
type CodeBuffer = [i64,512]
type CallContext = [i64,17]
type Arguments = [i64,16]
type CompileControl = [i64,6]
source_input()->ScalarSource = ScalarSource.persist()
code_output()->ScalarBytes = ScalarBytes.persist()
control()->CompileControl = CompileControl.persist()
struct TestState:
  case_number: i64
  memory: CodeBuffer
  context: CallContext
compile_case()->i64 =
  work = ScalarWork()
  nodes = ScalarNodes()
  lexer = ScalarLexer(LexBytes(),LexState())
  literal = LiteralBytes()
  value = LiteralValue()
  parameters = ScalarParameters()
  parsed = scalar_parse(cells.source(),cells.control()[0],work,lexer,literal,value,nodes,parameters)
  if cells.control()[1] != 0 and parsed == 0
    nodes[work.root * 6 + 1] = work.root
  code = ScalarCode()
  size = scalar_emit(work,nodes,parameters,code,cells.output(),4096,ScalarFixups())
  cells.control()[2] = work.error
  cells.control()[3] = code.size
  cells.control()[4] = work.count
  cells.control()[5] = work.error_offset
  size
check(state:TestState,length:i64,args:Arguments,expected:i64,expected_error:i64)->bool =
  state.case_number += 1
  cells.control()[0] = length
  cells.control()[1] = 0
  size = sys.scratch(compile_case,500000)
  if size > 0
    for index in 0..16
      state.context[index] = args[index]
    address = sys.data_address(state.memory)
    for index in 0..size
      sys.store8(sys.offset(address,index),cells.output()[index])
    barrier = sys.cpuid_eax(0,0)
    result = sys.call_sysv64(address,sys.data_address(state.context))
    passed = result == expected and state.context[16] == expected_error
    if not passed
      sys.out8(233,82)
      sys.out8(233,64 + state.case_number)
    passed
  else
    sys.out8(233,69)
    sys.out8(233,64 + state.case_number)
    sys.out8(233,48 + cells.control()[2])
    sys.out8(233,48 + cells.control()[3] / 1000)
    sys.out8(233,48 + cells.control()[4] / 100)
    false
reject(state:TestState,length:i64,corrupt:bool)->bool =
  state.case_number += 1
  cells.control()[0] = length
  cells.control()[1] = corrupt ? 1 : 0
  cells.output()[0] = 123
  size = sys.scratch(compile_case,500000)
  passed = size == -1 and cells.output()[0] == 123
  if not passed
    sys.out8(233,88)
    sys.out8(233,64 + state.case_number)
  passed
scalar_budget(value:i64)->i64 = ((value + value) + (value + value)) + ((value + value) + (value + value))
budget_work()->unit =
  value = scalar_budget(1)
kernel()->unit =
  state = TestState(0,CodeBuffer.aligned(16),CallContext())
  ok = true
"#);
    for (text, input, value, error) in [
        ("f(x:i64)->i64 = (x + 3) * 2\n", 7i64, 20i64, 0i64),
        ("f(x:i64)->i64 = ~x & 0xff\n", 0, 255, 0),
        ("f()->i64 = -9223372036854775808\n", 0, i64::MIN, 0),
        ("f(x:i64)->i64 = x / 3\n", -20, -6, 0),
        ("f(x:i64)->i64 = x % 3\n", -20, -2, 0),
        ("f(x:i64)->i64 = x + 1\n", i64::MAX, 0, 1),
        ("f(x:i64)->i64 = x / 0\n", 7, 0, 2),
        ("f(x:i64)->i64 = x / -1\n", i64::MIN, 0, 1),
    ] {
        let args = arguments(&[input]);
        source.push_str(&format!(
            "  ok = check(state,CaseText({}),{},Arguments({args}),{value},{error}) and ok\n",
            serde_json::to_string(text).unwrap(),
            text.len()
        ));
    }
    // Real compiler definitions, not renamed example arithmetic. This is the
    // first self-compilation boundary: stage-0 emits the Mira compiler, which
    // then reads and compiles these functions from that compiler's own source.
    let lexer = mira_lang::compiler_source().unwrap();
    let words = mira_lang::compiler_source().unwrap();
    let integers = mira_lang::compiler_source().unwrap();
    for (file, name, cases) in [
        (
            words,
            "pack3",
            vec![
                (vec![1, 200], "pack3(1,200)"),
                (vec![255, 255], "pack3(255,255)"),
            ],
        ),
        (
            words,
            "immediate_prefix",
            vec![
                (vec![0], "immediate_prefix(0)"),
                (vec![15], "immediate_prefix(15)"),
                (vec![16], "immediate_prefix(16)"),
            ],
        ),
        (
            words,
            "frame_access_prefix",
            vec![
                (vec![0, 0], "frame_access_prefix(false,0)"),
                (vec![1, 15], "frame_access_prefix(true,15)"),
            ],
        ),
        (
            integers,
            "literal_digit",
            vec![
                (vec![65], "literal_digit(65)"),
                (vec![102], "literal_digit(102)"),
                (vec![47], "literal_digit(47)"),
            ],
        ),
        (
            lexer,
            "operator_descriptor",
            vec![
                (vec![43, 1], "operator_descriptor(43,1)"),
                (vec![15420, 2], "operator_descriptor(15420,2)"),
                (vec![43, 2], "operator_descriptor(43,2)"),
            ],
        ),
        (
            lexer,
            "unary_descriptor",
            vec![
                (vec![45, 1], "unary_descriptor(45,1)"),
                (vec![7629166, 3], "unary_descriptor(7629166,3)"),
            ],
        ),
        (
            lexer,
            "digit",
            vec![
                (vec![47], "digit(47) ? 1 : 0"),
                (vec![57], "digit(57) ? 1 : 0"),
            ],
        ),
        (
            lexer,
            "letter",
            vec![
                (vec![65], "letter(65) ? 1 : 0"),
                (vec![123], "letter(123) ? 1 : 0"),
            ],
        ),
        (
            lexer,
            "whitespace",
            vec![
                (vec![9], "whitespace(9) ? 1 : 0"),
                (vec![0], "whitespace(0) ? 1 : 0"),
            ],
        ),
        (
            lexer,
            "state_valid",
            vec![
                (vec![11, 5], "state_valid(11,5) ? 1 : 0"),
                (vec![12, 5], "state_valid(12,5) ? 1 : 0"),
            ],
        ),
    ] {
        let text = definition(file, name);
        for (args, expected) in cases {
            let args = arguments(&args);
            source.push_str(&format!(
                "  ok = check(state,CaseText({}),{},Arguments({args}),{expected},0) and ok\n",
                serde_json::to_string(&text).unwrap(),
                text.len()
            ));
        }
    }
    for (text, args, expected, error) in [
        ("f(a:i64,b:i64)->i64 = (a << b) | (a >> b)\n", "8,2", 34, 0),
        ("f(a:bool,b:i64)->bool = a or b / 0 == 2\n", "1,7", 1, 0),
        ("f(a:bool,b:i64)->bool = a and b / 0 == 2\n", "0,7", 0, 0),
        ("f(a:bool,b:i64)->i64 = a ? b + 1 : b - 1\n", "1,7", 8, 0),
        ("f(a:bool,b:i64)->i64 = a ? b + 1 : b - 1\n", "0,7", 6, 0),
        ("f(a:bool)->bool = not a\n", "4", 0, 3),
        (
            "f(a:i64,b:i64,c:i64,d:i64,e:i64,f:i64,g:i64,h:i64)->i64 = a+b+c+d+e+f+g+h\n",
            "1,2,3,4,5,6,7,8",
            36,
            0,
        ),
        ("f(a:bool)->i64 = a ? 17 : 1 / 0\n", "1", 17, 0),
    ] {
        let args = arguments(
            &args
                .split(',')
                .map(|a| a.parse::<i64>().unwrap())
                .collect::<Vec<_>>(),
        );
        source.push_str(&format!(
            "  ok = check(state,CaseText({}),{},Arguments({args}),{expected},{error}) and ok\n",
            serde_json::to_string(text).unwrap(),
            text.len()
        ));
    }
    for (text, corrupt) in [
        ("f(a:bool)->i64 = a + 1\n", false),
        ("f(a:i64)->i64 = a + 1\n", true),
        ("f(a:i64)->i64 =\n  const x = a\n  x = 3\n  x\n", false),
        ("f(a:bool)->i64 =\n  if a\n    x = 3\n  x\n", false),
    ] {
        source.push_str(&format!(
            "  ok = reject(state,CaseText({}),{},{corrupt}) and ok\n",
            serde_json::to_string(text).unwrap(),
            text.len()
        ));
    }
    source.push_str("  copy_cost = ScalarSource.size_bytes() / 8 + ScalarBytes.size_bytes() / 8 + CompileControl.size_bytes() / 8\n  small = sys.neural_try(budget_work,copy_cost + 12,0)\n  metered = not small and sys.neural_status() == 1\n  enough = sys.neural_try(budget_work,copy_cost + 128,0)\n  ok = ok and metered and enough\n  sys.out8(233,ok ? 67 : 70)\n  sys.out8(233,33)\n  sys.halt()\n");
    // Load each source literal in bounded scratch. Compilation writes to the
    // ordinary persistent graph output; execution happens outside scratch.
    let start = source.find("kernel()->unit =\n").unwrap();
    let kernel = source[start..].to_string();
    source.truncate(start);
    let mut rewritten = String::new();
    for (index, line) in kernel.lines().enumerate() {
        if let Some((before, tail)) = line.split_once("CaseText(") {
            let mut decoder = serde_json::Deserializer::from_str(tail).into_iter::<String>();
            let text = decoder.next().unwrap().unwrap();
            let rest = &tail[decoder.byte_offset()..];
            let rest = rest.strip_prefix("),").unwrap();
            let (_, arguments) = rest.split_once(',').unwrap();
            source.push_str(&format!("load_source_{index}()->i64 =\n  text = ScalarSource({})\n  input = cells.source()\n  for byte in 0..{}\n    input[byte] = text[byte]\n  {}\n",serde_json::to_string(&text).unwrap(),text.len(),text.len()));
            rewritten.push_str(&format!(
                "{before}sys.scratch(load_source_{index},ScalarSource.size_bytes() + 4096),{arguments}\n"
            ));
        } else {
            rewritten.push_str(line);
            rewritten.push('\n');
        }
    }
    source.push_str(&rewritten);
    source.push_str("space CompilerCheck:\n  kernel = kernel():\n    space State:\n      source = source_input()\n      output = code_output()\n      control = control()\n");
    // The expanded compiler no longer fits the fixed Multiboot payload gap.
    // Use the actual UEFI target rather than changing that loader's limits.
    let program = mira_lang::syntax::parse(&source).unwrap();
    let image = common::work().join("self-compiler-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &image).unwrap();
    let output = common::qemu(&image, true, "shared-scalar-compiler");
    assert!(output.ends_with("C!"), "guest output: {output}");
}
