//! Bounded, thread-local reuse of compiler results. Runtime state never enters keys.
//! Exact serialized keys are retained; a hash collision cannot select another program.
use mira_core::{code::CodeModule, data, Id, Program, Result, Space};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

const MAX_ENTRIES: usize = 8;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROCEDURES: usize = 256;
const MAX_PROCEDURE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub source_hits: u64,
    pub source_misses: u64,
    pub source_entries: usize,
    pub source_bytes: usize,
    pub scalar_code_hits: u64,
    pub scalar_code_misses: u64,
    pub executable_hits: u64,
    pub executable_misses: u64,
    pub validation_hits: u64,
    pub validation_misses: u64,
    pub validation_entries: usize,
    pub validation_bytes: usize,
    pub procedure_hits: u64,
    pub procedure_misses: u64,
    pub executable_entries: usize,
    pub executable_bytes: usize,
    pub procedure_entries: usize,
    pub procedure_bytes: usize,
}
struct Entry<T> {
    key: Vec<u8>,
    value: T,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    sources: VecDeque<Entry<crate::frontend::ParsedSource>>,
    executables: VecDeque<Entry<Rc<crate::composition::Executable>>>,
    procedures: VecDeque<Entry<CodeModule>>,
    validations: VecDeque<Vec<u8>>,
    scalar: VecDeque<Entry<Vec<mira_core::code::Statement>>>,
    scalar_code: VecDeque<Entry<crate::bootstrap_scalar::Template>>,
    stats: Stats,
}
thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }
/// Counters belong to this host thread; retained byte counts measure serialized
/// compiler data, excluding allocator/container overhead and live Rc users.
pub fn stats() -> Stats {
    CACHE.with(|c| c.borrow().stats)
}
/// Release cache-owned references and reset counters on the calling thread.
pub fn clear() {
    CACHE.with(|c| *c.borrow_mut() = Cache::default());
}
fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| format!("compile cache key: {e}"))
}
fn canonical(program: &Program) -> Program {
    fn positions(space: &mut Space) {
        space.position = [0.0; 3];
        for cell in &mut space.cells {
            cell.position = [0.0; 3];
            if let Some(inner) = &mut cell.space {
                positions(inner);
            }
        }
    }
    let mut program = program.clone();
    positions(&mut program.root);
    program.next_identity = 1;
    program.next_identity = program.next_id().expect("validated program identity range");
    program
}
pub(crate) fn executable(
    program: &Program,
    updates: &[(Id, Id)],
) -> Result<Rc<crate::composition::Executable>> {
    let mut updates = updates.to_vec();
    updates.sort_unstable();
    updates.dedup();
    let key = encode(&(canonical(program), &updates))?;
    if let Some(hit) = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let index = c.executables.iter().position(|e| e.key == key);
        if let Some(index) = index {
            let entry = c.executables.remove(index).unwrap();
            let value = entry.value.clone();
            c.executables.push_back(entry);
            c.stats.executable_hits = c.stats.executable_hits.saturating_add(1);
            Some(value)
        } else {
            c.stats.executable_misses = c.stats.executable_misses.saturating_add(1);
            None
        }
    }) {
        return Ok(hit);
    }
    // Compilation can use the procedure cache, so never hold CACHE's borrow here.
    let value = Rc::new(crate::composition::compile_host(program, &updates)?);
    let bytes = key.len().saturating_add(
        encode(&(
            &value.definitions,
            value.bound_modules.iter().collect::<Vec<_>>(),
            value.entries.iter().collect::<Vec<_>>(),
            value.graph_entries.iter().collect::<Vec<_>>(),
            value.graph_slots.iter().collect::<Vec<_>>(),
            &value.host_entries,
            &value.data_types,
        ))?
        .len(),
    );
    if bytes <= MAX_BYTES {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            while c.executables.len() >= MAX_ENTRIES
                || c.stats.executable_bytes.saturating_add(bytes) > MAX_BYTES
            {
                if let Some(old) = c.executables.pop_front() {
                    c.stats.executable_bytes -= old.bytes;
                } else {
                    break;
                }
            }
            c.stats.executable_bytes += bytes;
            c.executables.push_back(Entry {
                key,
                value: value.clone(),
                bytes,
            });
            c.stats.executable_entries = c.executables.len();
        });
    }
    Ok(value)
}
/// Private procedure labels are allocation details. Normalize only exact #<id>
/// labels, never type/cell identities, integer payloads or graph binding names.
fn private_labels(
    signatures: &[CodeModule],
    reverse: bool,
) -> std::collections::BTreeMap<String, String> {
    signatures
        .iter()
        .filter(|d| {
            d.name
                .strip_prefix('#')
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        })
        .enumerate()
        .map(|(rank, d)| {
            let canonical = format!("#{}", rank + 1);
            if reverse {
                (canonical, d.name.clone())
            } else {
                (d.name.clone(), canonical)
            }
        })
        .collect()
}
fn rebind(procedure: &mut CodeModule, labels: &std::collections::BTreeMap<String, String>) {
    use mira_core::code::{Expr, Statement};
    fn name(name: &mut String, labels: &std::collections::BTreeMap<String, String>) {
        if let Some(replacement) = labels.get(name) {
            *name = replacement.clone();
        }
    }
    fn expr(e: &mut Expr, labels: &std::collections::BTreeMap<String, String>) {
        match e {
            Expr::Name(n) => name(n, labels),
            Expr::Call(n, args) => {
                name(n, labels);
                for arg in args {
                    expr(arg, labels);
                }
            }
            Expr::Unary(_, a) | Expr::View(_, a) | Expr::Field(a, _) => expr(a, labels),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => {
                expr(a, labels);
                expr(b, labels);
            }
            Expr::Choose(a, b, c) => {
                expr(a, labels);
                expr(b, labels);
                expr(c, labels);
            }
            Expr::Construct(_, args) => {
                for arg in args {
                    expr(arg, labels);
                }
            }
            Expr::Hole(_) | Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Unit => {}
        }
    }
    fn body(statements: &mut [Statement], labels: &std::collections::BTreeMap<String, String>) {
        for statement in statements {
            match statement {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e, labels),
                Statement::Set { target, value, .. } => {
                    expr(target, labels);
                    expr(value, labels);
                }
                Statement::If { condition, yes, no } => {
                    expr(condition, labels);
                    body(yes, labels);
                    body(no, labels);
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expr(condition, labels);
                    body(inner, labels);
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expr(start, labels);
                    expr(end, labels);
                    body(inner, labels);
                }
            }
        }
    }
    if let Some(replacement) = labels.get(&procedure.name) {
        procedure.id = replacement[1..]
            .parse()
            .expect("generated private procedure label");
        procedure.name = replacement.clone();
    }
    body(&mut procedure.body, labels);
    for local in &mut procedure.locals {
        rebind(local, labels);
    }
}
/// Numeric legalization depends on signatures/types, not unrelated bodies or
/// allocator high-water marks. Rebinding restores the current executable's IDs.
pub(crate) fn numeric_key(
    procedure: &CodeModule,
    signatures: &[CodeModule],
    types: &[data::Definition],
) -> Result<Vec<u8>> {
    let labels = private_labels(signatures, false);
    let mut procedure = procedure.clone();
    rebind(&mut procedure, &labels);
    let signatures: Vec<_> = signatures
        .iter()
        .map(|d| {
            (
                labels.get(&d.name).unwrap_or(&d.name),
                &d.parameters,
                d.output,
            )
        })
        .collect();
    encode(&(procedure, signatures, types))
}
pub(crate) fn numeric_get(key: &[u8], signatures: &[CodeModule]) -> Option<CodeModule> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(index) = c.procedures.iter().position(|e| e.key == key) {
            let entry = c.procedures.remove(index).unwrap();
            let mut value = entry.value.clone();
            rebind(&mut value, &private_labels(signatures, true));
            c.procedures.push_back(entry);
            c.stats.procedure_hits = c.stats.procedure_hits.saturating_add(1);
            Some(value)
        } else {
            c.stats.procedure_misses = c.stats.procedure_misses.saturating_add(1);
            None
        }
    })
}
pub(crate) fn numeric_insert(
    key: Vec<u8>,
    value: &CodeModule,
    signatures: &[CodeModule],
) -> Result<()> {
    let mut value = value.clone();
    rebind(&mut value, &private_labels(signatures, false));
    let bytes = key.len().saturating_add(encode(&value)?.len());
    if bytes > MAX_PROCEDURE_BYTES {
        return Ok(());
    }
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        while c.procedures.len() >= MAX_PROCEDURES
            || c.stats.procedure_bytes.saturating_add(bytes) > MAX_PROCEDURE_BYTES
        {
            if let Some(old) = c.procedures.pop_front() {
                c.stats.procedure_bytes -= old.bytes;
            } else {
                break;
            }
        }
        c.stats.procedure_bytes += bytes;
        c.procedures.push_back(Entry {
            key,
            value: value.clone(),
            bytes,
        });
        c.stats.procedure_entries = c.procedures.len();
    });
    Ok(())
}

/// Only successful source/readiness checks are cached. The caller must validate
/// live System state on every invocation before entering this function.
pub(crate) fn validated(
    program: &Program,
    updates: &[(Id, Id)],
    ready: bool,
    validate: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut updates = updates.to_vec();
    updates.sort_unstable();
    updates.dedup();
    let key = encode(&(canonical(program), updates, ready))?;
    if CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(index) = c.validations.iter().position(|old| old == &key) {
            let entry = c.validations.remove(index).unwrap();
            c.validations.push_back(entry);
            c.stats.validation_hits = c.stats.validation_hits.saturating_add(1);
            true
        } else {
            c.stats.validation_misses = c.stats.validation_misses.saturating_add(1);
            false
        }
    }) {
        return Ok(());
    }
    validate()?;
    if key.len() <= MAX_BYTES {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            while c.validations.len() >= MAX_ENTRIES
                || c.stats.validation_bytes.saturating_add(key.len()) > MAX_BYTES
            {
                if let Some(old) = c.validations.pop_front() {
                    c.stats.validation_bytes -= old.len();
                } else {
                    break;
                }
            }
            c.stats.validation_bytes += key.len();
            c.validations.push_back(key);
            c.stats.validation_entries = c.validations.len();
        });
    }
    Ok(())
}

/// Shared stage-1 parse results use the existing bounded compiler cache.
pub(crate) fn scalar(
    source: &str,
    parameters: &[mira_core::code::Parameter],
    build: impl FnOnce() -> Result<Vec<mira_core::code::Statement>>,
) -> Result<Vec<mira_core::code::Statement>> {
    let key = encode(&(source, parameters))?;
    if let Some(value) = CACHE.with(|cache| {
        cache
            .borrow()
            .scalar
            .iter()
            .find(|v| v.key == key)
            .map(|v| v.value.clone())
    }) {
        return Ok(value);
    }
    let value = build()?;
    let bytes = key.len() + encode(&value)?.len();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        while cache.scalar.len() >= 256
            || cache.scalar.iter().map(|v| v.bytes).sum::<usize>() + bytes > 256 * 1024
        {
            if cache.scalar.pop_front().is_none() {
                break;
            }
        }
        if bytes <= 256 * 1024 {
            cache.scalar.push_back(Entry {
                key,
                value: value.clone(),
                bytes,
            });
        }
    });
    Ok(value)
}

/// Emission is pure; persistent state and machine addresses are not cache keys.
pub(crate) fn scalar_code(
    source: &str,
    profile: i64,
    symbols: &[crate::bootstrap_scalar::Symbol],
    build: impl FnOnce() -> Result<crate::bootstrap_scalar::Template>,
) -> Result<crate::bootstrap_scalar::Template> {
    let shapes: Vec<_> = symbols.iter().map(|s| (&s.inputs, s.output)).collect();
    let key = encode(&(source, profile, shapes))?;
    if let Some(value) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let value = cache
            .scalar_code
            .iter()
            .find(|v| v.key == key)
            .map(|v| v.value.clone());
        if value.is_some() {
            cache.stats.scalar_code_hits += 1;
        } else {
            cache.stats.scalar_code_misses += 1;
        }
        value
    }) {
        return Ok(value);
    }
    let value = build()?;
    let bytes = key.len() + value.bytes.len() + value.calls.len() * 16;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        while cache.scalar_code.len() >= 128
            || cache.scalar_code.iter().map(|v| v.bytes).sum::<usize>() + bytes > 512 * 1024
        {
            if cache.scalar_code.pop_front().is_none() {
                break;
            }
        }
        if bytes <= 512 * 1024 {
            cache.scalar_code.push_back(Entry {
                key,
                value: value.clone(),
                bytes,
            });
        }
    });
    Ok(value)
}

/// Reuse the same checked Program and source sites for identical source bytes.
/// Modes are part of the exact key; draft/seed results cannot satisfy a normal
/// parse. Callers receive independent trees and errors are never retained.
pub(crate) fn source(
    source: &str,
    checked: bool,
    seed: bool,
    build: impl FnOnce() -> Result<crate::frontend::ParsedSource>,
) -> Result<crate::frontend::ParsedSource> {
    let mut key = Vec::with_capacity(source.len() + 2);
    key.extend([u8::from(checked), u8::from(seed)]);
    key.extend(source.as_bytes());
    if let Some(value) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.sources.iter().position(|e| e.key == key) {
            let entry = cache.sources.remove(index).unwrap();
            let value = entry.value.clone();
            cache.sources.push_back(entry);
            cache.stats.source_hits += 1;
            Some(value)
        } else {
            cache.stats.source_misses += 1;
            None
        }
    }) {
        return Ok(value);
    }
    let value = build()?;
    let bytes = key.len() + encode(&value)?.len();
    if bytes <= MAX_BYTES {
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            while cache.sources.len() >= MAX_ENTRIES
                || cache.sources.iter().map(|e| e.bytes).sum::<usize>() + bytes > MAX_BYTES
            {
                if cache.sources.pop_front().is_none() {
                    break;
                }
            }
            cache.sources.push_back(Entry {
                key,
                value: value.clone(),
                bytes,
            });
            cache.stats.source_entries = cache.sources.len();
            cache.stats.source_bytes = cache.sources.iter().map(|e| e.bytes).sum();
        });
    }
    Ok(value)
}
#[cfg(test)]
mod source_tests {
    #[test]
    fn repeated_source_reads_preserve_identity_sites_and_checker_modes() {
        super::clear();
        let source = include_str!("../../../language/examples/bootstrap-module");
        let original = crate::syntax::parse(source).unwrap();
        let mut edited = original.clone();
        edited.code_modules.clear();
        let before = super::stats();
        assert_eq!(crate::syntax::parse(source).unwrap(), original);
        assert!(super::stats().source_hits > before.source_hits);
        assert_ne!(edited, crate::syntax::parse(source).unwrap());
        let draft = include_str!("../../../language/examples/incomplete");
        assert!(crate::syntax::parse_draft(draft).is_ok());
        assert!(crate::syntax::parse(draft).is_err());
    }
}
