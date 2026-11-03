//! UEFI/GOP packaging and QEMU configuration. Graphics remain Mira code.
use mira_core::*;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
pub fn compile(program: &Program) -> Result<Vec<u8>> {
    let code = crate::kernel::payload(program, crate::native_behavior::Target::Uefi)?;
    package(code)
}
pub fn compile_system(system: &system::System) -> Result<Vec<u8>> {
    package(crate::kernel::payload_system(
        system,
        crate::native_behavior::Target::Uefi,
    )?)
}
fn package(code: Vec<u8>) -> Result<Vec<u8>> {
    let mut payload = b"MIRAK64\0".to_vec();
    payload.extend(1u32.to_le_bytes());
    payload.extend(32u32.to_le_bytes());
    payload.extend((code.len() as u64).to_le_bytes());
    payload.extend(32u64.to_le_bytes());
    payload.extend(code);
    Ok(payload)
}
pub fn prepare(program: &Program, directory: &Path) -> Result<()> {
    let payload = compile(program)?;
    write_esp(&payload, directory)?;
    std::fs::write(
        directory.join("source.mira"),
        crate::syntax::format(program),
    )
    .map_err(|e| e.to_string())
}
pub fn write_esp(payload: &[u8], directory: &Path) -> Result<()> {
    let boot = directory.join("EFI/BOOT");
    std::fs::create_dir_all(&boot).map_err(|e| e.to_string())?;
    std::fs::write(
        boot.join("BOOTX64.EFI"),
        include_bytes!("../boot/uefi/BOOTX64.EFI"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(directory.join("kernel.bin"), payload).map_err(|e| e.to_string())?;
    std::fs::write(
        directory.join("font.bin"),
        include_bytes!("../boot/uefi/font8x8.bin"),
    )
    .map_err(|e| e.to_string())?;
    // This entry point has no Program: remove only our previously generated source.
    match std::fs::remove_file(directory.join("source.mira")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
fn firmware(variable: &str, filename: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable) {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Ok(p);
        }
        return Err(format!("{variable}: firmware file does not exist"));
    }
    for base in [
        "/usr/share/edk2/x64",
        "/usr/share/edk2-ovmf/x64",
        "/usr/share/OVMF",
        "/usr/share/OVMF/x64",
    ] {
        let p = Path::new(base).join(filename);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "OVMF firmware missing: set {variable} to {filename}"
    ))
}
pub fn qemu_command(esp: &Path, runtime: &Path) -> Result<Command> {
    let code = firmware("MIRA_OVMF_CODE", "OVMF_CODE.4m.fd")?;
    let vars_template = firmware("MIRA_OVMF_VARS", "OVMF_VARS.4m.fd")?;
    std::fs::create_dir_all(runtime).map_err(|e| e.to_string())?;
    let vars = runtime.join("OVMF_VARS.fd");
    if !vars.exists() {
        std::fs::copy(vars_template, &vars).map_err(|e| e.to_string())?;
    }
    let esp = std::fs::canonicalize(esp).map_err(|e| e.to_string())?;
    // The ESP stays read-only; optional state uses a separately marked image file.
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args([
        "-accel", "tcg", "-machine", "q35", "-m", "128M", "-vga", "std",
    ])
    .args([
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", code.display()),
    ])
    .args([
        "-drive",
        &format!("if=pflash,format=raw,file={}", vars.display()),
    ])
    .args([
        "-drive",
        &format!(
            "if=none,id=esp,format=raw,readonly=on,file=fat:{}",
            esp.display()
        ),
    ])
    .args([
        "-device",
        "virtio-blk-pci,drive=esp,bootindex=1",
        "-no-reboot",
        "-no-shutdown",
    ]);
    if let Some(path) = std::env::var_os("MIRA_STATE_DISK") {
        attach_state_disk(&mut cmd, Path::new(&path))?;
    }
    Ok(cmd)
}

/// Provision only a dedicated regular image, never reformat an existing file.
pub fn attach_state_disk(cmd: &mut Command, path: &Path) -> Result<()> {
    use std::io::{Read, Write};
    const SIZE: u64 = 2 * 1024 * 1024;
    const MAGIC: &[u8; 8] = b"MIRAVOL2";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            let created = (|| -> std::io::Result<()> {
                file.set_len(SIZE)?;
                file.write_all(MAGIC)?;
                file.write_all(&2u64.to_le_bytes())?;
                file.sync_all()
            })();
            if let Err(error) = created {
                let _ = std::fs::remove_file(path);
                return Err(error.to_string());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() || (metadata.len() != SIZE && metadata.len() != SIZE / 2) {
        return Err(
            "state disk must be a dedicated 2 MiB regular image; existing file was not changed"
                .into(),
        );
    }
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|e| e.to_string())?;
    if &header[..8] != MAGIC || header[8..] != 2u64.to_le_bytes() {
        return Err("unrecognized state volume; existing file was not changed".into());
    }
    // Reserve appended sectors for larger source drafts. Existing neural/source
    // bytes are left in place; unknown volumes never reach this operation.
    if metadata.len() < SIZE {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() != SIZE / 2 {
            return Err("state disk size changed while opening it".into());
        }
        file.set_len(SIZE)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
    }

    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    // -blockdev uses JSON to keep commas and spaces in paths unambiguous.
    let block = serde_json::json!({"driver":"raw","node-name":"mira-state","file":{"driver":"file","filename":path}});
    cmd.arg("-blockdev").arg(block.to_string()).args(["-device","virtio-blk-pci,drive=mira-state,addr=06.0,disable-modern=on,vectors=0,queue-size=256,serial=MIRA-EXPERIENCE"]);
    Ok(())
}
