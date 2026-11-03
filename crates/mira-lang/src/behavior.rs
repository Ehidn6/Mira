//! Static scalar typing and a bounded reference executor for cell behaviors.
use mira_core::{code::*, *};
use std::collections::{BTreeMap, BTreeSet};
type Env = BTreeMap<String, (Type, bool)>;
type Signatures = BTreeMap<String, (Vec<Type>, Type)>;
fn expect(actual: Type, wanted: Type) -> Result<()> {
    if actual == wanted {
        Ok(())
    } else {
        Err(format!("expected {wanted:?}, got {actual:?}"))
    }
}
pub(crate) fn expression(
    e: &Expr,
    env: &Env,
    signatures: &Signatures,
    types: &[data::Definition],
) -> Result<Type> {
    Ok(match e {
        Expr::Hole(name) => return Err(format!("unfilled code hole ?{name}")),
        Expr::Construct(id, args) => {
            if let Some(id) = crate::tensor::text_type(*id, args, types) {
                return id.map(Type::Data);
            }
            let d = data::find(types, *id)?;
            let members = d.member_types();
            if args.is_empty() {
                if members.iter().any(|t| matches!(t, Type::Data(_))) {
                    return Err("aggregate members require explicit values".into());
                }
            } else {
                if args.len() != members.len() {
                    return Err(format!(
                        "{}: expected {} members, got {}",
                        d.name,
                        members.len(),
                        args.len()
                    ));
                }
                for (arg, ty) in args.iter().zip(members) {
                    expect(expression(arg, env, signatures, types)?, ty)?;
                }
            }
            Type::Data(*id)
        }
        Expr::View(id, address) => {
            data::find(types, *id)?;
            expect(expression(address, env, signatures, types)?, Type::Addr)?;
            Type::Data(*id)
        }
        Expr::Field(value, name) => {
            let Type::Data(id) = expression(value, env, signatures, types)? else {
                return Err("field access requires a record or array".into());
            };
            let d = data::find(types, id)?;
            if matches!(d.kind, data::Kind::Array { .. } | data::Kind::Tensor { .. })
                && name == "len"
            {
                Type::I64
            } else {
                d.field(name)
                    .ok_or_else(|| format!("{}: unknown field {name}", d.name))?
                    .1
            }
        }
        Expr::Index(value, index) => {
            expect(expression(index, env, signatures, types)?, Type::I64)?;
            let Type::Data(id) = expression(value, env, signatures, types)? else {
                return Err("index requires an array".into());
            };
            match data::find(types, id)?.kind {
                data::Kind::Array { element, .. } | data::Kind::Tensor { element, .. } => element,
                _ => return Err("index requires an array".into()),
            }
        }
        Expr::Float(bits) => {
            if !f32::from_bits(*bits).is_finite() {
                return Err("nonfinite f32 literal".into());
            }
            Type::F32
        }
        Expr::Int(_) => Type::I64,
        Expr::Bool(_) => Type::Bool,
        Expr::Unit => Type::Unit,
        Expr::Name(name) => {
            env.get(name)
                .ok_or_else(|| format!("{name}: unknown local or input"))?
                .0
        }
        Expr::Unary(op, value) => {
            let ty = expression(value, env, signatures, types)?;
            if matches!(op, Unary::Neg | Unary::Plus | Unary::FNeg) && ty == Type::F32 {
                return Ok(Type::F32);
            }
            expect(
                ty,
                if *op == Unary::Not {
                    Type::Bool
                } else {
                    Type::I64
                },
            )?;
            ty
        }
        Expr::Binary(op, a, b) => {
            let a = expression(a, env, signatures, types)?;
            let b = expression(b, env, signatures, types)?;
            match op {
                Binary::FAdd | Binary::FSub | Binary::FMul | Binary::FDiv => {
                    expect(a, Type::F32)?;
                    expect(b, Type::F32)?;
                    Type::F32
                }
                Binary::FEq
                | Binary::FNe
                | Binary::FLt
                | Binary::FLe
                | Binary::FGt
                | Binary::FGe => {
                    expect(a, Type::F32)?;
                    expect(b, Type::F32)?;
                    Type::Bool
                }
                Binary::Add | Binary::Sub | Binary::Mul | Binary::Div if a == Type::F32 => {
                    expect(b, Type::F32)?;
                    Type::F32
                }
                Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge if a == Type::F32 => {
                    expect(b, Type::F32)?;
                    Type::Bool
                }
                Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge if a == Type::Addr => {
                    expect(b, Type::Addr)?;
                    Type::Bool
                }
                Binary::And | Binary::Or => {
                    expect(a, Type::Bool)?;
                    expect(b, Type::Bool)?;
                    Type::Bool
                }
                Binary::Eq | Binary::Ne => {
                    expect(b, a)?;
                    if !matches!(a, Type::I64 | Type::F32 | Type::Bool | Type::Addr) {
                        return Err("equality requires a scalar value".into());
                    }
                    Type::Bool
                }
                Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge => {
                    expect(a, Type::I64)?;
                    expect(b, Type::I64)?;
                    Type::Bool
                }
                _ => {
                    expect(a, Type::I64)?;
                    expect(b, Type::I64)?;
                    Type::I64
                }
            }
        }
        Expr::Choose(c, a, b) => {
            expect(expression(c, env, signatures, types)?, Type::Bool)?;
            let a = expression(a, env, signatures, types)?;
            expect(expression(b, env, signatures, types)?, a)?;
            a
        }
        Expr::Call(name, args) => {
            if name == "sys.address_of" {
                let [target]=args.as_slice() else{return Err("sys.address_of expects one field or indexed element".into())};
                if !matches!(target,Expr::Field(..)|Expr::Index(..)){return Err("sys.address_of requires an addressable field or element".into());}
                expression(target,env,signatures,types)?;
                return Ok(Type::Addr);
            }
            if name == "sys.procedure_address" {
                let [Expr::Name(target)] = args.as_slice() else {
                    return Err("procedure_address expects one behavior symbol".into());
                };
                if env.contains_key(target) {
                    return Err("procedure_address expects a definition, not a local/input".into());
                }
                let (parameters, output) = signatures
                    .get(target)
                    .ok_or_else(|| format!("{target}: unknown procedure address target"))?;
                let word = |ty: &Type| matches!(ty, Type::I64 | Type::Addr | Type::Bool);
                if parameters.len() > 16
                    || !parameters.iter().all(word)
                    || !(word(output) || *output == Type::Unit)
                {
                    return Err("procedure_address requires 0..16 i64/addr/bool inputs and an i64/addr/bool/unit result".into());
                }
                return Ok(Type::Addr);
            }
            if crate::intrinsics::dynamic_native(name) {
                let argtypes = args
                    .iter()
                    .map(|e| expression(e, env, signatures, types))
                    .collect::<Result<Vec<_>>>()?;
                return crate::intrinsics::native_signature(name, &argtypes).unwrap();
            }
            if matches!(name.as_str(), "sys.scratch" | "sys.read_only") {
                let [Expr::Name(target), bytes, supplied @ ..] = args.as_slice() else {
                    return Err("scratch expects a behavior symbol, byte limit and optional inputs".into());
                };
                if env.contains_key(target) {
                    return Err("scratch entry must be a behavior symbol".into());
                }
                let (parameters, result) = signatures
                    .get(target)
                    .ok_or("scratch entry behavior missing")?;
                if parameters.len() != supplied.len() || (name == "sys.read_only" && !supplied.is_empty()) {
                    return Err("scratch: wrong input count".into());
                }
                if matches!(result, Type::Data(_)) {
                    return Err("scratch entry cannot return a temporary reference".into());
                }
                for (input, wanted) in supplied.iter().zip(parameters) {
                    expect(expression(input, env, signatures, types)?, *wanted)?;
                    if let Type::Data(id) = wanted {
                        if data::find(types, *id)?.member_types().iter().any(|ty| matches!(ty, Type::Data(_))) {
                            return Err("scratch inputs must be flat data without reference fields".into());
                        }
                    }
                }
                expect(expression(bytes, env, signatures, types)?, Type::I64)?;
                return Ok(*result);
            }
            if name == "sys.neural_try" {
                let [Expr::Name(target), fuel, scratch] = args.as_slice() else {
                    return Err(
                        "neural_try expects a behavior name, operation budget and scratch bytes"
                            .into(),
                    );
                };
                if env.contains_key(target) {
                    return Err("neural entry must be a behavior symbol".into());
                }
                let (parameters, result) = signatures
                    .get(target)
                    .ok_or("neural entry behavior missing")?;
                if !parameters.is_empty() || *result != Type::Unit {
                    return Err("neural entry must have signature () -> unit".into());
                }
                expect(expression(fuel, env, signatures, types)?, Type::I64)?;
                expect(expression(scratch, env, signatures, types)?, Type::I64)?;
                return Ok(Type::Bool);
            }
            if name.starts_with("tensor.")
                || name.starts_with("relation.")
                || name.ends_with(".persist")
                || name.ends_with(".aligned")
                || crate::tensor::is_layout_query(name)
            {
                let argtypes = args
                    .iter()
                    .map(|e| expression(e, env, signatures, types))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(result) = crate::tensor::signature(name, &argtypes, types) {
                    return result;
                }
            }
            if crate::intrinsics::handler_kind(name).is_some() {
                let [Expr::Name(target)] = args.as_slice() else {
                    return Err(format!(
                        "{name}: expected one behavior name, not a runtime value"
                    ));
                };
                if env.contains_key(target) {
                    return Err(format!(
                        "{target}: handler must be a behavior, not a local/input"
                    ));
                }
                let (parameters, output) = signatures
                    .get(target)
                    .ok_or_else(|| format!("{target}: unknown interrupt behavior"))?;
                if parameters != &[Type::Addr, Type::I64] || *output != Type::Unit {
                    return Err(format!(
                        "{target}: handler must have signature (frame: addr, error: i64) -> unit"
                    ));
                }
                return Ok(Type::Addr);
            }
            if env.contains_key(name) {
                return Err(format!("{name}: a scalar local/input is not callable"));
            }
            let builtin = crate::intrinsics::signature(name);
            let (parameters, output) = signatures
                .get(name)
                .or(builtin.as_ref())
                .ok_or_else(|| format!("{name}: unknown behavior"))?;
            if args.len() != parameters.len() {
                return Err(format!(
                    "{name}: expected {} arguments, got {}",
                    parameters.len(),
                    args.len()
                ));
            }
            for (value, ty) in args.iter().zip(parameters) {
                expect(expression(value, env, signatures, types)?, *ty)?;
            }
            *output
        }
    })
}
fn block(
    body: &[Statement],
    env: &mut Env,
    signatures: &Signatures,
    types: &[data::Definition],
) -> Result<Type> {
    let mut result = Type::Unit;
    for stmt in body {
        result = match stmt {
            Statement::Set { target, op, value } => {
                if !matches!(target, Expr::Field(..) | Expr::Index(..)) {
                    return Err("assignment target must be a field or array element".into());
                }
                if let Expr::Field(base, name) = target {
                    if name == "len" {
                        if let Type::Data(id) = expression(base, env, signatures, types)? {
                            if matches!(
                                data::find(types, id)?.kind,
                                data::Kind::Array { .. } | data::Kind::Tensor { .. }
                            ) {
                                return Err("array length is read-only".into());
                            }
                        }
                    }
                }
                let ty = expression(target, env, signatures, types)?;
                expect(expression(value, env, signatures, types)?, ty)?;
                if *op != Assign::Set {
                    if !matches!(ty, Type::I64 | Type::F32) {
                        return Err("compound assignment requires numeric data".into());
                    }
                }
                Type::Unit
            }
            Statement::Expr(e) => expression(e, env, signatures, types)?,
            Statement::Bind {
                name,
                ty,
                immutable,
                op,
                value,
            } => {
                let inferred = expression(value, env, signatures, types)?;
                if let Some(annotation) = ty {
                    expect(inferred, *annotation)?;
                }
                if let Some((old, can_write)) = env.get(name) {
                    if !can_write {
                        return Err(format!("{name}: input or const cannot be assigned"));
                    }
                    if ty.is_some() || *immutable {
                        return Err(format!("{name}: duplicate declaration"));
                    }
                    expect(inferred, *old)?;
                    if *op != Assign::Set {
                        if !matches!(old, Type::I64 | Type::F32) {
                            return Err("compound assignment requires numeric data".into());
                        }
                    }
                } else {
                    if *op != Assign::Set {
                        return Err(format!("{name}: compound assignment before declaration"));
                    }
                    env.insert(name.clone(), (inferred, !immutable));
                }
                Type::Unit
            }
            Statement::If { condition, yes, no } => {
                expect(expression(condition, env, signatures, types)?, Type::Bool)?;
                let a = block(yes, &mut env.clone(), signatures, types)?;
                if no.is_empty() {
                    Type::Unit
                } else {
                    let b = block(no, &mut env.clone(), signatures, types)?;
                    expect(b, a)?;
                    a
                }
            }
            Statement::While { condition, body } => {
                expect(expression(condition, env, signatures, types)?, Type::Bool)?;
                block(body, &mut env.clone(), signatures, types)?;
                Type::Unit
            }
            Statement::For {
                name,
                start,
                end,
                body,
            } => {
                expect(expression(start, env, signatures, types)?, Type::I64)?;
                expect(expression(end, env, signatures, types)?, Type::I64)?;
                let mut inner = env.clone();
                inner.insert(name.clone(), (Type::I64, false));
                block(body, &mut inner, signatures, types)?;
                Type::Unit
            }
        };
    }
    Ok(result)
}
pub(crate) fn collect_types(defs: &[CodeModule]) -> Vec<data::Definition> {
    fn walk(defs: &[CodeModule], out: &mut Vec<data::Definition>) {
        for d in defs {
            for ty in &d.data_types {
                if !out.iter().any(|t| t.id == ty.id) {
                    out.push(ty.clone());
                }
            }
            walk(&d.locals, out);
        }
    }
    let mut out = Vec::new();
    walk(defs, &mut out);
    out
}
pub(crate) fn expression_type(
    e: &Expr,
    env: &BTreeMap<String, Type>,
    defs: &[CodeModule],
    types: &[data::Definition],
) -> Result<Type> {
    let env = env.iter().map(|(n, t)| (n.clone(), (*t, true))).collect();
    let signatures = defs
        .iter()
        .map(|d| {
            (
                d.name.clone(),
                (d.parameters.iter().map(|p| p.ty).collect(), d.output),
            )
        })
        .collect();
    expression(e, &env, &signatures, types)
}
pub fn check(definitions: &mut [CodeModule]) -> Result<()> {
    let types = collect_types(definitions);
    check_with(definitions, &BTreeMap::new(), &types)
}
pub(crate) fn check_with(
    definitions: &mut [CodeModule],
    external: &Signatures,
    types: &[data::Definition],
) -> Result<()> {
    fn signatures(defs: &[CodeModule], base: &Signatures) -> Signatures {
        let mut s = base.clone();
        for (name, d) in mira_core::code::symbols(defs) {
            s.insert(
                name,
                (d.parameters.iter().map(|p| p.ty).collect(), d.output),
            );
        }
        s
    }
    fn count(defs: &[CodeModule]) -> usize {
        defs.iter().map(|d| 1 + count(&d.locals)).sum()
    }
    fn names(defs: &[CodeModule]) -> Result<()> {
        let mut seen = BTreeSet::new();
        for d in defs {
            if !seen.insert(&d.name) {
                return Err(format!("{}: duplicate local procedure", d.name));
            }
            let mut parameters = BTreeSet::new();
            for p in &d.parameters {
                if !parameters.insert(&p.name) {
                    return Err(format!("{}/{}: duplicate input", d.name, p.name));
                }
            }
            names(&d.locals)?;
        }
        Ok(())
    }
    fn infer(defs: &mut [CodeModule], base: &Signatures, types: &[data::Definition]) -> bool {
        let scope = signatures(defs, base);
        let mut changed = false;
        for d in defs {
            changed |= infer(&mut d.locals, &scope, types);
            let local = signatures(&d.locals, &scope);
            let mut env = d
                .parameters
                .iter()
                .map(|p| (p.name.clone(), (p.ty, false)))
                .collect();
            if let Ok(t) = block(&d.body, &mut env, &local, types) {
                if !d.annotated && d.output != t {
                    d.output = t;
                    changed = true;
                }
            }
        }
        changed
    }
    fn validate(defs: &[CodeModule], base: &Signatures, types: &[data::Definition]) -> Result<()> {
        let scope = signatures(defs, base);
        for d in defs {
            let local = signatures(&d.locals, &scope);
            validate(&d.locals, &scope, types).map_err(|e| format!("{}/{}", d.name, e))?;
            let mut env = d
                .parameters
                .iter()
                .map(|p| (p.name.clone(), (p.ty, false)))
                .collect();
            let actual =
                block(&d.body, &mut env, &local, types).map_err(|e| format!("{}: {e}", d.name))?;
            expect(actual, d.output).map_err(|e| format!("{}: {e}", d.name))?;
        }
        Ok(())
    }
    names(definitions)?;
    for _ in 0..count(definitions) + 2 {
        if !infer(definitions, external, types) {
            break;
        }
    }
    validate(definitions, external, types)
}
/// Lower local procedure names to private internal labels, without creating cells.
pub(crate) fn procedures(
    definitions: &[CodeModule],
    types: &[data::Definition],
) -> Result<Vec<CodeModule>> {
    type Scope = BTreeMap<String, Id>;
    fn expr(e: &mut Expr, scope: &Scope) -> Result<()> {
        match e {
            Expr::Call(name, args) => {
                if matches!(
                    name.as_str(),
                    "sys.neural_try" | "sys.scratch" | "sys.read_only"
                ) {
                    let Some(Expr::Name(target)) = args.first_mut() else {
                        return Err("neural entry requires a symbol".into());
                    };
                    let id = scope.get(target).ok_or("neural entry not in scope")?;
                    *target = format!("#{id}");
                    for a in args.iter_mut().skip(1) {
                        expr(a, scope)?;
                    }
                    return Ok(());
                }
                if crate::intrinsics::symbol_reference(name) {
                    let [Expr::Name(target)] = args.as_mut_slice() else {
                        return Err("handler expects one behavior symbol".into());
                    };
                    let id = scope
                        .get(target)
                        .ok_or_else(|| format!("{target}: unknown handler behavior"))?;
                    *target = format!("#{id}");
                    return Ok(());
                }
                if name.starts_with("tensor.")
                    || name == "array.literal_utf8"
                    || name.ends_with(".aligned")
                    || name.starts_with("relation.")
                    || crate::tensor::is_layout_query(name)
                    || crate::intrinsics::dynamic_native(name)
                {
                    for a in args {
                        expr(a, scope)?;
                    }
                    return Ok(());
                }
                let id = scope
                    .get(name)
                    .copied()
                    .or_else(|| crate::intrinsics::signature(name).map(|_| 0))
                    .ok_or_else(|| format!("{name}: unknown local procedure"))?;
                if id != 0 {
                    *name = format!("#{id}");
                }
                for a in args {
                    expr(a, scope)?;
                }
            }
            Expr::Construct(_, args) => {
                for arg in args {
                    expr(arg, scope)?;
                }
            }
            Expr::View(_, e) | Expr::Field(e, _) | Expr::Unary(_, e) => expr(e, scope)?,
            Expr::Index(a, b) => {
                expr(a, scope)?;
                expr(b, scope)?;
            }
            Expr::Binary(_, a, b) => {
                expr(a, scope)?;
                expr(b, scope)?;
            }
            Expr::Choose(c, a, b) => {
                expr(c, scope)?;
                expr(a, scope)?;
                expr(b, scope)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn body(stmts: &mut [Statement], scope: &Scope) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Set { target, value, .. } => {
                    expr(target, scope)?;
                    expr(value, scope)?;
                }
                Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e, scope)?,
                Statement::If { condition, yes, no } => {
                    expr(condition, scope)?;
                    body(yes, scope)?;
                    body(no, scope)?;
                }
                Statement::While {
                    condition,
                    body: stmts,
                } => {
                    expr(condition, scope)?;
                    body(stmts, scope)?;
                }
                Statement::For {
                    start,
                    end,
                    body: stmts,
                    ..
                } => {
                    expr(start, scope)?;
                    expr(end, scope)?;
                    body(stmts, scope)?;
                }
            }
        }
        Ok(())
    }
    fn walk(defs: &[CodeModule], base: &Scope, out: &mut Vec<CodeModule>) -> Result<()> {
        let mut scope = base.clone();
        for (name, d) in mira_core::code::symbols(defs) {
            scope.insert(name, d.id);
        }
        for d in defs {
            let mut local = scope.clone();
            for (name, helper) in mira_core::code::symbols(&d.locals) {
                local.insert(name, helper.id);
            }
            let mut procedure = d.clone();
            procedure.name = format!("#{}", d.id);
            procedure.locals.clear();
            body(&mut procedure.body, &local)?;
            out.push(procedure);
            walk(&d.locals, &scope, out)?;
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(definitions, &BTreeMap::new(), &mut out)?;
    crate::tensor::lower(&mut out, types)?;
    crate::numeric::lower(&mut out, types)?;
    crate::access::validate(&out, types)?;
    Ok(out)
}
/// Contextual calls stay in the owning graph activation, including fuel,
/// typed storage, stateful updates and transaction scopes.
pub(crate) enum BoundTarget<'a> {
    Cell(Id),
    Graph(Id, &'a str),
}
pub(crate) trait ExternalCalls {
    fn invoke(
        &mut self,
        target: BoundTarget<'_>,
        inputs: &[i64],
        arena: &mut crate::data_runtime::Arena,
        fuel: &mut u64,
        neural: bool,
    ) -> Result<i64>;
    fn begin_scope(&mut self) -> Result<()>;
    fn end_scope(&mut self, commit: bool, read_only: bool) -> Result<()>;
}
pub(crate) trait StructuralHost {
    fn call(&mut self, name: &str, args: &[i64]) -> Result<i64>;
}
struct Machine<'a> {
    definitions: &'a [CodeModule],
    data_types: &'a [data::Definition],
    arena: &'a mut crate::data_runtime::Arena,
    steps: u64,
    depth: usize,
    owners: BTreeMap<Id, Id>,
    observed: BTreeMap<Id, i64>,
    graph_owners: BTreeMap<Id, (Id, String)>,
    graph_slots: BTreeMap<Id, Vec<(Id, String)>>,
    ports: BTreeMap<(Id, String), i64>,
    neural_active: bool,
    neural_status: i64,
    external: Option<&'a mut dyn ExternalCalls>,
    structural: Option<&'a mut dyn StructuralHost>,
    host_entries: std::collections::BTreeSet<Id>,
}
impl Machine<'_> {
    fn tick(&mut self) -> Result<()> {
        self.steps = self
            .steps
            .checked_sub(1)
            .ok_or("behavior execution budget exceeded")?;
        Ok(())
    }
    fn call(&mut self, id: Id, args: &[i64]) -> Result<i64> {
        self.tick()?;
        if self.depth > 0 {
            let cell = self.owners.get(&id).copied();
            let graph = self.graph_owners.get(&id).cloned();
            if let Some(external) = self.external.as_mut() {
                let target = cell.map(BoundTarget::Cell).or_else(|| {
                    graph
                        .as_ref()
                        .map(|(id, output)| BoundTarget::Graph(*id, output))
                });
                if let Some(target) = target {
                    return external.invoke(
                        target,
                        args,
                        self.arena,
                        &mut self.steps,
                        self.neural_active,
                    );
                }
            }
        }
        if self.host_entries.contains(&id) {
            return Err("contextual graph entry requires the owning scheduler".into());
        }
        self.depth += 1;
        if self.depth > 256 {
            return Err("behavior call-depth budget exceeded".into());
        }
        let d = self
            .definitions
            .iter()
            .find(|d| d.id == id)
            .ok_or("missing cell behavior")?;
        if d.parameters.len() != args.len() {
            return Err(format!("{}: input count mismatch", d.name));
        }
        if let Some(owner) = self
            .owners
            .get(&id)
            .copied()
            .or_else(|| self.graph_owners.get(&id).map(|(space, _)| *space))
        {
            for (p, value) in d.parameters.iter().zip(args) {
                self.ports.insert((owner, p.name.clone()), *value);
            }
        }
        let mut locals = d
            .parameters
            .iter()
            .zip(args)
            .map(|(p, v)| (p.name.clone(), *v))
            .collect();
        let result = self.body(&d.body, &mut locals);
        self.depth -= 1;
        if let (Ok(value), Some(cell)) = (&result, self.owners.get(&id)) {
            self.observed.insert(*cell, *value);
        }
        // Publish only actual deliveries from the last invocation of this graph.
        // Inactive payload placeholders never become observable values.
        if result.is_ok() {
            if let Some(slots) = self.graph_slots.get(&id) {
                let mut cells = BTreeMap::new();
                for (cell, port) in slots {
                    let name = format!("#{cell}.{port}");
                    let Some(present) = locals.get(&format!("{name}?")) else {
                        continue;
                    };
                    cells.entry(*cell).or_insert(None);
                    self.ports.remove(&(*cell, port.clone()));
                    if *present != 0 {
                        let value = locals[&name];
                        self.ports.insert((*cell, port.clone()), value);
                        cells.insert(*cell, Some(value));
                    }
                }
                for (cell, value) in cells {
                    self.observed.remove(&cell);
                    if let Some(value) = value {
                        self.observed.insert(cell, value);
                    }
                }
            }
        }
        if let (Ok(value), Some((space, output))) = (&result, self.graph_owners.get(&id)) {
            self.ports.insert((*space, output.clone()), *value);
        }
        result.map_err(|e| format!("{}: {e}", d.name))
    }
    fn place(&mut self, e: &Expr, env: &mut BTreeMap<String, i64>) -> Result<(i64, usize)> {
        match e {
            Expr::Field(base, name) => {
                let handle = self.expr(base, env)?;
                let id = self.arena.object(handle)?.kind;
                let (index, _) = data::find(self.data_types, id)?
                    .field(name)
                    .ok_or_else(|| format!("unknown or read-only field {name}"))?;
                Ok((handle, index))
            }
            Expr::Index(base, index) => {
                let handle = self.expr(base, env)?;
                let index = self.expr(index, env)?;
                let len = self.arena.object(handle)?.words.len();
                let index = usize::try_from(index)
                    .ok()
                    .filter(|i| *i < len)
                    .ok_or("array index out of bounds")?;
                Ok((handle, index))
            }
            _ => Err("not an assignable data member".into()),
        }
    }
    fn expr(&mut self, e: &Expr, env: &mut BTreeMap<String, i64>) -> Result<i64> {
        self.tick()?;
        match e {
            Expr::Hole(name) => Err(format!("cannot execute code hole ?{name}")),
            Expr::Construct(id, args) => {
                let size = data::find(self.data_types, *id)?.len();
                // Allocate first, just like the native arena, including nested initializers.
                let handle = self.arena.allocate(*id, vec![0; size])?;
                for (index, arg) in args.iter().enumerate() {
                    let value = self.expr(arg, env)?;
                    self.arena.set(handle, index, value)?;
                }
                Ok(handle)
            }
            Expr::View(..) => Err(
                "typed memory views require build-kernel / UEFI; host memory is not accessible"
                    .into(),
            ),
            Expr::Field(base, name) if name == "len" => {
                let handle = self.expr(base, env)?;
                let o = self.arena.object(handle)?;
                if matches!(
                    data::find(self.data_types, o.kind)?.kind,
                    data::Kind::Array { .. } | data::Kind::Tensor { .. }
                ) {
                    Ok(o.words.len() as i64)
                } else {
                    let index = data::find(self.data_types, o.kind)?
                        .field(name)
                        .ok_or("unknown field")?
                        .0;
                    self.arena.get(handle, index)
                }
            }
            Expr::Field(..) | Expr::Index(..) => {
                let (handle, index) = self.place(e, env)?;
                self.arena.get(handle, index)
            }
            Expr::Float(bits) => Ok(*bits as i64),
            Expr::Int(v) => Ok(*v),
            Expr::Bool(v) => Ok(i64::from(*v)),
            Expr::Unit => Ok(0),
            Expr::Name(n) => env
                .get(n)
                .copied()
                .ok_or_else(|| format!("{n}: unavailable local")),
            Expr::Unary(op, e) => {
                let v = self.expr(e, env)?;
                match op {
                    Unary::FNeg => mira_core::vm::f32_word(-mira_core::vm::word_f32(v)?),
                    Unary::Not => Ok(i64::from(v == 0)),
                    Unary::Plus => Ok(v),
                    Unary::BitNot => Ok(!v),
                    Unary::Neg => v.checked_neg().ok_or("i64 overflow".into()),
                }
            }
            Expr::Choose(c, a, b) => {
                let c = self.expr(c, env)?;
                self.expr(if c != 0 { a } else { b }, env)
            }
            Expr::Call(name, arguments) => {
                if name == "sys.neural_status" {
                    return Ok(self.neural_status);
                }
                if matches!(name.as_str(), "sys.scratch" | "sys.read_only") {
                    let [Expr::Name(target), bytes, supplied @ ..] = arguments.as_slice() else {
                        return Err("invalid scratch entry".into());
                    };
                    crate::effects::verify_scratch(target, self.definitions, self.data_types)?;
                    if name == "sys.read_only" {
                        crate::access::verify_read_only(target, self.definitions, self.data_types)?;
                    }
                    let bytes = self.expr(bytes, env)?;
                    let values = supplied.iter().map(|arg| self.expr(arg, env)).collect::<Result<Vec<_>>>()?;
                    let read_only_snapshot = if name == "sys.read_only" {
                        Some(self.arena.clone())
                    } else {
                        None
                    };
                    let mark = self.arena.mark();
                    if bytes < 0 || bytes as usize > self.arena.limit.saturating_sub(mark.1) {
                        return Err("scratch arena limit exceeds available memory".into());
                    }
                    let d = self
                        .definitions
                        .iter()
                        .find(|d| d.name == *target)
                        .ok_or("scratch entry missing")?;
                    let limit = self.arena.limit;
                    let ports = self.ports.clone();
                    let observed = self.observed.clone();
                    if let Some(external) = self.external.as_mut() {
                        external.begin_scope()?;
                    }
                    self.arena.limit = mark.1 + bytes as usize;
                    let mut result = self.call(d.id, &values);
                    if let Some(external) = self.external.as_mut() {
                        let finish = external.end_scope(result.is_ok(), name == "sys.read_only");
                        if result.is_ok() {
                            if let Err(error) = finish {
                                result = Err(error);
                            }
                        }
                    }
                    if let Some(snapshot) = read_only_snapshot {
                        let unchanged = self.arena.export_buffers().and_then(|buffers| {
                            snapshot.export_buffers().map(|old| old == buffers)
                        });
                        if !matches!(unchanged, Ok(true)) {
                            result =
                                Err("read_only scope attempted a persistent graph buffer update"
                                    .into());
                        }
                        if result.is_err() {
                            self.arena.restore(snapshot);
                        }
                    }
                    self.arena.reclaim(mark);
                    self.arena.limit = limit;
                    // Internal diagnostic deliveries must not retain released handles.
                    self.ports = ports;
                    self.observed = observed;
                    return result;
                }
                if name == "sys.neural_try" {
                    let [Expr::Name(target), fuel, scratch] = arguments.as_slice() else {
                        return Err("invalid neural entry".into());
                    };
                    crate::effects::verify_neural(target, self.definitions)?;
                    let fuel = self.expr(fuel, env)?;
                    let scratch = self.expr(scratch, env)?;
                    if self.neural_active
                        || fuel <= 0
                        || fuel > 10_000_000
                        || scratch < 0
                        || scratch as usize > self.arena.limit.saturating_sub(self.arena.mark().1)
                    {
                        self.neural_status = 6;
                        return Ok(0);
                    }
                    let d = self
                        .definitions
                        .iter()
                        .find(|d| d.name == *target)
                        .ok_or("neural entry missing")?;
                    let backup = self.arena.clone();
                    let mark = self.arena.mark();
                    let old_steps = self.steps;
                    let old_depth = self.depth;
                    let old_ports = self.ports.clone();
                    let old_observed = self.observed.clone();
                    let copied = 1 + self
                        .arena
                        .persistent
                        .values()
                        .map(|(_, h)| self.arena.object(*h).map(|o| o.words.len()).unwrap_or(0))
                        .sum::<usize>();
                    let bounded_fuel = (fuel as u64).min(old_steps);
                    if bounded_fuel as usize <= copied {
                        self.neural_status = 1;
                        return Ok(0);
                    }
                    self.arena.limit = mark.1 + scratch as usize;
                    self.steps = bounded_fuel - copied as u64;
                    if let Some(external) = self.external.as_mut() {
                        external.begin_scope()?;
                    }
                    self.neural_active = true;
                    let mut result = self.call(d.id, &[]);
                    if let Some(external) = self.external.as_mut() {
                        let finish = external.end_scope(result.is_ok(), false);
                        if result.is_ok() {
                            if let Err(error) = finish {
                                result = Err(error);
                            }
                        }
                    }
                    self.neural_active = false;
                    let spent = bounded_fuel.saturating_sub(self.steps);
                    self.steps = old_steps.saturating_sub(spent);
                    self.depth = old_depth;
                    if result.is_ok() {
                        self.arena.reclaim(mark);
                        self.arena.limit = backup.limit;
                        self.ports = old_ports;
                        self.observed = old_observed;
                        self.neural_status = 0;
                        return Ok(1);
                    }
                    let error = result.unwrap_err();
                    self.neural_status = if error.contains("budget") {
                        1
                    } else if error.contains("arena") || error.contains("stack") {
                        2
                    } else if error.contains("index") || error.contains("delivery") {
                        3
                    } else {
                        4
                    };
                    self.arena.restore(backup);
                    self.ports = old_ports;
                    self.observed = old_observed;
                    return Ok(0);
                }
                if let Some(rest) = name.strip_prefix("#buffer.") {
                    let (cell, ty) = rest.split_once('.').ok_or("invalid buffer reference")?;
                    return self.arena.persistent_buffer(
                        cell.parse().map_err(|_| "invalid buffer owner")?,
                        ty.parse().map_err(|_| "invalid buffer type")?,
                        self.data_types,
                    );
                }
                if crate::intrinsics::symbol_reference(name) {
                    return Err(format!(
                        "{name}: entry address generation requires native machine-code execution"
                    ));
                }
                let mut args = Vec::new();
                for e in arguments {
                    args.push(self.expr(e, env)?);
                }
                if let Some(id) = crate::intrinsics::aligned_type(name) {
                    let size = data::find(self.data_types, id)?.len();
                    return self.arena.allocate_aligned(id, vec![0; size], args[0]);
                }
                if name.starts_with("meta.") || name.starts_with("reflect.") {
                    if let Some(stage) = self.structural.as_mut() {
                        return stage.call(name, &args);
                    }
                    return Err("structural operation requires compile-time expansion".into());
                }
                if crate::intrinsics::dynamic_native(name) {
                    return Err(format!(
                        "{name}: requires native machine-code execution; portable Run does not expose host addresses"
                    ));
                }
                if crate::intrinsics::signature(name).is_some() {
                    return crate::intrinsics::interpret(name, &args);
                }
                let id = self
                    .definitions
                    .iter()
                    .find(|d| d.name == *name)
                    .ok_or_else(|| format!("{name}: unknown behavior"))?
                    .id;
                self.call(id, &args)
            }
            Expr::Binary(op, a, b) => {
                let a = self.expr(a, env)?;
                if *op == Binary::And && a == 0 {
                    return Ok(0);
                }
                if *op == Binary::Or && a != 0 {
                    return Ok(1);
                }
                let b = self.expr(b, env)?;
                arithmetic(*op, a, b)
            }
        }
    }
    fn body(&mut self, body: &[Statement], env: &mut BTreeMap<String, i64>) -> Result<i64> {
        let mut result = 0;
        for stmt in body {
            self.tick()?;
            result = match stmt {
                Statement::Set { target, op, value } => {
                    let (handle, index) = self.place(target, env)?;
                    let old = self.arena.get(handle, index)?;
                    let rhs = self.expr(value, env)?;
                    let value = if *op == Assign::Set {
                        rhs
                    } else {
                        arithmetic(assign_binary(*op), old, rhs)?
                    };
                    self.arena.set(handle, index, value)?;
                    0
                }
                Statement::Expr(e) => self.expr(e, env)?,
                Statement::Bind {
                    name, op, value, ..
                } => {
                    let v = self.expr(value, env)?;
                    let v = if *op == Assign::Set {
                        v
                    } else {
                        let a = *env.get(name).ok_or("missing local")?;
                        arithmetic(
                            match op {
                                Assign::FAdd => Binary::FAdd,
                                Assign::FSub => Binary::FSub,
                                Assign::FMul => Binary::FMul,
                                Assign::FDiv => Binary::FDiv,
                                Assign::Add => Binary::Add,
                                Assign::Sub => Binary::Sub,
                                Assign::Mul => Binary::Mul,
                                Assign::Div => Binary::Div,
                                Assign::Rem => Binary::Rem,
                                Assign::Set => unreachable!(),
                            },
                            a,
                            v,
                        )?
                    };
                    env.insert(name.clone(), v);
                    0
                }
                Statement::If { condition, yes, no } => {
                    let c = self.expr(condition, env)?;
                    self.scoped(if c != 0 { yes } else { no }, env)?
                }
                Statement::While { condition, body } => {
                    while self.expr(condition, env)? != 0 {
                        self.tick()?;
                        self.scoped(body, env)?;
                    }
                    0
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let start = self.expr(start, env)?;
                    let end = self.expr(end, env)?;
                    for i in start..end {
                        self.tick()?;
                        let mut inner = env.clone();
                        inner.insert(name.clone(), i);
                        self.body(body, &mut inner)?;
                        for (n, v) in env.iter_mut() {
                            if n != name {
                                *v = inner[n];
                            }
                        }
                    }
                    0
                }
            };
        }
        Ok(result)
    }
    fn scoped(&mut self, body: &[Statement], env: &mut BTreeMap<String, i64>) -> Result<i64> {
        let mut inner = env.clone();
        let value = self.body(body, &mut inner)?;
        for (n, v) in env.iter_mut() {
            *v = inner[n];
        }
        Ok(value)
    }
}
pub(crate) fn assign_binary(op: Assign) -> Binary {
    match op {
        Assign::FAdd => Binary::FAdd,
        Assign::FSub => Binary::FSub,
        Assign::FMul => Binary::FMul,
        Assign::FDiv => Binary::FDiv,
        Assign::Add => Binary::Add,
        Assign::Sub => Binary::Sub,
        Assign::Mul => Binary::Mul,
        Assign::Div => Binary::Div,
        Assign::Rem => Binary::Rem,
        Assign::Set => unreachable!(),
    }
}
pub use mira_core::vm::arithmetic;

pub fn execute(program: &Program, id: Id, inputs: &[i64]) -> Result<i64> {
    Prepared::new(program)?.call(id, inputs, &mut crate::data_runtime::Arena::default())
}
/// Reuse the same typed interpreter preparation for compiler-stage invocations.
/// This is not a contextual scheduler or a second execution engine.
pub(crate) struct Prepared {
    definitions: Vec<CodeModule>,
    types: Vec<data::Definition>,
    macros: BTreeSet<Id>,
}
impl Prepared {
    pub(crate) fn new(program: &Program) -> Result<Self> {
        program.require_complete()?;
        crate::access::validate_source(program)?;
        let runtime: Vec<_> = program
            .code_modules
            .iter()
            .filter(|d| !d.compile_time)
            .cloned()
            .collect();
        let definitions = procedures(&runtime, &program.data_types())?;
        let types = program.data_types();
        Ok(Self {
            definitions,
            types,
            macros: program
                .code_modules
                .iter()
                .filter(|m| m.compile_time)
                .map(|m| m.id)
                .collect(),
        })
    }
    pub(crate) fn call(
        &self,
        id: Id,
        inputs: &[i64],
        arena: &mut crate::data_runtime::Arena,
    ) -> Result<i64> {
        self.call_with_budget(id,inputs,arena,1_000_000)
    }
    #[cfg(test)]
    pub(crate) fn compiler_model(&self)->(&[CodeModule],&[data::Definition]){(&self.definitions,&self.types)}
    pub(crate) fn call_compiler(&self,id:Id,inputs:&[i64],arena:&mut crate::data_runtime::Arena)->Result<i64>{
        // Compiler trees use the VM's existing bounded recursion contract. Give
        // this bootstrap-only invocation its own finite host stack rather than
        // inheriting a small test/UI thread stack. Native/user budgets are unchanged.
        std::thread::scope(|scope|{
            std::thread::Builder::new().name("mira-compiler".into()).stack_size(32*1024*1024)
                .spawn_scoped(scope,||self.call_with_budget(id,inputs,arena,32_000_000))
                .map_err(|e|format!("compiler worker: {e}"))?
                .join().map_err(|_|"compiler worker panicked".to_string())?
        })
    }
    fn call_with_budget(&self,id:Id,inputs:&[i64],arena:&mut crate::data_runtime::Arena,steps:u64)->Result<i64>{
        if self.macros.contains(&id) {
            return Err("macro requires compile-time expansion".into());
        }
        Machine {
            data_types: &self.types,
            arena,
            definitions: &self.definitions,
            steps,
            depth: 0,
            owners: BTreeMap::new(),
            observed: BTreeMap::new(),
            graph_owners: BTreeMap::new(),
            graph_slots: BTreeMap::new(),
            ports: BTreeMap::new(),
            neural_active: false,
            neural_status: 0,
            external: None,
            structural: None,
            host_entries: Default::default(),
        }
        .call(id, inputs)
    }
}
pub(crate) fn execute_macro(
    program: &Program,
    id: Id,
    inputs: &[i64],
    stage: &mut dyn StructuralHost,
    fuel: &mut u64,
) -> Result<i64> {
    let mut modules = program.code_modules.clone();
    check_with(&mut modules, &BTreeMap::new(), &program.data_types())?;
    let definitions = procedures(&modules, &program.data_types())?;
    let types = program.data_types();
    let mut arena = crate::data_runtime::Arena::default();
    let mut machine = Machine {
        data_types: &types,
        arena: &mut arena,
        definitions: &definitions,
        steps: *fuel,
        depth: 0,
        owners: Default::default(),
        observed: Default::default(),
        graph_owners: Default::default(),
        graph_slots: Default::default(),
        ports: Default::default(),
        neural_active: false,
        neural_status: 0,
        external: None,
        structural: Some(stage),
        host_entries: Default::default(),
    };
    let result = machine.call(id, inputs);
    *fuel = machine.steps;
    result
}

pub(crate) fn execute_contextual(
    executable: &crate::composition::Executable,
    entry: Id,
    inputs: &[i64],
    arena: &mut crate::data_runtime::Arena,
    external: &mut dyn ExternalCalls,
    fuel: &mut u64,
    inherited_neural: bool,
) -> Result<i64> {
    let mut machine = Machine {
        data_types: &executable.data_types,
        arena,
        definitions: &executable.definitions,
        steps: *fuel,
        depth: 0,
        owners: executable
            .entries
            .iter()
            .map(|(cell, entry)| (*entry, *cell))
            .collect(),
        observed: BTreeMap::new(),
        graph_owners: executable.graph_entries.clone(),
        graph_slots: executable.graph_slots.clone(),
        ports: BTreeMap::new(),
        neural_active: inherited_neural,
        neural_status: 0,
        external: Some(external),
        structural: None,
        host_entries: executable.host_entries.clone(),
    };
    let result = machine.call(entry, inputs);
    *fuel = machine.steps;
    result
}
pub fn format(definition: &CodeModule) -> String {
    format_with_types(definition, &collect_types(std::slice::from_ref(definition)))
}
pub fn format_with_types(definition: &CodeModule, types: &[data::Definition]) -> String {
    let mut s = String::new();
    format_definition(definition, 0, true, types, &mut s);
    s
}
/// Format a source identifier or qualified call without confusing '-' with subtraction.
pub fn quote_identifier(name: &str) -> String {
    name.split('.')
        .map(|part| {
            if part.contains('-') {
                format!("`{part}`")
            } else {
                part.into()
            }
        })
        .collect::<Vec<String>>()
        .join(".")
}
pub fn type_name(t: Type, types: &[data::Definition]) -> String {
    match t {
        Type::Infer => "_".into(),
        Type::F32 => "f32".into(),
        Type::I64 => "i64".into(),
        Type::Bool => "bool".into(),
        Type::Event => "event".into(),
        Type::Unit => "unit".into(),
        Type::Addr => "addr".into(),
        Type::Data(id) => data::find(types, id)
            .map_or_else(|_| format!("data_{id}"), |d| quote_identifier(&d.name)),
    }
}
pub(crate) fn format_data(
    d: &data::Definition,
    depth: usize,
    ids: bool,
    types: &[data::Definition],
    out: &mut String,
) {
    use std::fmt::Write;
    let pad = "  ".repeat(depth);
    let id = if ids {
        format!(" @{}", d.id)
    } else {
        String::new()
    };
    match &d.kind {
        data::Kind::Tensor { element, shape } => {
            writeln!(
                out,
                "{pad}type {}{id} = tensor[{}, {}]",
                quote_identifier(&d.name),
                type_name(*element, types),
                shape
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .unwrap();
        }
        data::Kind::Record(fields) => {
            writeln!(out, "{pad}struct {}{id}:", quote_identifier(&d.name)).unwrap();
            for field in fields {
                writeln!(
                    out,
                    "{pad}  {}: {}",
                    quote_identifier(&field.name),
                    type_name(field.ty, types)
                )
                .unwrap();
            }
        }
        data::Kind::Array { element, length } => {
            writeln!(
                out,
                "{pad}type {}{id} = [{}, {length}]",
                quote_identifier(&d.name),
                type_name(*element, types)
            )
            .unwrap();
        }
    }
}
fn format_definition(
    d: &CodeModule,
    depth: usize,
    ids: bool,
    types: &[data::Definition],
    out: &mut String,
) {
    use std::fmt::Write;
    let pad = "  ".repeat(depth);
    let params = d
        .parameters
        .iter()
        .map(|p| {
            if !ids && p.ty == Type::I64 {
                quote_identifier(&p.name)
            } else {
                format!("{}: {}", quote_identifier(&p.name), type_name(p.ty, types))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "{pad}{}{}{}({params}){} =",
        if d.compile_time { "macro " } else { "" },
        quote_identifier(&d.name),
        if ids {
            format!(" @{}", d.id)
        } else {
            String::new()
        },
        if d.output_ports {
            format!(" -> ports {}", type_name(d.output, types))
        } else if d.annotated {
            format!(" -> {}", type_name(d.output, types))
        } else {
            String::new()
        }
    )
    .unwrap();
    for ty in &d.data_types {
        format_data(ty, depth + 1, ids, types, out);
    }
    for helper in &d.locals {
        format_definition(helper, depth + 1, ids, types, out);
    }
    format_block(&d.body, depth + 1, types, out);
}
fn expr_text(e: &Expr, types: &[data::Definition]) -> String {
    match e {
        Expr::Hole(name) => format!("?{name}"),
        Expr::Float(bits) => format!("{:?}", f32::from_bits(*bits)),
        Expr::Int(n) => n.to_string(),
        Expr::Bool(v) => v.to_string(),
        Expr::Unit => "()".into(),
        Expr::Name(n) => quote_identifier(n),
        Expr::Construct(id, args)
            if crate::tensor::text_type(*id, args, types).is_some_and(|r| r.is_ok()) =>
        {
            let Expr::Call(_, bytes) = &args[0] else {
                unreachable!()
            };
            let bytes: Vec<u8> = bytes
                .iter()
                .map(|e| if let Expr::Int(n) = e { *n as u8 } else { 0 })
                .collect();
            format!(
                "{}({})",
                type_name(Type::Data(*id), types),
                serde_json::to_string(&String::from_utf8(bytes).expect("validated literal"))
                    .unwrap()
            )
        }
        Expr::Construct(id, args) => format!(
            "{}({})",
            type_name(Type::Data(*id), types),
            args.iter()
                .map(|e| expr_text(e, types))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::View(id, address) => format!(
            "{}.at({})",
            type_name(Type::Data(*id), types),
            expr_text(address, types)
        ),
        Expr::Field(base, name) => {
            format!("({}).{}", expr_text(base, types), quote_identifier(name))
        }
        Expr::Index(base, index) => {
            format!("({})[{}]", expr_text(base, types), expr_text(index, types))
        }
        Expr::Unary(op, e) => format!(
            "{}({})",
            match op {
                Unary::Neg | Unary::FNeg => "-",
                Unary::Not => "not ",
                Unary::Plus => "+",
                Unary::BitNot => "~",
            },
            expr_text(e, types)
        ),
        Expr::Binary(op, a, b) => format!(
            "({} {} {})",
            expr_text(a, types),
            match op {
                Binary::Add | Binary::FAdd => "+",
                Binary::Sub | Binary::FSub => "-",
                Binary::Mul | Binary::FMul => "*",
                Binary::Div | Binary::FDiv => "/",
                Binary::Rem => "%",
                Binary::Eq | Binary::FEq => "==",
                Binary::Ne | Binary::FNe => "!=",
                Binary::Lt | Binary::FLt => "<",
                Binary::Le | Binary::FLe => "<=",
                Binary::Gt | Binary::FGt => ">",
                Binary::Ge | Binary::FGe => ">=",
                Binary::And => "and",
                Binary::Or => "or",
                Binary::BitAnd => "&",
                Binary::BitOr => "|",
                Binary::BitXor => "^",
                Binary::Shl => "<<",
                Binary::Shr => ">>",
            },
            expr_text(b, types)
        ),
        Expr::Choose(c, a, b) => format!(
            "({} ? {} : {})",
            expr_text(c, types),
            expr_text(a, types),
            expr_text(b, types)
        ),
        Expr::Call(n, args) if n.contains(".offset_of.") && args.is_empty() => {
            let (base, field) = n.split_once(".offset_of.").unwrap();
            format!(
                "{}.offset_of({})",
                quote_identifier(base),
                quote_identifier(field)
            )
        }
        Expr::Call(n, args) => format!(
            "{}({})",
            quote_identifier(n),
            args.iter()
                .map(|e| expr_text(e, types))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn assign_text(op: Assign) -> &'static str {
    match op {
        Assign::Set => "=",
        Assign::Add | Assign::FAdd => "+=",
        Assign::Sub | Assign::FSub => "-=",
        Assign::Mul | Assign::FMul => "*=",
        Assign::Div | Assign::FDiv => "/=",
        Assign::Rem => "%=",
    }
}
fn format_block(body: &[Statement], depth: usize, types: &[data::Definition], out: &mut String) {
    use std::fmt::Write;
    let pad = "  ".repeat(depth);
    for stmt in body {
        match stmt {
            Statement::Expr(e) => {
                writeln!(out, "{pad}{}", expr_text(e, types)).unwrap();
            }
            Statement::Set { target, op, value } => {
                writeln!(
                    out,
                    "{pad}{} {} {}",
                    expr_text(target, types),
                    assign_text(*op),
                    expr_text(value, types)
                )
                .unwrap();
            }
            Statement::Bind {
                name,
                ty,
                immutable,
                op,
                value,
            } => {
                writeln!(
                    out,
                    "{pad}{}{}{} {} {}",
                    if *immutable { "const " } else { "" },
                    quote_identifier(name),
                    ty.map_or(String::new(), |t| format!(": {}", type_name(t, types))),
                    assign_text(*op),
                    expr_text(value, types)
                )
                .unwrap();
            }
            Statement::If { condition, yes, no } => {
                writeln!(out, "{pad}if {}", expr_text(condition, types)).unwrap();
                format_block(yes, depth + 1, types, out);
                let mut tail=no.as_slice();
                while let [Statement::If{condition,yes,no}]=tail {
                    writeln!(out,"{pad}else if {}",expr_text(condition,types)).unwrap();
                    format_block(yes,depth+1,types,out);
                    tail=no;
                }
                if !tail.is_empty() {
                    writeln!(out, "{pad}else").unwrap();
                    format_block(tail, depth + 1, types, out);
                }
            }
            Statement::While { condition, body } => {
                writeln!(out, "{pad}while {}", expr_text(condition, types)).unwrap();
                format_block(body, depth + 1, types, out);
            }
            Statement::For {
                name,
                start,
                end,
                body,
            } => {
                writeln!(
                    out,
                    "{pad}for {} in {}..{}",
                    quote_identifier(name),
                    expr_text(start, types),
                    expr_text(end, types)
                )
                .unwrap();
                format_block(body, depth + 1, types, out);
            }
        }
    }
}

/// Compile a cell's edited body with its existing input interface; no second language.
pub fn edit_body(program: &mut Program, cell: Id, text: &str) -> Result<()> {
    let c = program.root.find_cell(cell).ok_or("cell missing")?.clone();
    let input_ports = if let Definition::Adaptive(id) = c.definition {
        mira_core::code::find(&program.code_modules, id)
            .ok_or("adaptive behavior missing")?
            .parameters
            .iter()
            .map(|p| Port {
                name: p.name.as_str(),
                ty: p.ty,
                direction: Direction::In,
            })
            .collect()
    } else {
        program.ports(&c)
    };
    let parameters: Vec<_> = input_ports
        .iter()
        .filter(|p| p.direction == Direction::In)
        .map(|p| {
            format!(
                "{}: {}",
                if p.name == "in" && !matches!(c.definition, Definition::Adaptive(_)) {
                    "value"
                } else {
                    p.name
                },
                type_name(p.ty, &program.data_types())
            )
        })
        .collect();
    let blank_draft = matches!(c.definition, Definition::Code(id) if program.code_modules.iter().any(|m| m.id == id && m.body.is_empty() && !m.annotated));
    let text = text.trim();
    if text.is_empty() && blank_draft {
        return Ok(());
    }
    if text.is_empty() {
        return Err("cell body cannot be empty".into());
    }
    // Simple operators remain cheap intrinsic definitions and keep their original ports.
    if !matches!(c.definition, Definition::Adaptive(_)) && !text.contains('\n') {
        if let Ok(candidate) = crate::syntax::parse(&format!("space edit:\n  cell = {text}\n")) {
            if let Some(replacement) = candidate.root.cells.first() {
                fn set(s: &mut Space, id: Id, d: Definition, v: i64) -> bool {
                    if let Some(c) = s.cells.iter_mut().find(|c| c.id == id) {
                        c.definition = d;
                        c.parameter = v;
                        true
                    } else {
                        s.cells
                            .iter_mut()
                            .filter_map(|c| c.space.as_mut())
                            .any(|s| set(s, id, d, v))
                    }
                }
                let previous: Vec<_> = program
                    .ports(&c)
                    .iter()
                    .filter(|p| p.direction == Direction::In)
                    .map(|p| p.name.to_string())
                    .collect();
                let next: Vec<_> = replacement
                    .definition
                    .ports()
                    .iter()
                    .filter(|p| p.direction == Direction::In)
                    .map(|p| p.name.to_string())
                    .collect();
                fn remap(s: &mut Space, id: Id, old: &[String], new: &[String]) {
                    if old.len() == new.len() {
                        for l in &mut s.links {
                            if l.to.cell == id {
                                if let Some(i) = old.iter().position(|n| n == &l.to.port) {
                                    l.to.port = new[i].clone();
                                }
                            }
                        }
                    }
                    for c in &mut s.cells {
                        if c.id == id {
                            if let Some(inner) = &mut c.space {
                                if old.len() == new.len() {
                                    for p in &mut inner.inputs {
                                        if let Some(i) = old.iter().position(|n| n == &p.name) {
                                            let before = p.name.clone();
                                            p.name = new[i].clone();
                                            for l in &mut inner.links {
                                                if l.from.cell == inner.id && l.from.port == before
                                                {
                                                    l.from.port = p.name.clone();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(inner) = &mut c.space {
                            remap(inner, id, old, new);
                        }
                    }
                }
                remap(&mut program.root, cell, &previous, &next);
                set(
                    &mut program.root,
                    cell,
                    replacement.definition,
                    replacement.parameter,
                );
                crate::composition::infer(program)?;
                return Ok(());
            }
        }
    }
    let existing = if let Definition::Code(id) | Definition::Adaptive(id) = c.definition {
        Some(id)
    } else {
        None
    };
    let id = existing.unwrap_or(program.next_id()?);
    let name = if let Some(id) = existing {
        mira_core::code::find(&program.code_modules, id)
            .ok_or("behavior absent")?
            .name
            .clone()
    } else {
        format!("cell_{cell}_behavior")
    };
    let output = program
        .ports(&c)
        .iter()
        .find(|p| p.direction == Direction::Out)
        .map_or(Type::Unit, |p| p.ty);
    let global_text = program
        .global_types
        .iter()
        .map(|t| {
            let mut s = String::new();
            format_data(t, 0, true, &program.data_types(), &mut s);
            s
        })
        .collect::<String>();
    let source = format!(
        "identities {}\n{global_text}{}{name} @{id}({}){} =\n{}\n",
        program.next_id()?,
        program
            .code_modules
            .iter()
            .filter(|d| d.id != id)
            .map(|d| format_with_types(d, &program.data_types()))
            .collect::<Vec<_>>()
            .join("\n"),
        parameters.join(", "),
        if blank_draft {
            String::new()
        } else {
            format!(" -> {}", type_name(output, &program.data_types()))
        },
        text.lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let parsed = crate::frontend::parse_template(&source)?;
    let mut definition = parsed
        .code_modules
        .into_iter()
        .find(|d| d.name == name)
        .ok_or("behavior missing")?;
    // Keep data identities stable when an existing code module is edited.
    let old_types = program.data_types();
    let new_types = collect_types(std::slice::from_ref(&definition));
    let ids: BTreeMap<Id, Id> = new_types
        .iter()
        .filter_map(|d| {
            old_types
                .iter()
                .find(|o| o.name == d.name)
                .map(|o| (d.id, o.id))
        })
        .collect();
    fn remap_type(t: &mut Type, ids: &BTreeMap<Id, Id>) {
        if let Type::Data(id) = t {
            if let Some(old) = ids.get(id) {
                *id = *old;
            }
        }
    }
    fn remap_expr(e: &mut Expr, ids: &BTreeMap<Id, Id>) {
        match e {
            Expr::Construct(id, args) => {
                if let Some(old) = ids.get(id) {
                    *id = *old;
                }
                for arg in args {
                    remap_expr(arg, ids);
                }
            }
            Expr::View(id, e) => {
                if let Some(old) = ids.get(id) {
                    *id = *old;
                }
                remap_expr(e, ids);
            }
            Expr::Field(e, _) | Expr::Unary(_, e) => remap_expr(e, ids),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                remap_expr(a, ids);
                remap_expr(b, ids);
            }
            Expr::Choose(c, a, b) => {
                remap_expr(c, ids);
                remap_expr(a, ids);
                remap_expr(b, ids);
            }
            Expr::Call(_, args) => {
                for arg in args {
                    remap_expr(arg, ids);
                }
            }
            _ => {}
        }
    }
    fn remap_body(body: &mut [Statement], ids: &BTreeMap<Id, Id>) {
        for s in body {
            match s {
                Statement::Expr(e) => remap_expr(e, ids),
                Statement::Set { target, value, .. } => {
                    remap_expr(target, ids);
                    remap_expr(value, ids);
                }
                Statement::Bind { ty, value, .. } => {
                    if let Some(t) = ty {
                        remap_type(t, ids);
                    }
                    remap_expr(value, ids);
                }
                Statement::If { condition, yes, no } => {
                    remap_expr(condition, ids);
                    remap_body(yes, ids);
                    remap_body(no, ids);
                }
                Statement::While { condition, body } => {
                    remap_expr(condition, ids);
                    remap_body(body, ids);
                }
                Statement::For {
                    start, end, body, ..
                } => {
                    remap_expr(start, ids);
                    remap_expr(end, ids);
                    remap_body(body, ids);
                }
            }
        }
    }
    fn remap_module(d: &mut CodeModule, ids: &BTreeMap<Id, Id>) {
        for ty in &mut d.data_types {
            if let Some(old) = ids.get(&ty.id) {
                ty.id = *old;
            }
            match &mut ty.kind {
                data::Kind::Record(fields) => {
                    for p in fields {
                        remap_type(&mut p.ty, ids);
                    }
                }
                data::Kind::Array { element, .. } | data::Kind::Tensor { element, .. } => {
                    remap_type(element, ids)
                }
            }
        }
        for p in &mut d.parameters {
            remap_type(&mut p.ty, ids);
        }
        remap_type(&mut d.output, ids);
        remap_body(&mut d.body, ids);
        for helper in &mut d.locals {
            remap_module(helper, ids);
        }
    }
    remap_module(&mut definition, &ids);
    definition.id = id;
    fn preserve_helpers(old: &[CodeModule], new: &mut [CodeModule]) {
        for d in new {
            if let Some(previous) = old.iter().find(|p| p.name == d.name) {
                d.id = previous.id;
                preserve_helpers(&previous.locals, &mut d.locals);
            }
        }
    }
    if let Some(previous) = mira_core::code::find(&program.code_modules, id) {
        preserve_helpers(&previous.locals, &mut definition.locals);
    }
    // Calls may reference definitions already present in the containing program.
    let mut definitions = program.code_modules.clone();
    definitions.retain(|d| d.id != id);
    definitions.push(definition);
    fn replace(s: &mut Space, id: Id, definition: Id, adaptive: bool) {
        if let Some(c) = s.cells.iter_mut().find(|c| c.id == id) {
            c.definition = if adaptive {
                Definition::Adaptive(definition)
            } else {
                Definition::Code(definition)
            };
            c.parameter = 0;
        }
        for c in &mut s.cells {
            if let Some(inner) = &mut c.space {
                replace(inner, id, definition, adaptive);
            }
        }
    }
    // Existing 'in' wires refer to the implicit input named value in the code context.
    fn rename(s: &mut Space, id: Id) {
        for l in &mut s.links {
            if l.to.cell == id && l.to.port == "in" {
                l.to.port = "value".into();
            }
        }
        for c in &mut s.cells {
            if let Some(inner) = &mut c.space {
                rename(inner, id);
            }
        }
    }
    if !matches!(c.definition, Definition::Adaptive(_)) {
        rename(&mut program.root, cell);
    }
    replace(
        &mut program.root,
        cell,
        id,
        matches!(c.definition, Definition::Adaptive(_)),
    );
    program.code_modules = definitions;
    // Editing can introduce a behavior, local helpers and data types. Reserve all
    // their identities before inference validates the resulting program.
    program.next_identity = program.next_id()?;
    crate::composition::infer(program)?;
    Ok(())
}

/// A draft is editor text only: opening it neither allocates an identity nor
/// changes the cell's execution. Applying it installs the algorithm explicitly.
pub fn definition_draft(program: &Program, cell: Id) -> Result<String> {
    let c = program.root.find_cell(cell).ok_or("cell missing")?;
    let inner = c.space.as_ref().ok_or("cell has no internal space")?;
    let types = program.data_types();
    let mut name = format!("{}_behavior", c.name);
    while program.code_modules.iter().any(|m| m.name == name) {
        name.push('_');
    }
    let params = inner
        .inputs
        .iter()
        .map(|p| format!("{}: {}", quote_identifier(&p.name), type_name(p.ty, &types)))
        .collect::<Vec<_>>()
        .join(", ");
    if inner.outputs.len() > 1 || inner.outputs.first().is_some_and(|p| p.name != "out") {
        // This is editor text only: opening it neither replaces the Graph nor
        // allocates identities. The user supplies an algorithm before Apply.
        let mut result_name = format!("{}_result", c.name);
        while types.iter().any(|t| t.name == result_name) {
            result_name.push('_');
        }
        let result_name = quote_identifier(&result_name);
        let mut source = format!(
            "{}({params}) -> ports {result_name} =\n  struct {result_name}:\n",
            quote_identifier(&name)
        );
        for port in &inner.outputs {
            source.push_str(&format!(
                "    {}: {}\n",
                quote_identifier(&port.name),
                type_name(port.ty, &types)
            ));
        }
        source.push_str(&format!(
            "\n  // Напишите алгоритм и верните {result_name}({}).\n  // Поля результата станут выходными портами ячейки.\n",
            inner.outputs.iter().map(|p| quote_identifier(&p.name)).collect::<Vec<_>>().join(", ")
        ));
        return Ok(source);
    }
    let output = inner.outputs.first().map_or(Type::Unit, |p| p.ty);
    let body = if let Some(p) = inner.outputs.first() {
        let args = inner
            .inputs
            .iter()
            .map(|p| quote_identifier(&p.name))
            .collect::<Vec<_>>()
            .join(", ");
        format!("  graph.{}({args})\n", quote_identifier(&p.name))
    } else {
        String::new()
    };
    Ok(format!(
        "{}({params}) -> {} =\n{body}",
        quote_identifier(&name),
        type_name(output, &types)
    ))
}

/// Edit one complete behavior, including its typed interface, inside its program.
/// The containing editor transaction provides rollback on validation failure.
pub fn edit_definition(program: &mut Program, cell: Id, source: &str) -> Result<()> {
    let c = program.root.find_cell(cell).ok_or("cell missing")?;
    let existing = match c.definition {
        Definition::Code(id) | Definition::Adaptive(id) => Some(id),
        Definition::Graph => None,
        _ => return Err("cell has no editable behavior".into()),
    };
    let id = if let Some(id) = existing {
        id
    } else {
        program.allocate_id()?
    };
    let others: Vec<_> = program.code_modules.iter().filter(|m| m.id != id).collect();
    let mut text = format!("identities {}\n", program.next_id()?);
    for ty in &program.global_types {
        format_data(ty, 0, true, &program.data_types(), &mut text);
    }
    for module in &others {
        text.push_str(&format_with_types(module, &program.data_types()));
    }
    text.push_str(source);
    text.push('\n');
    let mut parsed = crate::frontend::parse_template(&text)?;
    let replacements: Vec<_> = parsed
        .code_modules
        .iter()
        .enumerate()
        .filter(|(_, m)| !others.iter().any(|old| old.id == m.id))
        .map(|(i, _)| i)
        .collect();
    if replacements.len() != 1 {
        return Err(
            "Edit exactly one cell definition; use project definitions to add shared functions"
                .into(),
        );
    }
    // The cell continues to refer to the same behavior even if its name changes.
    parsed.code_modules[replacements[0]].id = id;
    if existing.is_none() {
        program.cell_mut(cell).ok_or("cell missing")?.definition = Definition::Code(id);
    }
    program.code_modules = parsed.code_modules;
    program.next_identity = program.next_id()?.max(parsed.next_identity);
    crate::composition::infer(program)?;
    program.validate()
}

/// The IDE displays the same intrinsic signatures used by the type checker.
pub fn system_operations() -> Vec<(String, Vec<Type>, Type)> {
    [
        "sys.addr",
        "sys.address_value",
        "sys.offset",
        "sys.align_up",
        "sys.address_distance",
        "sys.memory_copy",
        "sys.memory_fill",
        "sys.read_cr0",
        "sys.read_cr4",
        "sys.write_cr0",
        "sys.write_cr4",
        "sys.read_msr",
        "sys.write_msr",
        "sys.ltr",
        "sys.swapgs",
        "sys.acquire_fence",
        "sys.release_fence",
        "sys.load8",
        "sys.load16",
        "sys.load32",
        "sys.load64",
        "sys.store8",
        "sys.store16",
        "sys.store32",
        "sys.store64",
        "sys.atomic_load64",
        "sys.atomic_store64",
        "sys.atomic_exchange64",
        "sys.atomic_compare_exchange64",
        "sys.atomic_fetch_add64",
        "sys.memory_fence",
        "sys.in8",
        "sys.in16",
        "sys.in32",
        "sys.out8",
        "sys.out16",
        "sys.out32",
        "sys.boot_info",
        "sys.halt",
        "sys.cli",
        "sys.sti",
        "sys.hlt",
        "sys.sti_hlt",
        "sys.pause",
        "sys.breakpoint",
        "sys.next_tick",
        "sys.current_tick",
        "sys.flags",
        "sys.cs",
        "sys.cpuid_eax",
        "sys.cpuid_ebx",
        "sys.cpuid_ecx",
        "sys.cpuid_edx",
        "sys.cpu_local",
        "sys.set_cpu_local",
        "sys.read_cr2",
        "sys.read_cr3",
        "sys.write_cr3",
        "sys.invlpg",
        "sys.lidt",
        "sys.sidt",
        "sys.lgdt",
        "sys.sgdt",
    ]
    .into_iter()
    .map(|name| {
        let (inputs, output) =
            crate::intrinsics::signature(name).expect("system operation is typed");
        (name.into(), inputs, output)
    })
    .chain(
        [
            "sys.atomic_load64",
            "sys.atomic_store64",
            "sys.atomic_exchange64",
            "sys.atomic_fetch_add64",
            "sys.atomic_compare_exchange64",
        ]
        .into_iter()
        .flat_map(|base| {
            [8, 16, 32, 64].into_iter().flat_map(move |width| {
                [
                    "", "_relaxed", "_acquire", "_release", "_acq_rel", "_seq_cst",
                ]
                .into_iter()
                .filter_map(move |order| {
                    if width == 64 && order.is_empty() {
                        return None;
                    }
                    let name = format!("{}{width}{order}", base.strip_suffix("64").unwrap());
                    crate::intrinsics::signature(&name).map(|(args, result)| (name, args, result))
                })
            })
        }),
    )
    .chain(
        crate::intrinsics::INTEGER_OPERATIONS
            .iter()
            .map(|(name, arity)| ((*name).into(), vec![Type::I64; *arity], Type::I64)),
    )
    .chain(
        crate::reflection::QUERIES
            .iter()
            .map(|(name, arity)| ((*name).into(), vec![Type::I64; *arity], Type::I64)),
    )
    .collect()
}
