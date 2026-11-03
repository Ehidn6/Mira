mod access;
pub mod behavior;
mod bootstrap_codegen;
mod bootstrap_compiler;
mod bootstrap_lexer;
mod bootstrap_literals;
mod bootstrap_scalar;
mod bootstrap_lowering;
mod composition;
pub mod context;
pub mod data_runtime;
mod effects;
mod frontend;
pub mod inspect;
pub mod inference;
pub mod assist;
pub mod intelligence;
mod intrinsics;
pub mod kernel;
mod kernel_state;
pub mod learning;
pub mod linker;
pub mod native;
mod native_behavior;
mod native_state;
mod numeric;
pub mod project_files;
pub mod runtime;
pub mod source_cli;
pub mod source_tree;
pub mod syntax;
pub mod system;
mod tensor;
pub mod uefi;
use mira_core::{Program, Result};
use std::{fs, io::Write, path::Path};
/// Derived standalone bootstrap view of the compiler's actual MiraOS source
/// object. It retains definition/type IDs and is not a second stored library.
pub fn compiler_source() -> Result<&'static str> {
    Ok(&bootstrap_compiler::library()?.source)
}

pub fn load(path: &Path) -> Result<Program> {
    open_source(path).map(|(program, _)| program)
}

/// Open the authoritative source once, returning its derived Program and text view.
pub fn open_source(path: &Path) -> Result<(Program, String)> {
    if path.is_dir() {
        let (tree, program) = source_tree::open(path)?;
        return Ok((program, tree.assembled()?));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("project source exceeds storage budget".into());
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if text.trim_start().starts_with('{') {
        let (tree, program) = source_tree::open(path)?;
        return Ok((program, tree.assembled()?));
    }
    Ok((syntax::parse(&text)?, text))
}

/// Complete text projection for compilation/export; use source read ID for navigation.
pub fn source_text(path: &Path) -> Result<String> {
    open_source(path).map(|(_, source)| source)
}
/// Write a validated, reparsed model to a unique sibling file, then atomically rename.
pub fn save(path: &Path, program: &Program) -> Result<()> {
    program.validate()?;
    let text = syntax::format(program);
    if syntax::parse(&text)? != *program {
        return Err("serialization changed the model".into());
    }
    let tmp = path.with_extension(format!("mira-saving-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| e.to_string())
}

mod reflection;

pub mod compilation_cache;
mod differentiation;

pub mod information;

pub mod metaprogram;

/// Self-build the portable compiler from its canonical nested Mira source.
/// Rust supplies the VM and file I/O, not parsing/lowering/codegen of that input.
pub fn build_compiler(directory: &Path) -> Result<(usize,usize)> {
    bootstrap_scalar::module::write_compiler(directory)
}
