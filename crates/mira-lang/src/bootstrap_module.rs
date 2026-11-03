//! Stage-zero storage and I/O for the same compiler authored in Mira.
use super::*;
pub(super) struct Workspace {
    pub(super) arena: crate::data_runtime::Arena,
    pub(super) input: i64,
    pub(super) state: i64,
    pub(super) symbols: i64,
    pub(super) parameters: i64,
    pub(super) work: i64,
    pub(super) lexer: i64,
    pub(super) literal: i64,
    pub(super) value: i64,
    pub(super) nodes: i64,
    pub(super) emission: i64,
    pub(super) body: i64,
    pub(super) output: i64,
    #[cfg(test)]
    pub(super) image: i64,
    pub(super) fixups: i64,
    pub(super) relocations: i64,
}
impl Workspace {
    pub(super) fn new() -> Result<Self> {
        let f = frontend()?;
        let types = &crate::bootstrap_compiler::library()?.program.global_types;
        let mut arena = crate::data_runtime::Arena::default();
        arena.limit = 32 * 1024 * 1024;
        let mut allocate = |name: &str| {
            let d = types
                .iter()
                .find(|d| d.name == name)
                .ok_or_else(|| format!("compiler type {name} missing"))?;
            arena.allocate(d.id, vec![0; d.len()])
        };
        let input = allocate("ScalarSource")?;
        let state = allocate("ModuleState")?;
        let symbols = allocate("ModuleSymbols")?;
        let parameters = allocate("ScalarParameters")?;
        let work = allocate("ScalarWork")?;
        let window = allocate("LexBytes")?;
        let lex_state = allocate("LexState")?;
        let nodes = allocate("ScalarNodes")?;
        let emission = allocate("ScalarCode")?;
        let body = allocate("ScalarBytes")?;
        let output = allocate("ScalarBytes")?;
        #[cfg(test)]
        let image = allocate("ScalarBytes")?;
        let fixups = allocate("ScalarFixups")?;
        let relocations = allocate("ScalarFixups")?;
        let literal = allocate("LiteralBytes")?;
        let value = allocate("LiteralValue")?;
        let lexer = arena.allocate(f.lexer, vec![window, lex_state])?;
        Ok(Self {
            arena,
            input,
            state,
            symbols,
            parameters,
            work,
            lexer,
            literal,
            value,
            nodes,
            emission,
            body,
            output,
            #[cfg(test)]
            image,
            fixups,
            relocations,
        })
    }
    pub(super) fn field(&self, name: &str) -> Result<i64> {
        let types = &crate::bootstrap_compiler::library()?.program.global_types;
        let definition = types
            .iter()
            .find(|d| d.name == "ModuleState")
            .ok_or("compiler state missing")?;
        self.arena.get(self.state, super::field(definition, name)?)
    }
    pub(super) fn call(&mut self, name: &str, args: &[i64]) -> Result<i64> {
        let f = frontend()?;
        let library = crate::bootstrap_compiler::library()?;
        let id = library
            .program
            .code_modules
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| format!("compiler operation {name} missing"))?
            .id;
        f.code.call_compiler(id, args, &mut self.arena)
    }
    #[cfg(test)]
    pub(super) fn compile(&mut self, source: &str, entry: i64) -> Result<Vec<u8>> {
        self.compile_with_budget(source, entry, 10000)
    }
    #[cfg(test)]
    pub(super) fn compile_with_budget(
        &mut self,
        source: &str,
        entry: i64,
        budget: i64,
    ) -> Result<Vec<u8>> {
        self.link(source, entry, budget)?;
        self.package()
    }
    pub(super) fn link(&mut self, source: &str, entry: i64, budget: i64) -> Result<()> {
        let capacity = self.arena.object(self.input)?.words.len();
        if source.len() > capacity {
            return Err(format!("compiler source exceeds {capacity}-byte workspace"));
        }
        for (i, b) in source.bytes().enumerate() {
            self.arena.set(self.input, i, i64::from(b))?;
        }
        self.call(
            "module_index_begin",
            &[
                self.input,
                source.len() as i64,
                entry,
                self.state,
                self.symbols,
                self.work,
                self.lexer,
                self.literal,
                self.value,
                self.parameters,
            ],
        )?;
        while self.field("error")? == 0 && self.field("phase")? < 3 {
            self.call(
                "module_index_step",
                &[
                    self.input,
                    self.state,
                    self.symbols,
                    self.work,
                    self.lexer,
                    self.literal,
                    self.value,
                    self.parameters,
                ],
            )?;
        }
        if self.field("error")? == 0 {
            self.call(
                "module_prepare",
                &[self.state, self.symbols, self.parameters],
            )?;
            while self.field("error")? == 0 && self.field("emitted")? < self.field("count")? {
                self.call(
                    "module_emit_next",
                    &[
                        self.input,
                        self.state,
                        self.symbols,
                        self.parameters,
                        self.work,
                        self.lexer,
                        self.literal,
                        self.value,
                        self.nodes,
                        self.emission,
                        self.body,
                        self.output,
                        self.fixups,
                        self.relocations,
                    ],
                )?;
            }
        }
        if self.field("error")? == 0 {
            self.call(
                "module_link",
                &[
                    self.state,
                    self.symbols,
                    self.output,
                    self.relocations,
                    budget,
                ],
            )?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn package(&mut self) -> Result<Vec<u8>> {
        let size = self.call(
            "module_elf",
            &[
                self.state,
                self.symbols,
                self.emission,
                self.body,
                self.output,
                self.image,
            ],
        )?;
        (0..size as usize)
            .map(|i| {
                u8::try_from(self.arena.get(self.image, i)?)
                    .map_err(|_| "compiler emitted a non-byte".to_string())
            })
            .collect()
    }
}

#[derive(serde::Serialize)]
pub(super) struct Export {
    pub id: Id,
    pub offset: usize,
}
pub(super) struct BuiltCompiler {
    pub source: &'static str,
    pub code: Vec<u8>,
    pub exports: std::collections::BTreeMap<String, Export>,
}
pub(super) fn compile_own() -> Result<BuiltCompiler> {
    let source = crate::bootstrap_compiler::library()?
        .canonical_source
        .as_str();
    let mut work = Workspace::new()?;
    work.link(source, 258, 200_000_000)?;
    let error = work.field("error")?;
    if error != 0 {
        return Err(format!(
            "Mira compiler rejected its source: error {error} at byte {}",
            work.field("error_offset")?
        ));
    }
    let size = usize::try_from(work.field("size")?).map_err(|_| "negative compiler image size")?;
    let code = (0..size)
        .map(|i| {
            u8::try_from(work.arena.get(work.output, i)?)
                .map_err(|_| "compiler emitted a non-byte".to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut exports = std::collections::BTreeMap::new();
    for index in 0..work.field("count")? as usize {
        let row = index * 128;
        let length = usize::try_from(work.arena.get(work.symbols, row)?)
            .map_err(|_| "invalid procedure name")?;
        if length == 0 || length > 64 {
            return Err("invalid compiler procedure name".into());
        }
        let name = (0..length)
            .map(|i| {
                u8::try_from(work.arena.get(work.symbols, row + 32 + i)?)
                    .map_err(|_| "invalid procedure name byte".to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        let name = String::from_utf8(name).map_err(|_| "invalid compiler procedure name")?;
        let id = Id::try_from(work.arena.get(work.symbols, row + 19)?)
            .map_err(|_| "invalid procedure identity")?;
        let offset = usize::try_from(work.arena.get(work.symbols, row + 101)?)
            .map_err(|_| "invalid procedure entry")?;
        if offset >= code.len() {
            return Err("compiler procedure exceeds image".into());
        }
        exports.insert(name, Export { id, offset });
    }
    Ok(BuiltCompiler {
        source,
        code,
        exports,
    })
}
pub(crate) fn write_compiler(directory: &std::path::Path) -> Result<(usize, usize)> {
    let descriptor = directory.join("index");
    if descriptor.exists() {
        let size = std::fs::metadata(&descriptor)
            .map_err(|e| e.to_string())?
            .len();
        if size > 128 * 1024 {
            return Err("compiler destination contains a foreign index".into());
        }
        let bytes = std::fs::read(&descriptor).map_err(|e| e.to_string())?;
        let old: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| "compiler destination contains a foreign index")?;
        if old.get("format").and_then(|v| v.as_str()) != Some("mira-native-compiler-v1") {
            return Err("compiler destination belongs to another project or artifact".into());
        }
    } else if directory.join("source").exists() || directory.join("code").exists() {
        return Err("compiler destination contains files not owned by this builder".into());
    }
    let compiled = compile_own()?;
    let index = serde_json::json!({
        "format": "mira-native-compiler-v1",
        "input_words": 16,
        "error_offset": 128,
        "source_blake3": blake3::hash(compiled.source.as_bytes()).to_hex().to_string(),
        "code_blake3": blake3::hash(&compiled.code).to_hex().to_string(),
        "exports": &compiled.exports,
    });
    let index = serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    // Publish the checksummed descriptor last; no reader can accept a mixed image.
    for (name, bytes) in [
        ("code", compiled.code.as_slice()),
        ("source", compiled.source.as_bytes()),
        ("index", index.as_slice()),
    ] {
        let temporary = directory.join(format!(".{name}-{}", std::process::id()));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&temporary, directory.join(name)).map_err(|e| e.to_string())?;
    }
    Ok((compiled.exports.len(), compiled.code.len()))
}
