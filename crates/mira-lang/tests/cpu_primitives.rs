mod common;
use common::*;
const SOURCE: &str = "type Scratch = [i64,2]\nboot()->unit =\n  held = Scratch(17,19)\n  original = sys.cpu_local()\n  local = sys.addr(8388608)\n  sys.set_cpu_local(local)\n  sys.store64(sys.cpu_local(),23)\n  count = (sys.cpuid_ebx(1,0) >> 16) & 255\n  valid = sys.cpuid_eax(0,0) >= 1 and (sys.cpuid_edx(1,0) & 32) != 0\n  vendor = sys.cpuid_ecx(0,0)\n  after = Scratch(29,31)\n  sys.set_cpu_local(original)\n  if count == 4 and valid and vendor != 0 and sys.load64(local) == 23 and sys.cpu_local() == original and held[0] == 17 and after[1] == 31\n    sys.out8(233,84)\n  else\n    sys.out8(233,70)\n  sys.out8(233,33)\n  sys.halt()\nspace main:\n  kernel = boot()\n";
#[test]
fn cpu_primitives_are_typed_and_not_executed_on_the_host() {
    let p = mira_lang::syntax::parse(SOURCE).unwrap();
    assert!(
        mira_lang::runtime::execute(&p, 1)
            .unwrap_err()
            .contains("system operation")
    );
    assert!(
        mira_lang::syntax::parse(
            &SOURCE.replace("sys.set_cpu_local(local)", "sys.set_cpu_local(7)")
        )
        .is_err()
    );
    assert!(
        mira_lang::syntax::parse(&SOURCE.replace("sys.cpuid_ebx(1,0)", "sys.cpuid_ebx(local,0)"))
            .is_err()
    );
}
#[test]
#[ignore = "requires QEMU/OVMF; four CPUs are configured but only BSP execution is tested"]
fn native_cpu_local_state_with_four_configured_cpus() {
    let image = native_image(SOURCE, "cpu-primitives");
    assert_eq!(qemu_cpus(&image, false, "cpu-primitives", 4), "T!");
    let p = mira_lang::syntax::parse(SOURCE).unwrap();
    let esp = work().join("cpu-primitives-esp");
    mira_lang::uefi::write_esp(&mira_lang::uefi::compile(&p).unwrap(), &esp).unwrap();
    assert!(qemu_cpus(&esp, true, "cpu-primitives-uefi", 4).ends_with("T!"));
}
