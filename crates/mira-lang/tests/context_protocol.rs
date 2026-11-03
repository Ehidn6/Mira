#[allow(dead_code)]
mod common;
use mira_lang::inspect::{Options, Snapshot};

const SOURCE: &str =
    "helper(x:i64)->i64 = x+1\nrun()->i64 = helper(2)\nspace Main:\n  result @100 = run()\n";

#[test]
fn entry_and_cards_reuse_program_identities_without_exporting_bodies() {
    let program = mira_lang::syntax::parse(SOURCE).unwrap();
    let snapshot = Snapshot::new(&program).unwrap();
    let options = Options::default();
    let entry = snapshot.query("enter", None, &options).unwrap();
    assert_eq!(mira_lang::inspect::command(&program, &[]).unwrap(), entry);
    assert_eq!(
        entry["protocol"]["rules"],
        serde_json::json!(mira_core::inspect::ENTRY_RULES)
    );
    assert_eq!(entry["mutation"], false);
    let definitions = snapshot.query("definitions", None, &options).unwrap();
    assert!(definitions.to_string().contains("helper"));
    let id = program
        .code_modules
        .iter()
        .find(|d| d.name == "helper")
        .unwrap()
        .id;
    let card = snapshot
        .query("card", Some(&id.to_string()), &options)
        .unwrap();
    assert_eq!(card["name"], "helper");
    assert_eq!(card["in"][0][1], "i64");
    assert_eq!(card["effect_analysis"], "not_provided");
    assert!(card.get("source").is_none());
    let cell = snapshot.query("card", Some("100"), &options).unwrap();
    assert_eq!(cell["kind"], "cell");
    let source = snapshot
        .query("module", Some(&id.to_string()), &options)
        .unwrap();
    assert_eq!(source["hash"], card["source_hash"]);
    assert_eq!(source["line_scope"], "definition");
    assert_eq!(source["authority"], "project_content");
}

#[test]
fn protocol_keeps_budget_revision_and_known_source_guards() {
    let program = mira_lang::syntax::parse(SOURCE).unwrap();
    let snapshot = Snapshot::new(&program).unwrap();
    let stale = Options {
        revision: Some("stale".into()),
        ..Options::default()
    };
    assert!(snapshot.query("enter", None, &stale).is_err());
    assert!(snapshot.query("card", Some("100"), &stale).is_err());
    let small = Options {
        budget: 512,
        ..Options::default()
    };
    assert!(snapshot.query("enter", None, &small).is_err());
    let source = snapshot
        .query("code", Some("100"), &Options::default())
        .unwrap();
    let known = Options {
        known: Some(source["hash"].as_str().unwrap().into()),
        ..Options::default()
    };
    let skipped = snapshot.query("code", Some("100"), &known).unwrap();
    assert_eq!(skipped["unchanged"], true);
    assert_eq!(skipped["authority"], "project_content");
    assert!(skipped.get("source").is_none());
    assert!(
        snapshot
            .query("card", Some("999999"), &Options::default())
            .is_err()
    );
}

fn reflection_source() -> String {
    let expected: usize = mira_core::inspect::ENTRY_RULES
        .iter()
        .flat_map(|r| r.bytes())
        .map(usize::from)
        .sum();
    format!(
        "verify()->i64 =\n  sum = 0\n  for rule in 0..reflect.rule_count()\n    for byte in 0..reflect.rule_length(rule)\n      sum += reflect.rule_byte(rule,byte)\n  ok = reflect.protocol_version() == 1 and reflect.rule_count() == {}\n  ok = ok and reflect.rule_length(-1) == -1 and reflect.rule_byte(0,-1) == -1\n  ok = ok and reflect.rule_byte(0,reflect.rule_length(0)) == -1\n  ok and sum == {expected} ? 1 : 0\nrun()->i64 = sys.read_only(verify,4096)\nspace Main:\n  result = run()\n",
        mira_core::inspect::ENTRY_RULES.len()
    )
}

#[test]
fn mira_program_reads_the_same_protocol_through_reflection() {
    let program = mira_lang::syntax::parse(&reflection_source()).unwrap();
    assert_eq!(
        mira_lang::runtime::execute(&program, 1).unwrap().terminals[0].1,
        1
    );
}

#[test]
#[ignore = "one UEFI/QEMU check of native protocol reflection"]
fn native_mira_reads_protocol_without_a_host_query_service() {
    let source = reflection_source().replace("space Main:\n  result = run()", "boot()->unit =\n  if run() == 1\n    sys.out8(233,80)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace Main:\n  kernel = boot()");
    let program = mira_lang::syntax::parse(&source).unwrap();
    let esp = common::work().join("context-protocol-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "context-protocol").ends_with("P!"));
}

#[test]
fn revision_words_match_the_inspect_snapshot_identity() {
    let program = mira_lang::syntax::parse("type Revision = [i64,4]\nrevision()->Revision = Revision(reflect.revision_word(0),reflect.revision_word(1),reflect.revision_word(2),reflect.revision_word(3))\ninvalid()->i64 = reflect.revision_word(4)\nspace Main:\n  words = revision()\n  invalid = invalid()\n").unwrap();
    let revision = mira_lang::inspect::Snapshot::new(&program)
        .unwrap()
        .revision;
    let result = mira_lang::runtime::execute(&program, 1).unwrap();
    let handle = result
        .terminals
        .iter()
        .find(|(name, _)| name.ends_with("/words"))
        .unwrap()
        .1;
    let mut bytes = Vec::new();
    for index in 0..4 {
        bytes.extend(result.data.get(handle, index).unwrap().to_le_bytes());
    }
    let actual: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(actual, revision);
    assert_eq!(
        result
            .terminals
            .iter()
            .find(|(name, _)| name.ends_with("/invalid"))
            .unwrap()
            .1,
        -1
    );
}

#[test]
#[ignore = "one UEFI check of source revision across native Adaptive lowering"]
fn native_revision_keeps_the_inspect_identity_before_adaptive_lowering() {
    let source = r#"counter(state_value:i64)->i64 = state_value + 1
boot()->unit =
  for word_index in 0..4
    word = reflect.revision_word(word_index)
    for byte_index in 0..8
      byte = (word >> (byte_index * 8)) & 255
      high = byte >> 4
      low = byte & 15
      sys.out8(233,high < 10 ? high + 48 : high + 87)
      sys.out8(233,low < 10 ? low + 48 : low + 87)
  sys.out8(233,33)
  sys.halt()
space Main:
  kernel = boot():
    space Internal:
      counter = adaptive(counter)
"#;
    let program = mira_lang::syntax::parse(source).unwrap();
    let expected = Snapshot::new(&program).unwrap().revision;
    let esp = common::work().join("adaptive-revision-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&program).unwrap(), &esp).unwrap();
    assert!(common::qemu(&esp, true, "adaptive-revision").ends_with(&format!("{expected}!")));
}
