//! Keep editable source text separate from its complete runtime checkpoint.
use mira_core::system::System;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MARKER: &str = "mira-source-state-v2";
const LIMIT: u64 = 24 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq)]
pub enum Format {
    Source,
    Project,
}
#[derive(Serialize, Deserialize)]
struct SourceState {
    format: String,
    source: String,
    system: System,
}
fn state_path(source: &Path) -> PathBuf {
    let mut name = source.as_os_str().to_os_string();
    name.push(".state.json");
    PathBuf::from(name)
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("project/source state exceeds storage limit".into());
    }
    Ok(bytes)
}
fn decode_sidecar(path: &Path) -> Result<SourceState, String> {
    let state: SourceState = serde_json::from_slice(&read(path)?).map_err(|_| {
        format!(
            "{} is not a Mira source checkpoint; it will not be overwritten",
            path.display()
        )
    })?;
    if state.format != MARKER {
        return Err("unrecognized source checkpoint format".into());
    }
    crate::system::check(&state.system)?;
    Ok(state)
}
pub fn load(path: &Path) -> Result<(System, Format), String> {
    let bytes = read(path)?;
    if bytes.starts_with(b"MIRA\x04\0\0\0") {
        return Ok((crate::system::decode(&bytes)?, Format::Project));
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| "source file is not UTF-8")?;
    let system = crate::system::decode(&bytes)?;
    let sidecar = state_path(path);
    if !sidecar.exists() {
        return Ok((system, Format::Source));
    }
    let saved = decode_sidecar(&sidecar)?;
    if saved.source != source || saved.system.program != system.program {
        return Err(format!(
            "Source and checkpoint differ: {}. The saved state was not applied. Move the checkpoint aside to open the edited source without that state.",
            sidecar.display()
        ));
    }
    Ok((saved.system, Format::Source))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temp = path.as_os_str().to_os_string();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    temp.push(format!(".saving-{}-{nonce}", std::process::id()));
    let temp = PathBuf::from(temp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e: std::io::Error| e.to_string())
}
pub fn save(path: &Path, system: &System, format: Format) -> Result<(), String> {
    if format == Format::Project {
        return crate::system::save(path, system);
    }
    crate::system::check(system)?;
    let source = crate::syntax::format(&system.program);
    if crate::syntax::parse(&source)? != system.program {
        return Err("text serialization changed the program".into());
    }
    let sidecar = state_path(path);
    if sidecar.exists() {
        let saved = decode_sidecar(&sidecar)?;
        if !path.exists() || read(path)? != saved.source.as_bytes() {
            return Err(format!(
                "Existing source checkpoint does not belong to the current text: {}. Use Save As or move the checkpoint aside.",
                sidecar.display()
            ));
        }
    }
    // A crash between the two renames causes an explicit mismatch on load,
    // never silent application of a checkpoint to a different source program.
    let state = SourceState {
        format: MARKER.into(),
        source: source.clone(),
        system: system.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("source checkpoint exceeds storage limit".into());
    }
    atomic_write(&sidecar, &bytes)?;
    atomic_write(path, source.as_bytes())
}
pub fn selected_format(path: &Path) -> Format {
    if path.extension().is_some_and(|e| e == "mira-system") {
        Format::Project
    } else {
        Format::Source
    }
}
