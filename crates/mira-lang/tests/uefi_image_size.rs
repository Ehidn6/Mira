#[allow(dead_code)]
mod common;

#[test]
#[ignore = "one UEFI boot of a real image larger than the Multiboot memory gap"]
fn uefi_loads_large_persistent_state_without_multiboot_address_limit() {
    let source = r#"type Pool = tensor[f32,65536]
memory()->Pool = Pool.persist()
boot()->unit =
  pool = cells.memory()
  zeroed = pool[0] == 0.0 and pool[65535] == 0.0
  pool[65535] = 3.0
  if zeroed and pool[65535] == 3.0 and pool[0] == 0.0
    sys.out8(233,66)
  else
    sys.out8(233,70)
  sys.out8(233,33)
  sys.halt()
space Large:
  kernel = boot():
    space State:
      memory = memory()
"#;
    let program = mira_lang::syntax::parse(source).unwrap();
    // The persistent pool plus its transaction bank exceeds one MiB. This is
    // actual linked state, not padding added to fool a loader-only check.
    let payload = mira_lang::uefi::compile(&program).unwrap();
    assert!(payload.len() > 1024 * 1024);
    assert!(payload.len() < 2 * 1024 * 1024);
    let multiboot = mira_lang::kernel::compile(&program).unwrap_err();
    assert!(
        multiboot.contains("exceeds image address limit"),
        "{multiboot}"
    );
    let esp = common::work().join("large-uefi-esp");
    mira_lang::uefi::write_esp(&payload, &esp).unwrap();
    assert!(common::qemu(&esp, true, "large-uefi").ends_with("B!"));
}
