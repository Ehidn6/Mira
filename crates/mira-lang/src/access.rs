//! Conservative shared-memory write analysis on the existing typed behavior IR.
//! Aggregate references are tracked through parameters, results and constructors.
use mira_core::{code::*, *};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    Shared,
    Parameter(usize),
}
type Origins = BTreeSet<Origin>;
type Env = BTreeMap<String, (Type, Origins)>;
#[derive(Clone, Default, PartialEq, Eq)]
struct Summary {
    returned: Origins,
    written: Origins,
    forbidden: BTreeSet<String>,
}
impl Summary {
    fn merge(&mut self, other: &Self) {
        self.returned.extend(&other.returned);
        self.written.extend(&other.written);
        self.forbidden.extend(other.forbidden.iter().cloned());
    }
}
struct Analysis<'a> {
    defs: &'a [CodeModule],
    types: &'a [data::Definition],
    known: &'a BTreeMap<Id, Summary>,
    budget: &'a mut usize,
    result: Summary,
}
fn substitute(source: &Origins, args: &[Origins]) -> Origins {
    let mut out = Origins::new();
    for item in source {
        match item {
            Origin::Shared => {
                out.insert(*item);
            }
            Origin::Parameter(i) => {
                if let Some(arg) = args.get(*i) {
                    out.extend(arg);
                } else {
                    out.insert(Origin::Shared);
                }
            }
        }
    }
    out
}
fn merge_outer(env: &mut Env, other: &Env) {
    for (name, (_, origins)) in env {
        if let Some((_, more)) = other.get(name) {
            origins.extend(more);
        }
    }
}
impl Analysis<'_> {
    fn step(&mut self) -> Result<()> {
        *self.budget = self
            .budget
            .checked_sub(1)
            .ok_or("read-only access analysis budget exceeded")?;
        Ok(())
    }
    fn ty(&self, e: &Expr, env: &Env) -> Result<Type> {
        crate::behavior::expression_type(
            e,
            &env.iter().map(|(n, (t, _))| (n.clone(), *t)).collect(),
            self.defs,
            self.types,
        )
    }
    fn expr(&mut self, e: &Expr, env: &Env) -> Result<Origins> {
        self.step()?;
        let mut out = Origins::new();
        match e {
            Expr::Name(n) => {
                if let Some((_, o)) = env.get(n) {
                    out.extend(o);
                }
            }
            Expr::Construct(_, args) => {
                for a in args {
                    out.extend(self.expr(a, env)?);
                }
            }
            Expr::View(_, a) => {
                self.expr(a, env)?;
                out.insert(Origin::Shared);
                self.result.forbidden.insert("raw memory view".into());
            }
            Expr::Field(base, _) => {
                let o = self.expr(base, env)?;
                if matches!(self.ty(e, env)?, Type::Data(_)) {
                    out = o;
                }
            }
            Expr::Index(base, index) => {
                let o = self.expr(base, env)?;
                self.expr(index, env)?;
                if matches!(self.ty(e, env)?, Type::Data(_)) {
                    out = o;
                }
            }
            Expr::Choose(c, a, b) => {
                self.expr(c, env)?;
                out.extend(self.expr(a, env)?);
                out.extend(self.expr(b, env)?);
            }
            Expr::Unary(_, a) => {
                self.expr(a, env)?;
            }
            Expr::Binary(_, a, b) => {
                self.expr(a, env)?;
                self.expr(b, env)?;
            }
            Expr::Call(name, args) => {
                let scoped = matches!(name.as_str(), "sys.scratch" | "sys.read_only");
                let target = if scoped {
                    match args.first() {
                        Some(Expr::Name(n)) => n,
                        _ => return Err("scope entry requires a symbol".into()),
                    }
                } else {
                    name
                };
                let origins = args
                    .iter()
                    .skip(usize::from(scoped))
                    .map(|a| self.expr(a, env))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(d) = self.defs.iter().find(|d| d.name == *target) {
                    let summary = self.known.get(&d.id).cloned().unwrap_or_default();
                    // The first evaluated scoped argument is its byte limit.
                    let actual = if scoped { &origins[1..] } else { origins.as_slice() };
                    self.result
                        .written
                        .extend(substitute(&summary.written, actual));
                    self.result.forbidden.extend(summary.forbidden);
                    if matches!(d.output, Type::Data(_)) {
                        out = substitute(&summary.returned, actual);
                    }
                } else if name.starts_with("#buffer.") {
                    out.insert(Origin::Shared);
                } else if !matches!(
                    name.as_str(),
                    "math.sqrt"
                        | "math.to_f32"
                        | "math.bits"
                        | "math.from_bits"
                        | "#f32.bits"
                        | "#f32.from_bits"
                        | "#graph.require"
                        | "sys.neural_status"
                        | "sys.current_tick"
                ) && !name.starts_with("#data.identity.")
                    && !crate::intrinsics::pure_integer(name)
                    && crate::intrinsics::aligned_type(name).is_none()
                    && !crate::intrinsics::pure_address(name)
                {
                    self.result.forbidden.insert(name.clone());
                }
            }
            _ => {}
        }
        Ok(out)
    }
    fn body(&mut self, body: &[Statement], env: &mut Env) -> Result<Origins> {
        let mut returned = Origins::new();
        for s in body {
            self.step()?;
            returned.clear();
            match s {
                Statement::Expr(e) => returned = self.expr(e, env)?,
                Statement::Bind { name, value, .. } => {
                    let ty = self.ty(value, env)?;
                    let origins = self.expr(value, env)?;
                    env.entry(name.clone())
                        .or_insert((ty, Origins::new()))
                        .1
                        .extend(origins);
                }
                Statement::Set { target, value, .. } => {
                    let base = match target {
                        Expr::Field(base, _) | Expr::Index(base, _) => base.as_ref(),
                        _ => return Err("invalid assignment target".into()),
                    };
                    let destination = self.expr(base, env)?;
                    self.expr(target, env)?;
                    self.expr(value, env)?;
                    self.result.written.extend(destination);
                    // Reference rewiring needs heap-shape analysis. Reject it in this
                    // initial read-only profile, including writes through helper aliases.
                    if matches!(self.ty(value, env)?, Type::Data(_)) {
                        self.result
                            .forbidden
                            .insert("aggregate reference reassignment".into());
                    }
                }
                Statement::If { condition, yes, no } => {
                    self.expr(condition, env)?;
                    for body in [yes, no] {
                        let mut branch = env.clone();
                        returned.extend(self.body(body, &mut branch)?);
                        merge_outer(env, &branch);
                    }
                }
                Statement::While { condition, body } => loop {
                    let before = env.clone();
                    self.expr(condition, env)?;
                    let mut inner = env.clone();
                    self.body(body, &mut inner)?;
                    merge_outer(env, &inner);
                    if *env == before {
                        break;
                    }
                },
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    self.expr(start, env)?;
                    self.expr(end, env)?;
                    loop {
                        let before = env.clone();
                        let mut inner = env.clone();
                        inner.insert(name.clone(), (Type::I64, Origins::new()));
                        self.body(body, &mut inner)?;
                        merge_outer(env, &inner);
                        if *env == before {
                            break;
                        }
                    }
                }
            }
        }
        Ok(returned)
    }
}
pub(crate) fn verify_read_only(
    name: &str,
    defs: &[CodeModule],
    types: &[data::Definition],
) -> Result<()> {
    crate::effects::verify_scratch(name, defs, types)?;
    let entry = defs
        .iter()
        .find(|d| d.name == name)
        .ok_or("read-only entry missing")?
        .id;
    let mut reachable = BTreeSet::new();
    let mut pending = vec![entry];
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let d = defs
            .iter()
            .find(|d| d.id == id)
            .ok_or("effect analysis entry missing")?;
        walk_body(&d.body, &mut |e| {
            if let Expr::Call(n, _) | Expr::Name(n) = e {
                if let Some(callee) = defs.iter().find(|d| d.name == *n) {
                    pending.push(callee.id);
                }
            }
        });
    }
    let mut known = BTreeMap::<Id, Summary>::new();
    let mut budget = 1_000_000usize;
    loop {
        let mut changed = false;
        for d in defs.iter().filter(|d| reachable.contains(&d.id)) {
            let mut analysis = Analysis {
                defs,
                types,
                known: &known,
                budget: &mut budget,
                result: Summary::default(),
            };
            let mut env = d
                .parameters
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        p.name.clone(),
                        (
                            p.ty,
                            if matches!(p.ty, Type::Data(_)) {
                                BTreeSet::from([Origin::Parameter(i)])
                            } else {
                                Origins::new()
                            },
                        ),
                    )
                })
                .collect();
            let returned = analysis.body(&d.body, &mut env)?;
            if matches!(d.output, Type::Data(_)) {
                analysis.result.returned = returned;
            }
            let mut next = analysis.result;
            let previous = known.get(&d.id).cloned().unwrap_or_default();
            next.merge(&previous);
            if next != previous {
                changed = true;
            }
            known.insert(d.id, next);
        }
        if !changed {
            break;
        }
    }
    let result = known.get(&entry).ok_or("read-only entry summary missing")?;
    if let Some(effect) = result.forbidden.first() {
        return Err(format!("read-only entry forbids effect: {effect}"));
    }
    if !result.written.is_empty() {
        return Err("read-only entry may modify shared data through an alias".into());
    }
    Ok(())
}

fn walk_body(body: &[Statement], f: &mut impl FnMut(&Expr)) {
    fn expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
        f(e);
        match e {
            Expr::Call(_, args) | Expr::Construct(_, args) => {
                for a in args {
                    expr(a, f);
                }
            }
            Expr::Unary(_, a) | Expr::View(_, a) | Expr::Field(a, _) => expr(a, f),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                expr(a, f);
                expr(b, f);
            }
            Expr::Choose(c, a, b) => {
                expr(c, f);
                expr(a, f);
                expr(b, f);
            }
            _ => {}
        }
    }
    for s in body {
        match s {
            Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e, f),
            Statement::Set { target, value, .. } => {
                expr(target, f);
                expr(value, f);
            }
            Statement::If { condition, yes, no } => {
                expr(condition, f);
                walk_body(yes, f);
                walk_body(no, f);
            }
            Statement::While { condition, body } => {
                expr(condition, f);
                walk_body(body, f);
            }
            Statement::For {
                start, end, body, ..
            } => {
                expr(start, f);
                expr(end, f);
                walk_body(body, f);
            }
        }
    }
}
pub(crate) fn validate(defs: &[CodeModule], types: &[data::Definition]) -> Result<()> {
    let mut entries = BTreeSet::new();
    for d in defs {
        walk_body(&d.body, &mut |e| {
            if let Expr::Call(n, args) = e {
                if n == "sys.read_only" {
                    if let Some(Expr::Name(target)) = args.first() {
                        entries.insert(target.clone());
                    }
                }
            }
        });
    }
    for entry in entries {
        verify_read_only(&entry, defs, types)?;
    }
    Ok(())
}

/// Serialized source programs must not impersonate compiler-generated operations.
pub(crate) fn validate_source(program: &Program) -> Result<()> {
    fn modules(defs: &[CodeModule]) -> Result<()> {
        for d in defs {
            let mut private = false;
            walk_body(&d.body, &mut |e| {
                if let Expr::Call(n, _) | Expr::Name(n) = e {
                    private |= n.starts_with('#');
                }
            });
            if private {
                return Err(
                    "source program contains a compiler-private operation or reference".into(),
                );
            }
            modules(&d.locals)?;
        }
        Ok(())
    }
    modules(&program.code_modules)
}
