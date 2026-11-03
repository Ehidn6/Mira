use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub fn miraos_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../projects/MiraOS")
        .canonicalize()
        .unwrap();
    mira_lang::source_text(&path).unwrap()
}
pub fn work() -> PathBuf {
    let p = std::env::temp_dir().join(format!("mira-neural-tests-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
pub fn qemu(image: &Path, uefi: bool, label: &str) -> String {
    qemu_cpus(image, uefi, label, 1)
}
pub fn qemu_cpus(image: &Path, uefi: bool, label: &str, cpus: usize) -> String {
    qemu_config(image, uefi, label, cpus, &[], if uefi { 12 } else { 4 })
}
pub fn qemu_devices(image: &Path, uefi: bool, label: &str, devices: &[&str]) -> String {
    qemu_config(image, uefi, label, 1, devices, if uefi { 12 } else { 4 })
}
pub fn qemu_timed(image: &Path, uefi: bool, label: &str, seconds: u64) -> String {
    assert!((1..=180).contains(&seconds));
    qemu_config(image, uefi, label, 1, &[], seconds)
}
pub fn qemu_timed_memory(image: &Path, uefi: bool, label: &str, seconds: u64, memory_mib: usize) -> String {
    assert!((1..=180).contains(&seconds) && (128..=1024).contains(&memory_mib));
    qemu_config(image, uefi, label, 1, &["-m", &format!("{memory_mib}M")], seconds)
}
fn qemu_config(
    image: &Path,
    uefi: bool,
    label: &str,
    cpus: usize,
    devices: &[&str],
    seconds: u64,
) -> String {
    assert!((1..=64).contains(&cpus));
    let log = work().join(format!("{label}.log"));
    let errors = work().join(format!("{label}.qemu.log"));
    let mut command = Command::new("qemu-system-x86_64");
    command
        .args([
            "-accel",
            "tcg",
            "-m",
            "128M",
            "-display",
            "none",
            "-serial",
            "none",
            "-monitor",
            "none",
            "-no-reboot",
            "-no-shutdown",
            "-nic",
            "none",
            "-debugcon",
        ])
        .arg(format!("file:{}", log.display()));
    if uefi {
        let firmware = std::env::var_os("MIRA_OVMF_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/edk2/x64"));
        let vars = work().join(format!("{label}-vars.fd"));
        std::fs::copy(firmware.join("OVMF_VARS.4m.fd"), &vars).unwrap();
        command
            .args(["-machine", "q35", "-vga", "std", "-drive"])
            .arg(format!(
                "if=pflash,format=raw,readonly=on,file={}",
                firmware.join("OVMF_CODE.4m.fd").display()
            ))
            .arg("-drive")
            .arg(format!("if=pflash,format=raw,file={}", vars.display()))
            .arg("-drive")
            .arg(format!(
                "if=none,id=esp,format=raw,readonly=on,file=fat:{}",
                image.display()
            ))
            .args(["-device", "virtio-blk-pci,drive=esp,bootindex=1"]);
    } else {
        command.arg("-kernel").arg(image);
    }
    command
        .arg("-smp")
        .arg(format!("cpus={cpus},sockets=1,cores={cpus},threads=1"));
    command.args(devices);
    let mut child = command
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&errors).unwrap())
        .spawn()
        .expect("QEMU must be installed for native tests");
    let until = Instant::now() + Duration::from_secs(seconds);
    let output = loop {
        let output = std::fs::read_to_string(&log).unwrap_or_default();
        if output.ends_with('!') || Instant::now() >= until || child.try_wait().unwrap().is_some() {
            break output;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        output.ends_with('!'),
        "guest did not finish: {output:?}; QEMU: {}",
        std::fs::read_to_string(errors).unwrap_or_default()
    );
    output
}
pub fn native_image(source: &str, label: &str) -> PathBuf {
    let p = mira_lang::syntax::parse(source).unwrap();
    let path = work().join(format!("{label}.elf"));
    std::fs::write(&path, mira_lang::kernel::compile(&p).unwrap()).unwrap();
    path
}
