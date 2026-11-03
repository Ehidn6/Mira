//! Stage-1 scalar parsing is authored in Mira. The bootstrap compiler and the
//! native compiler consume the same portable node representation.
use mira_core::{code::*, data, Id, Result, Type};
use std::sync::OnceLock;

struct Frontend {
    code: &'static crate::behavior::Prepared,
    entry: Id,
    input: Id,
    work: data::Definition,
    lexer: Id,
    lexer_state: Id,
    lexer_bytes: Id,
    literal: Id,
    value: Id,
    nodes: Id,
    parameters: Id,
    check: Id,
    emit: Id,
    elf: Id,
    output: Id,
    fixups: Id,
    emission: data::Definition,
    #[cfg(test)]
    names: std::collections::BTreeMap<Id, String>,
}
fn frontend() -> Result<&'static Frontend> {
    static FRONTEND: OnceLock<Result<Frontend>> = OnceLock::new();
    FRONTEND
        .get_or_init(|| {
            let library = crate::bootstrap_compiler::library()?;
            let p = &library.program;
            let ty = |name: &str| {
                p.global_types
                    .iter()
                    .find(|t| t.name == name)
                    .cloned()
                    .ok_or_else(|| format!("scalar compiler type {name} missing"))
            };
            Ok(Frontend {
                entry: p
                    .code_modules
                    .iter()
                    .find(|m| m.name == "scalar_parse")
                    .ok_or("Mira scalar frontend missing")?
                    .id,
                input: ty("ScalarSource")?.id,
                work: ty("ScalarWork")?,
                lexer: ty("ScalarLexer")?.id,
                lexer_state: ty("LexState")?.id,
                lexer_bytes: ty("LexBytes")?.id,
                literal: ty("LiteralBytes")?.id,
                value: ty("LiteralValue")?.id,
                nodes: ty("ScalarNodes")?.id,
                parameters: ty("ScalarParameters")?.id,
                check: p
                    .code_modules
                    .iter()
                    .find(|m| m.name == "scalar_check")
                    .ok_or("Mira scalar checker missing")?
                    .id,
                emit: p
                    .code_modules
                    .iter()
                    .find(|m| m.name == "scalar_emit")
                    .ok_or("Mira scalar emitter missing")?
                    .id,
                elf: p
                    .code_modules
                    .iter()
                    .find(|m| m.name == "elf_header")
                    .ok_or("Mira ELF writer missing")?
                    .id,
                output: ty("ScalarBytes")?.id,
                fixups: ty("ScalarFixups")?.id,
                emission: ty("ScalarCode")?,
                #[cfg(test)]
                names: mira_core::code::symbols(&p.code_modules)
                    .into_iter()
                    .map(|(n, d)| (d.id, n))
                    .collect(),
                code: &library.code,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}
pub(crate) fn elf_header(size: usize, entry: u64) -> Result<Vec<u8>> {
    let f = frontend()?;
    let mut arena = crate::data_runtime::Arena::default();
    let output = arena.allocate(f.output, vec![0; 65536])?;
    let size = i64::try_from(size).map_err(|_| "ELF size exceeds compiler word")?;
    let entry = i64::try_from(entry).map_err(|_| "ELF entry exceeds compiler word")?;
    if f.code
        .call_compiler(f.elf, &[output, size, entry], &mut arena)?
        != 4096
    {
        return Err("Mira ELF writer rejected the image layout".into());
    }
    (0..4096)
        .map(|i| {
            u8::try_from(arena.get(output, i)?)
                .map_err(|_| "Mira ELF writer returned a non-byte".into())
        })
        .collect()
}

fn supported(e: &Expr, parameters: &[Parameter], depth: usize, calls: bool) -> bool {
    if depth > if calls { 64 } else { 12 } {
        return false;
    }
    match e {
        Expr::Int(_) | Expr::Bool(_) | Expr::Unit => true,
        Expr::Name(_) => true,
        Expr::Unary(_, e) => supported(e, parameters, depth + 1, calls),
        Expr::Binary(op, a, b) => {
            !matches!(
                op,
                Binary::FAdd
                    | Binary::FSub
                    | Binary::FMul
                    | Binary::FDiv
                    | Binary::FEq
                    | Binary::FNe
                    | Binary::FLt
                    | Binary::FLe
                    | Binary::FGt
                    | Binary::FGe
            ) && supported(a, parameters, depth + 1, calls)
                && supported(b, parameters, depth + 1, calls)
        }
        Expr::Choose(a, b, c) => [a, b, c]
            .iter()
            .all(|e| supported(e, parameters, depth + 1, calls)),
        Expr::Call(_, args) => {
            calls
                && args.len() <= 16
                && args
                    .iter()
                    .all(|e| supported(e, parameters, depth + 1, calls))
        }
        _ => false,
    }
}
pub(crate) fn eligible(d: &CodeModule) -> bool {
    admitted(d, false)
}
fn admitted(d: &CodeModule, calls: bool) -> bool {
    fn statements(body: &[Statement]) -> usize {
        body.iter()
            .map(|s| {
                1 + match s {
                    Statement::If { yes, no, .. } => statements(yes) + statements(no),
                    Statement::While { body, .. } | Statement::For { body, .. } => statements(body),
                    _ => 0,
                }
            })
            .sum()
    }

    // Profile bounds are admission limits, not language errors. Count lexical
    // declarations before invoking the bounded compiler, including each for index.
    fn locals(body: &[Statement], names: &mut std::collections::BTreeSet<String>) -> usize {
        body.iter()
            .map(|statement| match statement {
                Statement::Bind { name, .. } => usize::from(names.insert(name.clone())),
                Statement::If { yes, no, .. } => {
                    locals(yes, &mut names.clone()) + locals(no, &mut names.clone())
                }
                Statement::While { body, .. } => locals(body, &mut names.clone()),
                Statement::For { name, body, .. } => {
                    let mut inner = names.clone();
                    inner.insert(name.clone());
                    1 + locals(body, &mut inner)
                }
                _ => 0,
            })
            .sum()
    }

    fn block(body: &[Statement], parameters: &[Parameter], depth: usize, calls: bool) -> bool {
        depth <= if calls { 32 } else { 8 }
            && body.iter().all(|stmt| match stmt {
                Statement::Expr(e) => supported(e, parameters, 0, calls),
                Statement::Bind { ty, value, .. } => {
                    ty.is_none_or(|t| {
                        matches!(t, Type::I64 | Type::Bool | Type::Unit | Type::Infer)
                    }) && supported(value, parameters, 0, calls)
                }
                Statement::While { condition, body } => {
                    supported(condition, parameters, 0, calls)
                        && block(body, parameters, depth + 1, calls)
                }
                Statement::For {
                    start, end, body, ..
                } => {
                    supported(start, parameters, 0, calls)
                        && supported(end, parameters, 0, calls)
                        && block(body, parameters, depth + 1, calls)
                }
                Statement::If { condition, yes, no } => {
                    supported(condition, parameters, 0, calls)
                        && block(yes, parameters, depth + 1, calls)
                        && block(no, parameters, depth + 1, calls)
                }
                _ => false,
            })
    }
    // Keep wider active programs on the general backend until this bounded
    // source stage supports them; do not turn its limits into language limits.
    locals(
        &d.body,
        &mut d.parameters.iter().map(|p| p.name.clone()).collect(),
    ) <= 48
        && statements(&d.body) <= if calls { 512 } else { 32 }
        && mira_core::code::expressions(d)
            .is_ok_and(|nodes| nodes.len() <= if calls { 2000 } else { 200 })
        && !d.compile_time
        && !d.output_ports
        && d.data_types.is_empty()
        && d.locals.is_empty()
        && matches!(d.output, Type::I64 | Type::Bool | Type::Unit)
        && d.parameters.len() <= 16
        && d.parameters
            .iter()
            .all(|p| matches!(p.ty, Type::I64 | Type::Bool))
        && block(&d.body, &d.parameters, 0, calls)
}
pub(crate) fn parse(source: &str, parameters: &[Parameter]) -> Result<Vec<Statement>> {
    crate::compilation_cache::scalar(source, parameters, || parse_uncached(source, parameters))
}
struct Stage {
    arena: crate::data_runtime::Arena,
    work: i64,
    nodes: i64,
    parameters: i64,
}
fn parse_stage(source: &str, symbols: &[Symbol]) -> Result<Stage> {
    if source.len() > 16384 {
        return Err("scalar source exceeds 16384 bytes".into());
    }
    let f = frontend()?;
    let mut arena = crate::data_runtime::Arena::default();
    // Compiler work owns a separate bounded arena; this is not the execution
    // budget of user programs or the native kernel's activation arena.
    arena.limit = 4 * 1024 * 1024;
    let mut bytes = vec![0; 16384];
    for (i, b) in source.bytes().enumerate() {
        bytes[i] = i64::from(b);
    }
    let input = arena.allocate(f.input, bytes)?;
    let work = arena.allocate(f.work.id, vec![0; f.work.len()])?;
    let state = arena.allocate(f.lexer_state, vec![0; 3])?;
    let window = arena.allocate(f.lexer_bytes, vec![0; 4096])?;
    let lexer = arena.allocate(f.lexer, vec![window, state])?;
    let literal = arena.allocate(f.literal, vec![0; 128])?;
    let value = arena.allocate(f.value, vec![0; 1])?;
    let nodes = arena.allocate(f.nodes, vec![0; 24576])?;
    let parameter_buffer = arena.allocate(f.parameters, vec![0; 20832])?;
    for (index, symbol) in symbols.iter().enumerate() {
        let base = 320 + index * 96;
        let name = format!("callee{index}");
        arena.set(parameter_buffer, base, name.len() as i64)?;
        arena.set(parameter_buffer, base + 1, type_tag(symbol.output)?)?;
        arena.set(parameter_buffer, base + 2, symbol.inputs.len() as i64)?;
        for (i, ty) in symbol.inputs.iter().enumerate() {
            arena.set(parameter_buffer, base + 3 + i, type_tag(*ty)?)?;
        }
        for (i, b) in name.bytes().enumerate() {
            arena.set(parameter_buffer, base + 32 + i, b as i64)?;
        }
    }
    let mut error = f.code.call_compiler(
        f.entry,
        &[
            input,
            source.len() as i64,
            work,
            lexer,
            literal,
            value,
            nodes,
            parameter_buffer,
        ],
        &mut arena,
    )?;
    if error == 0 {
        error = f
            .code
            .call_compiler(f.check, &[work, nodes, parameter_buffer], &mut arena)?;
    }
    check_error(f, &arena, work, error)?;
    Ok(Stage {
        arena,
        work,
        nodes,
        parameters: parameter_buffer,
    })
}
fn field(definition: &data::Definition, name: &str) -> Result<usize> {
    let data::Kind::Record(fields) = &definition.kind else {
        return Err("compiler work must be a record".into());
    };
    fields
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| format!("compiler field {name} missing"))
}
fn check_error(
    f: &Frontend,
    arena: &crate::data_runtime::Arena,
    work: i64,
    error: i64,
) -> Result<()> {
    if error != 0 {
        let description = match error {
            1 => "invalid syntax or indentation",
            2 => "unknown/unsupported scalar name or type",
            3 => "expected an expression",
            4 => "scalar compiler work budget exceeded",
            5 => "invalid or out-of-range integer",
            6 => "incompatible scalar types",
            7 => "ambiguous type: add a constraint or annotation",
            8 => "invalid lowered code structure",
            9 => "invalid binding, scope or write to an immutable value",
            _ => "unknown compiler error",
        };
        return Err(format!(
            "Mira scalar frontend: error {error} at byte {}: {description} (tokens {}, nodes {})",
            arena.get(work, field(&f.work, "error_offset")?)?,
            arena.get(work, field(&f.work, "tokens")?)?,
            arena.get(work, field(&f.work, "count")?)?
        ));
    }
    Ok(())
}
fn parse_uncached(source: &str, parameters: &[Parameter]) -> Result<Vec<Statement>> {
    let Stage {
        arena,
        work,
        nodes,
        parameters: parameter_buffer,
    } = parse_stage(source, &[])?;
    let f = frontend()?;
    let field = |name| field(&f.work, name);
    if arena.get(work, field("arity")?)? != parameters.len() as i64 {
        return Err("scalar input count mismatch".into());
    }
    for (index, p) in parameters.iter().enumerate() {
        let ty = arena.get(parameter_buffer, index * 5 + 2)?;
        if !matches!((ty, p.ty), (1, Type::I64) | (2, Type::Bool)) {
            return Err("scalar input type mismatch".into());
        }
    }
    let binding_count = arena.get(work, field("bindings")?)? as usize;
    let mut bindings = Vec::new();
    for index in 0..binding_count {
        let start = arena.get(parameter_buffer, index * 5)? as usize;
        let end = arena.get(parameter_buffer, index * 5 + 1)? as usize;
        bindings.push(
            source
                .get(start..end)
                .ok_or("invalid compiler binding span")?
                .to_string(),
        );
    }
    let count = arena.get(work, field("count")?)?;
    let root = arena.get(work, field("root")?)?;
    fn expression(
        arena: &crate::data_runtime::Arena,
        nodes: i64,
        count: i64,
        index: i64,
        parameters: &[String],
        depth: usize,
    ) -> Result<Expr> {
        if index < 0 || index >= count || depth > 256 {
            return Err("invalid scalar compiler node".into());
        }
        let at = index as usize * 6;
        let kind = arena.get(nodes, at)?;
        let a = arena.get(nodes, at + 1)?;
        let b = arena.get(nodes, at + 2)?;
        let op = arena.get(nodes, at + 3)?;
        let child = |id| expression(arena, nodes, count, id, parameters, depth + 1).map(Box::new);
        Ok(match kind {
            0 => Expr::Int(a),
            1 => Expr::Name(
                parameters
                    .get(usize::try_from(a).map_err(|_| "negative parameter index")?)
                    .ok_or("scalar input is absent")?
                    .clone(),
            ),
            2 => Expr::Unary(
                match op {
                    0 => Unary::Neg,
                    1 => Unary::Plus,
                    2 => Unary::BitNot,
                    3 => Unary::Not,
                    _ => return Err("invalid scalar unary tag".into()),
                },
                child(a)?,
            ),
            3 => Expr::Binary(
                match op {
                    0 => Binary::Or,
                    1 => Binary::And,
                    2 => Binary::Eq,
                    3 => Binary::Ne,
                    4 => Binary::Lt,
                    5 => Binary::Le,
                    6 => Binary::Gt,
                    7 => Binary::Ge,
                    11 => Binary::Shl,
                    12 => Binary::Shr,
                    8 => Binary::BitOr,
                    9 => Binary::BitXor,
                    10 => Binary::BitAnd,
                    13 => Binary::Add,
                    14 => Binary::Sub,
                    15 => Binary::Mul,
                    16 => Binary::Div,
                    17 => Binary::Rem,
                    _ => return Err("invalid scalar binary tag".into()),
                },
                child(a)?,
                child(b)?,
            ),
            4 => Expr::Bool(a != 0),
            5 => Expr::Choose(child(a)?, child(b)?, child(op)?),
            10 => Expr::Unit,
            _ => return Err("invalid scalar expression kind".into()),
        })
    }
    fn body(
        arena: &crate::data_runtime::Arena,
        nodes: i64,
        count: i64,
        node: i64,
        names: &[String],
        depth: usize,
    ) -> Result<Vec<Statement>> {
        if node < 0 || node >= count || depth > 64 {
            return Err("invalid compiler statement tree".into());
        }
        let base = node as usize * 6;
        let a = arena.get(nodes, base + 1)?;
        let b = arena.get(nodes, base + 2)?;
        let op = arena.get(nodes, base + 3)?;
        let expr = |node| expression(arena, nodes, count, node, names, 0);
        Ok(match arena.get(nodes, base)? {
            8 => {
                let mut first = body(arena, nodes, count, a, names, depth + 1)?;
                first.extend(body(arena, nodes, count, b, names, depth + 1)?);
                first
            }
            7 => vec![Statement::Bind {
                name: names
                    .get(b as usize)
                    .ok_or("invalid compiler binding")?
                    .clone(),
                ty: match (op >> 16) & 7 {
                    0 => None,
                    1 => Some(Type::Infer),
                    2 => Some(Type::I64),
                    3 => Some(Type::Bool),
                    5 => Some(Type::Unit),
                    _ => return Err("invalid compiler annotation".into()),
                },
                immutable: op & 512 != 0,
                op: match op & 255 {
                    0 => Assign::Set,
                    1 => Assign::Add,
                    2 => Assign::Sub,
                    3 => Assign::Mul,
                    4 => Assign::Div,
                    5 => Assign::Rem,
                    _ => return Err("invalid compiler assignment".into()),
                },
                value: expr(a)?,
            }],
            9 => vec![Statement::If {
                condition: expr(a)?,
                yes: body(arena, nodes, count, b, names, depth + 1)?,
                no: if op < 0 {
                    vec![]
                } else {
                    body(arena, nodes, count, op, names, depth + 1)?
                },
            }],
            12 => vec![Statement::While {
                condition: expr(a)?,
                body: body(arena, nodes, count, b, names, depth + 1)?,
            }],
            13 => {
                let range = a as usize * 6;
                vec![Statement::For {
                    name: names
                        .get(op as usize)
                        .ok_or("invalid compiler loop binding")?
                        .clone(),
                    start: expr(arena.get(nodes, range + 1)?)?,
                    end: expr(arena.get(nodes, range + 2)?)?,
                    body: body(arena, nodes, count, b, names, depth + 1)?,
                }]
            }
            11 => vec![Statement::Expr(expr(a)?)],
            _ => return Err("invalid compiler statement kind".into()),
        })
    }
    body(&arena, nodes, count, root, &bindings, 0)
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct Symbol {
    pub name: String,
    pub id: Id,
    pub inputs: Vec<Type>,
    pub output: Type,
}
fn type_tag(ty: Type) -> Result<i64> {
    match ty {
        Type::I64 => Ok(1),
        Type::Bool => Ok(2),
        Type::Unit => Ok(4),
        _ => Err("unsupported stage-1 type".into()),
    }
}
/// Admitted pure functions are emitted by the same Mira compiler in hosted and
/// freestanding builds. No error silently falls back to the Rust emitter.
#[derive(Clone)]
pub(crate) struct Template {
    pub bytes: Vec<u8>,
    pub calls: Vec<(usize, usize)>,
    pub stack_bytes: usize,
}
#[derive(Clone)]
pub(crate) struct Compiled {
    pub calls: Vec<(usize, Id)>,
    pub bytes: Vec<u8>,
    pub stack_bytes: usize,
}
pub(crate) fn compile(
    definition: &CodeModule,
    definitions: &[CodeModule],
    kernel: bool,
) -> Result<Option<Compiled>> {
    let profile = if kernel { 2 } else { 1 };
    if !admitted(definition, true) || definition.parameters.iter().any(|p| p.name.contains('-')) {
        return Ok(None);
    }
    let nodes = mira_core::code::expressions(definition)?;
    if nodes.len() > 2000 || nodes.iter().any(|n| n.path.len() > 96) {
        return Ok(None);
    }
    let mut definition = definition.clone();
    let mut symbols = Vec::<Symbol>::new();
    let mut bindings = Vec::new();
    for node in mira_core::code::expressions(&definition)? {
        if let Expr::Call(name, _) = node.expression {
            let Some(target) = definitions.iter().find(|d| d.name == *name) else {
                return Ok(None);
            };
            if target.parameters.len() > 16
                || !matches!(target.output, Type::I64 | Type::Bool | Type::Unit)
                || target
                    .parameters
                    .iter()
                    .any(|p| !matches!(p.ty, Type::I64 | Type::Bool))
            {
                return Ok(None);
            }
            let index = if let Some(i) = symbols.iter().position(|s| s.name == *name) {
                i
            } else {
                if symbols.len() >= 128 {
                    return Ok(None);
                }
                symbols.push(Symbol {
                    name: name.clone(),
                    id: target.id,
                    inputs: target.parameters.iter().map(|p| p.ty).collect(),
                    output: target.output,
                });
                symbols.len() - 1
            };
            bindings.push((node.path.clone(), format!("callee{index}")));
        }
    }
    for (path, name) in bindings {
        if let Expr::Call(target, _) = mira_core::code::expression_mut(&mut definition, &path)? {
            *target = name;
        }
    }
    fn alias(name: &mut String, names: &mut std::collections::BTreeMap<String, String>) {
        let next = format!("word{}", names.len());
        *name = names.entry(name.clone()).or_insert(next).clone();
    }
    fn local_expr(e: &mut Expr, names: &mut std::collections::BTreeMap<String, String>) {
        match e {
            Expr::Name(n) => alias(n, names),
            Expr::Unary(_, e) => local_expr(e, names),
            Expr::Binary(_, a, b) => {
                local_expr(a, names);
                local_expr(b, names);
            }
            Expr::Choose(a, b, c) => {
                local_expr(a, names);
                local_expr(b, names);
                local_expr(c, names);
            }
            Expr::Call(_, args) => {
                for e in args {
                    local_expr(e, names);
                }
            }
            _ => {}
        }
    }
    fn locals(body: &mut [Statement], names: &mut std::collections::BTreeMap<String, String>) {
        for s in body {
            match s {
                Statement::Expr(e) => local_expr(e, names),
                Statement::Bind { name, value, .. } => {
                    alias(name, names);
                    local_expr(value, names);
                }
                Statement::If { condition, yes, no } => {
                    local_expr(condition, names);
                    locals(yes, names);
                    locals(no, names);
                }
                Statement::While { condition, body } => {
                    local_expr(condition, names);
                    locals(body, names);
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    local_expr(start, names);
                    local_expr(end, names);
                    alias(name, names);
                    locals(body, names);
                }
                _ => {}
            }
        }
    }
    let mut names = std::collections::BTreeMap::new();
    for p in &mut definition.parameters {
        alias(&mut p.name, &mut names);
    }
    locals(&mut definition.body, &mut names);
    definition.name = "compiled".into();
    definition.id = 1;
    definition.annotated = true;
    let source = crate::behavior::format(&definition);
    if source.len() > 16384 || source.contains('`') {
        return Ok(None);
    }
    let template = crate::compilation_cache::scalar_code(&source, profile, &symbols, || {
        let f = frontend()?;
        let mut stage = parse_stage(&source, &symbols)?;
        let emission = stage
            .arena
            .allocate(f.emission.id, vec![0; f.emission.len()])?;
        stage
            .arena
            .set(emission, field(&f.emission, "profile")?, profile)?;
        let output = stage.arena.allocate(f.output, vec![0; 65536])?;
        let fixups = stage.arena.allocate(f.fixups, vec![0; 8192])?;
        // Parsing/checking happened in Mira. The host only marshals bounded
        // arrays and retrieves byte output, with no opcode/arithmetic decisions.
        let size = f.code.call_compiler(
            f.emit,
            &[
                stage.work,
                stage.nodes,
                stage.parameters,
                emission,
                output,
                65536,
                fixups,
            ],
            &mut stage.arena,
        )?;
        if size < 0 {
            check_error(
                f,
                &stage.arena,
                stage.work,
                stage.arena.get(stage.work, field(&f.work, "error")?)?,
            )?;
            return Err("Mira scalar emitter exceeded its code budget".into());
        }
        if size > 65536 {
            return Err("Mira scalar emitter returned invalid code size".into());
        }
        let bytes = (0..size as usize)
            .map(|i| {
                u8::try_from(stage.arena.get(output, i)?)
                    .map_err(|_| "Mira scalar emitter returned a non-byte".into())
            })
            .collect::<Result<Vec<_>>>()?;
        let guards = stage.arena.get(emission, field(&f.emission, "guards")?)?;
        let mut calls = Vec::new();
        for i in 0..guards as usize {
            let kind = stage.arena.get(fixups, i * 2 + 1)?;
            if kind < 0 {
                let symbol = symbols
                    .get((-kind - 1) as usize)
                    .ok_or("invalid Mira call relocation")?;
                let offset = usize::try_from(stage.arena.get(fixups, i * 2)?)
                    .map_err(|_| "negative Mira relocation offset")?;
                if offset.checked_add(4).is_none_or(|end| end > bytes.len()) {
                    return Err("Mira relocation exceeds code".into());
                }
                let _ = symbol;
                calls.push((offset, (-kind - 1) as usize));
            }
        }
        let entry = usize::try_from(stage.arena.get(emission, field(&f.emission, "entry")?)?)
            .map_err(|_| "negative compiler entry")?;
        if entry >= bytes.len() {
            return Err("compiler entry exceeds image".into());
        }
        Ok(Template {
            bytes,
            calls,
            // 64 binding slots + 64 loop bounds + bounded expression/argument stack.
            stack_bytes: 2048,
        })
    })?;
    let calls = template
        .calls
        .iter()
        .map(|(offset, index)| (*offset, symbols[*index].id))
        .collect();
    Ok(Some(Compiled {
        bytes: template.bytes,
        calls,
        stack_bytes: template.stack_bytes,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiler_library_is_admitted_by_its_mira_backend() {
        let f = super::frontend().unwrap();
        let (definitions, types) = f.code.compiler_model();
        let lowered = crate::bootstrap_lowering::lower(definitions, types).unwrap();
        let mut missed = Vec::new();
        let mut count = 0;
        for d in definitions {
            let Some(body) = lowered.bodies.get(&d.id) else {
                missed.push(format!(
                    "{}: lowering",
                    f.names.get(&d.id).unwrap_or(&d.name)
                ));
                continue;
            };
            match super::compile(body, &lowered.signatures, true) {
                Ok(Some(_)) => count += 1,
                Ok(None) => missed.push(format!(
                    "{}: profile ({} expr, maxpath {}, {} bytes)",
                    f.names.get(&d.id).unwrap_or(&d.name),
                    mira_core::code::expressions(body).unwrap().len(),
                    mira_core::code::expressions(body)
                        .unwrap()
                        .iter()
                        .map(|n| n.path.len())
                        .max()
                        .unwrap_or(0),
                    crate::behavior::format(body).len()
                )),
                Err(e) => missed.push(format!("{}: {e}", f.names.get(&d.id).unwrap_or(&d.name))),
            }
        }
        assert!(
            missed.is_empty(),
            "compiled {count}/{}; remaining: {missed:?}",
            definitions.len()
        );
    }
    #[test]
    fn loops_match_core_scope_and_types() {
        let text = include_str!("../../../language/examples/bootstrap-loops");
        let seed = crate::frontend::parse_seed(text).unwrap();
        let parsed = crate::syntax::parse(text).unwrap();
        assert_eq!(seed, parsed);
        assert_eq!(
            crate::runtime::execute(&parsed, 1).unwrap().terminals[0].1,
            25
        );
        for text in [
            "f(value:i64)->i64 =\n  result = value\n  for value in 0..3\n    result += value\n  result + value\n",
            "f()->i64 =\n  result = 0\n  for i in 2..2\n    result += i\n  result\n",
        ] { assert_eq!(crate::frontend::parse_seed(text).unwrap(),crate::syntax::parse(text).unwrap()); }
        for text in [
            "f()->i64 =\n  for i in 0..3\n    i = 4\n  0\n",
            "f()->i64 =\n  for i in 0..3\n    ()\n  i\n",
            "f()->unit =\n  while 1\n    ()\n",
        ] {
            assert!(crate::syntax::parse(text).is_err());
        }
    }
    #[test]
    fn parses_the_real_operator_selector() {
        let library = crate::bootstrap_compiler::library().unwrap();
        let module = library
            .program
            .code_modules
            .iter()
            .find(|d| d.name == "operator_descriptor")
            .unwrap();
        let formatted = crate::behavior::format(module);
        let text = formatted.as_str();
        let p = crate::frontend::parse_seed(text).unwrap();
        assert_eq!(
            super::parse(text, &p.code_modules[0].parameters).unwrap(),
            p.code_modules[0].body
        );
    }
    #[test]
    fn block_profile_preserves_bindings_branches_and_rejects_bad_scope() {
        let example = include_str!("../../../language/examples/bootstrap-blocks");
        assert_eq!(
            crate::frontend::parse_seed(example).unwrap(),
            crate::syntax::parse(example).unwrap()
        );
        let p = crate::syntax::parse(example).unwrap();
        assert_eq!(crate::runtime::execute(&p, 1).unwrap().terminals[0].1, 22);
        for source in [
            "f(flag:bool)->i64 =\n  result = 0\n  if flag\n    temporary = 3\n    result = temporary\n  else\n    temporary = 4\n    result = temporary\n  result\n",
            "f(value:i64)->unit =\n  local = value\n  local *= 3\n",
            "f()->unit =\n  value:unit = ()\n  value\n",
            "f(flag:bool)->i64 =\n  if flag\n    7\n  else\n    9\n",
        ] {assert_eq!(crate::frontend::parse_seed(source).unwrap(),crate::syntax::parse(source).unwrap(),"{source}");}
        for source in [
            "f(value:i64)->i64 =\n  value = 3\n  value\n",
            "f()->i64 =\n  const value = 1\n  value = 2\n  value\n",
            "f(flag:bool)->i64 =\n  if flag\n    local = 1\n  local\n",
            "f()->i64 =\n  value += 2\n  value\n",
            "f()->i64 =\n  result = 1\n  result = true\n  result\n",
        ] {
            assert!(crate::syntax::parse(source).is_err(), "{source}");
        }
    }
    #[test]
    fn boolean_multi_input_profile_matches_the_seed_checker() {
        use mira_core::code::Parameter;
        use mira_core::Type;
        let example = include_str!("../../../language/examples/bootstrap");
        let seed = crate::frontend::parse_seed(example).unwrap();
        let compiled = crate::syntax::parse(example).unwrap();
        assert_eq!(seed, compiled);
        assert_eq!(
            crate::runtime::execute(&compiled, 1).unwrap().terminals[0].1,
            20
        );
        for source in [
            "f(a:i64,b:i64)->bool = a < b or (a == 0 and b != 0)\n",
            "f(a:bool,b:i64)->i64 = a ? b + 1 : b - 1\n",
            "f(a:bool,b:bool)->bool = not a or b\n",
            "f(a:i64,b:i64,c:i64,d:i64,e:i64,f:i64,g:i64,h:i64)->i64 = a+b+c+d+e+f+g+h\n",
        ] {
            let seed = crate::frontend::parse_seed(source).unwrap();
            let compiled = crate::syntax::parse(source).unwrap();
            assert_eq!(seed, compiled, "{source}");
        }
        let x = [Parameter {
            name: "a".into(),
            ty: Type::I64,
        }];
        let flag = [Parameter {
            name: "a".into(),
            ty: Type::Bool,
        }];
        for source in [
            "f(a:i64)->bool = a and true\n",
            "f(a:i64)->bool = not a\n",
            "f(a:i64)->i64 = true ? a : false\n",
            "f(a:i64)->i64 = a ? 1 : 2\n",
            "f(a:i64,a:i64)->i64 = a\n",
        ] {
            assert!(super::parse(source, &x).is_err(), "{source}");
            assert!(crate::syntax::parse(source).is_err(), "{source}");
        }
        assert!(super::parse("f(a:bool)->i64 = a + 1\n", &flag).is_err());
        assert!(super::parse("f(a)->i64 = 1\n", &x)
            .unwrap_err()
            .contains("error 7"));
        // The checker can infer the isolated scalar signature; it does not
        // invent i64 for an unconstrained input or accept a mismatching caller.
        assert_eq!(
            super::parse("f(a)->i64 = a + 1\n", &x).unwrap(),
            super::parse("f(a:i64)->i64 = a + 1\n", &x).unwrap()
        );
    }
    #[test]
    fn shared_frontend_preserves_seed_ast_and_rejects_unsupported_native_grammar() {
        for expression in [
            "x + 2 * 3",
            "(x - 7) * (x + 2)",
            "~x & 0xff",
            "-9223372036854775808",
            "-x / 3",
            "x % -7",
            "0x7fff | (x ^ 0b1010)",
        ] {
            let source = format!("calculate(x:i64)->i64 = {expression}\n");
            let old = crate::frontend::parse_seed(&source).unwrap();
            let new = crate::syntax::parse(&source).unwrap();
            assert_eq!(old, new, "{expression}");
        }
        for source in [
            "f(x:f32)->f32 = x\n",
            "f(x:i64)->i64 = sys.in8(1)\n",
            "f(x:i64)->i64 = other(x)\n",
            "f(x:i64)->i64 = x\n + 1\n",
            "f()->i64 = 9223372036854775808\n",
        ] {
            assert!(
                super::parse(
                    source,
                    &[mira_core::code::Parameter {
                        name: "x".into(),
                        ty: mira_core::Type::I64
                    }]
                )
                .is_err(),
                "{source}"
            );
        }
    }
}

#[cfg(test)]
#[path = "bootstrap_selftest.rs"]
mod selftest;

#[cfg(test)]
#[path = "bootstrap_module_tests.rs"]
mod module_tests;

#[path = "bootstrap_module.rs"]
pub(crate) mod module;
