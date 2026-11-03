//! The compiler is an ordinary nested MiraOS source object. Stage zero loads
//! precisely that object, rather than maintaining a second compiler library.
use mira_core::{Program, Result, code::CodeModule};
use std::{collections::BTreeMap, sync::OnceLock};

pub(crate) struct Library {
    pub program: Program,
    pub code: crate::behavior::Prepared,
    pub source: String,
    pub canonical_source: String,
}
pub(crate) fn library() -> Result<&'static Library> {
    static LIBRARY: OnceLock<Result<Library>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let source = crate::source_tree::embedded_selected(
                include_str!("../../../projects/MiraOS/index"),
                &[
                    (
                        "compiler-lexer",
                        include_str!("../../../projects/MiraOS/compiler-lexer"),
                    ),
                    (
                        "compiler-parser",
                        include_str!("../../../projects/MiraOS/compiler-parser"),
                    ),
                    (
                        "compiler-types",
                        include_str!("../../../projects/MiraOS/compiler-types"),
                    ),
                    (
                        "compiler-x86",
                        include_str!("../../../projects/MiraOS/compiler-x86"),
                    ),
                    (
                        "compiler-module",
                        include_str!("../../../projects/MiraOS/compiler-module"),
                    ),
                    (
                        "compiler-data",
                        include_str!("../../../projects/MiraOS/compiler-data"),
                    ),
                ],
                258,
            )?;
            let canonical_source = source.clone();
            let mut program = crate::frontend::parse_seed(&source)?;
            // This source object has a single lexical namespace and unique internal
            // operation names. The bootstrap view exposes its definitions to the VM;
            // it preserves their IDs, bodies and types, and creates no runtime cells.
            let types = program.data_types();
            fn expose(mut definition: CodeModule, output: &mut Vec<CodeModule>) {
                for child in std::mem::take(&mut definition.locals) {
                    expose(child, output);
                }
                definition.data_types.clear();
                output.push(definition);
            }
            let mut definitions = Vec::new();
            for definition in std::mem::take(&mut program.code_modules) {
                expose(definition, &mut definitions);
            }
            program.global_types = types;
            program.code_modules = definitions;
            let names = program
                .code_modules
                .iter()
                .map(|d| (d.name.clone(), d.id))
                .collect::<BTreeMap<_, _>>();
            if names.len() != program.code_modules.len() {
                return Err("compiler bootstrap view requires unique operation names".into());
            }
            let formatted = crate::syntax::format(&program);
            let end = formatted
                .rfind("\nspace ")
                .ok_or("compiler source root missing")?;
            let source = formatted[..end]
                .lines()
                .skip_while(|line| line.starts_with("identities "))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let code = crate::behavior::Prepared::new(&program)?;
            Ok(Library {
                program,
                code,
                source,
                canonical_source,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}
