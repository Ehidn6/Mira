//! Source constraints over the existing Program/Expr model. Model proposals
//! narrow variables; they cannot bypass these same deterministic constraints.
use mira_core::{code::*, *};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
type Var = usize;
type Domain = BTreeSet<Type>;
const LIMIT: usize = 16_384;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Parameter {
        definition: Id,
        index: usize,
    },
    Result {
        definition: Id,
    },
    Hole {
        definition: Id,
        path: Vec<usize>,
        label: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub target: Target,
    pub ty: Type,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Requirement {
    pub target: Target,
    pub candidates: Vec<Type>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub requirements: Vec<Requirement>,
}
#[derive(Clone)]
enum Constraint {
    Same(Var, Var),
    Allowed(Var, Domain),
    Relation(Var, Var, Vec<(Type, Type)>),
    Call(String, Vec<Var>, Var),
}
struct Solver<'a> {
    program: &'a Program,
    types: Vec<data::Definition>,
    all: Domain,
    domains: Vec<Domain>,
    rules: Vec<Constraint>,
    signatures: BTreeMap<Id, (Vec<Var>, Var)>,
    targets: Vec<(Target, Var)>,
    holes: Vec<Var>,
}
impl<'a> Solver<'a> {
    fn variable(&mut self, ty: Type) -> Result<Var> {
        if self.domains.len() >= LIMIT
            || self
                .domains
                .len()
                .saturating_add(1)
                .saturating_mul(self.all.len())
                > 1_048_576
        {
            return Err("type inference variable budget exceeded".into());
        }
        let id = self.domains.len();
        self.domains.push(if ty == Type::Infer {
            self.all.clone()
        } else {
            [ty].into()
        });
        Ok(id)
    }
    fn same(&mut self, a: Var, b: Var) {
        self.rules.push(Constraint::Same(a, b));
    }
    fn allowed(&mut self, a: Var, types: &[Type]) {
        self.rules
            .push(Constraint::Allowed(a, types.iter().copied().collect()));
    }
    fn pair(&mut self, a: Var, b: Var, rows: Vec<(Type, Type)>) {
        self.rules.push(Constraint::Relation(a, b, rows));
    }
    fn constant(&mut self, ty: Type) -> Result<Var> {
        self.variable(ty)
    }
    fn expression(
        &mut self,
        e: &Expr,
        owner: Id,
        env: &BTreeMap<String, Var>,
        path: &mut Vec<usize>,
    ) -> Result<Var> {
        let mut child = |this: &mut Self, e: &Expr, index| {
            path.push(index);
            let r = this.expression(e, owner, env, path);
            path.pop();
            r
        };
        Ok(match e {
            Expr::Hole(label) => {
                let v = self.variable(Type::Infer)?;
                self.targets.push((
                    Target::Hole {
                        definition: owner,
                        path: path.clone(),
                        label: label.clone(),
                    },
                    v,
                ));
                self.holes.push(v);
                v
            }
            Expr::Int(_) => self.constant(Type::I64)?,
            Expr::Float(_) => self.constant(Type::F32)?,
            Expr::Bool(_) => self.constant(Type::Bool)?,
            Expr::Unit => self.constant(Type::Unit)?,
            Expr::Name(n) => *env
                .get(n)
                .ok_or_else(|| format!("{n}: unknown local/input while inferring"))?,
            Expr::Unary(op, e) => {
                let a = child(self, e, 0)?;
                self.allowed(
                    a,
                    match op {
                        Unary::Not => &[Type::Bool],
                        Unary::BitNot => &[Type::I64],
                        Unary::FNeg => &[Type::F32],
                        _ => &[Type::I64, Type::F32],
                    },
                );
                a
            }
            Expr::Binary(op, a, b) => {
                let a = child(self, a, 0)?;
                let b = child(self, b, 1)?;
                self.same(a, b);
                let (types, boolean): (&[Type], bool) = match op {
                    Binary::FAdd | Binary::FSub | Binary::FMul | Binary::FDiv => {
                        (&[Type::F32], false)
                    }
                    Binary::FEq
                    | Binary::FNe
                    | Binary::FLt
                    | Binary::FLe
                    | Binary::FGt
                    | Binary::FGe => (&[Type::F32], true),
                    Binary::Add | Binary::Sub | Binary::Mul | Binary::Div => {
                        (&[Type::I64, Type::F32], false)
                    }
                    Binary::Eq | Binary::Ne => {
                        (&[Type::I64, Type::F32, Type::Bool, Type::Addr], true)
                    }
                    Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge => {
                        (&[Type::I64, Type::F32, Type::Addr], true)
                    }
                    Binary::And | Binary::Or => (&[Type::Bool], true),
                    _ => (&[Type::I64], false),
                };
                self.allowed(a, types);
                if boolean {
                    self.constant(Type::Bool)?
                } else {
                    a
                }
            }
            Expr::Choose(c, a, b) => {
                let c = child(self, c, 0)?;
                self.allowed(c, &[Type::Bool]);
                let a = child(self, a, 1)?;
                let b = child(self, b, 2)?;
                self.same(a, b);
                a
            }
            Expr::Construct(id, args) => {
                let result = self.constant(Type::Data(*id))?;
                let members = data::find(&self.types, *id)?.member_types();
                if args.len() == members.len() {
                    for (i, (e, ty)) in args.iter().zip(members).enumerate() {
                        let v = child(self, e, i)?;
                        self.allowed(v, &[ty]);
                    }
                }
                // UTF-8 constructor payloads are validated by the existing tensor checker.
                result
            }
            Expr::View(id, e) => {
                let a = child(self, e, 0)?;
                self.allowed(a, &[Type::Addr]);
                self.constant(Type::Data(*id))?
            }
            Expr::Field(e, name) => {
                let a = child(self, e, 0)?;
                let b = self.variable(Type::Infer)?;
                let rows = self
                    .types
                    .iter()
                    .filter_map(|d| {
                        if name == "len"
                            && matches!(
                                d.kind,
                                data::Kind::Array { .. } | data::Kind::Tensor { .. }
                            )
                        {
                            Some((Type::Data(d.id), Type::I64))
                        } else {
                            d.field(name).map(|(_, ty)| (Type::Data(d.id), ty))
                        }
                    })
                    .collect();
                self.pair(a, b, rows);
                b
            }
            Expr::Index(e, index) => {
                let a = child(self, e, 0)?;
                let index = child(self, index, 1)?;
                self.allowed(index, &[Type::I64]);
                let b = self.variable(Type::Infer)?;
                let rows = self
                    .types
                    .iter()
                    .filter_map(|d| match d.kind {
                        data::Kind::Array { element, .. } | data::Kind::Tensor { element, .. } => {
                            Some((Type::Data(d.id), element))
                        }
                        _ => None,
                    })
                    .collect();
                self.pair(a, b, rows);
                b
            }
            Expr::Call(name, args) => {
                if name == "diff.derivative" {
                    let [Expr::Name(symbol), argument] = args.as_slice() else {
                        return Err("diff.derivative expects a definition and a value".into());
                    };
                    let target = code::resolve(&self.program.code_modules, owner, symbol)
                        .ok_or("derivative definition missing")?;
                    let (inputs, result) = self.signatures[&target.id].clone();
                    if inputs.len() != 1 {
                        return Err("derivative needs one input".into());
                    }
                    self.allowed(inputs[0], &[Type::F32]);
                    self.allowed(result, &[Type::F32]);
                    let argument = child(self, argument, 1)?;
                    self.allowed(argument, &[Type::F32]);
                    self.constant(Type::F32)?
                } else if let Some(target) =
                    mira_core::code::resolve(&self.program.code_modules, owner, name)
                {
                    let (inputs, output) = self.signatures[&target.id].clone();
                    if inputs.len() != args.len() {
                        return Err(format!("{name}: wrong input count"));
                    }
                    for (i, (arg, wanted)) in args.iter().zip(inputs).enumerate() {
                        let v = child(self, arg, i)?;
                        self.same(v, wanted);
                    }
                    output
                } else if matches!(
                    name.as_str(),
                    "sys.scratch" | "sys.read_only" | "sys.neural_try"
                ) || crate::intrinsics::symbol_reference(name)
                {
                    let symbol = match args.first() {
                        Some(Expr::Name(n)) => n,
                        _ => return Err(format!("{name}: expected a definition symbol")),
                    };
                    let signature =
                        mira_core::code::resolve(&self.program.code_modules, owner, symbol)
                            .and_then(|d| self.signatures.get(&d.id))
                            .cloned();
                    for (i, arg) in args.iter().enumerate().skip(1) {
                        let v = child(self, arg, i)?;
                        if name == "sys.scratch" && i >= 2 {
                            if let Some((inputs, _)) = &signature {
                                let wanted = *inputs.get(i - 2).ok_or("scratch: wrong input count")?;
                                self.same(v, wanted);
                            }
                        } else {
                            self.allowed(v, &[Type::I64]);
                        }
                    }
                    if name == "sys.neural_try" {
                        if let Some((_, v)) = signature {
                            self.allowed(v, &[Type::Unit]);
                        }
                        self.constant(Type::Bool)?
                    } else if matches!(name.as_str(), "sys.scratch" | "sys.read_only") {
                        signature
                            .map(|s| s.1)
                            .unwrap_or(self.variable(Type::Infer)?)
                    } else {
                        self.constant(Type::Addr)?
                    }
                } else if let Some((inputs, result)) = crate::intrinsics::signature(name)
                    .or_else(|| crate::metaprogram::signature(name))
                    .or_else(|| {
                        crate::reflection::QUERIES
                            .iter()
                            .find(|(n, _)| *n == name)
                            .map(|(_, arity)| (vec![Type::I64; *arity], Type::I64))
                    })
                {
                    if inputs.len() == args.len() {
                        for (i, (arg, ty)) in args.iter().zip(inputs).enumerate() {
                            let v = child(self, arg, i)?;
                            self.allowed(v, &[ty]);
                        }
                    }
                    self.constant(result)?
                } else {
                    // Defer to the existing signatures after call/graph constraints
                    // resolve operands. Do not copy tensor shape rules here.
                    let mut variables = Vec::new();
                    let mut symbolic = false;
                    for (i, arg) in args.iter().enumerate() {
                        if matches!(arg,Expr::Name(n) if !env.contains_key(n)) {
                            symbolic = true;
                            continue;
                        }
                        variables.push(child(self, arg, i)?);
                    }
                    let result = self.variable(Type::Infer)?;
                    if !symbolic {
                        self.rules
                            .push(Constraint::Call(name.clone(), variables, result));
                    }
                    result
                }
            }
        })
    }
    fn block(
        &mut self,
        stmts: &[Statement],
        owner: Id,
        env: &mut BTreeMap<String, Var>,
        path: &mut Vec<usize>,
    ) -> Result<Var> {
        let mut result = self.constant(Type::Unit)?;
        for (i, stmt) in stmts.iter().enumerate() {
            path.push(i);
            result = match stmt {
                Statement::Expr(e) => {
                    path.push(0);
                    let v = self.expression(e, owner, env, path)?;
                    path.pop();
                    v
                }
                Statement::Bind {
                    name,
                    ty,
                    value,
                    op,
                    ..
                } => {
                    path.push(0);
                    let value = self.expression(value, owner, env, path)?;
                    path.pop();
                    let v = if let Some(v) = env.get(name) {
                        *v
                    } else {
                        let v = self.variable(ty.unwrap_or(Type::Infer))?;
                        env.insert(name.clone(), v);
                        v
                    };
                    self.same(v, value);
                    if let Some(ty) = ty {
                        if *ty != Type::Infer {
                            self.allowed(v, &[*ty]);
                        }
                    }
                    if *op != Assign::Set {
                        self.allowed(v, &[Type::I64, Type::F32]);
                    }
                    self.constant(Type::Unit)?
                }
                Statement::Set { target, value, .. } => {
                    path.push(0);
                    let a = self.expression(target, owner, env, path)?;
                    path.pop();
                    path.push(1);
                    let b = self.expression(value, owner, env, path)?;
                    path.pop();
                    self.same(a, b);
                    self.constant(Type::Unit)?
                }
                Statement::If { condition, yes, no } => {
                    path.push(0);
                    let c = self.expression(condition, owner, env, path)?;
                    path.pop();
                    self.allowed(c, &[Type::Bool]);
                    path.push(1);
                    let a = self.block(yes, owner, &mut env.clone(), path)?;
                    path.pop();
                    if no.is_empty() {
                        self.constant(Type::Unit)?
                    } else {
                        path.push(2);
                        let b = self.block(no, owner, &mut env.clone(), path)?;
                        path.pop();
                        self.same(a, b);
                        a
                    }
                }
                Statement::While { condition, body } => {
                    path.push(0);
                    let c = self.expression(condition, owner, env, path)?;
                    path.pop();
                    self.allowed(c, &[Type::Bool]);
                    path.push(1);
                    self.block(body, owner, &mut env.clone(), path)?;
                    path.pop();
                    self.constant(Type::Unit)?
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    for (j, e) in [start, end].iter().enumerate() {
                        path.push(j);
                        let v = self.expression(e, owner, env, path)?;
                        path.pop();
                        self.allowed(v, &[Type::I64]);
                    }
                    let mut inner = env.clone();
                    let v = self.constant(Type::I64)?;
                    inner.insert(name.clone(), v);
                    path.push(2);
                    self.block(body, owner, &mut inner, path)?;
                    path.pop();
                    self.constant(Type::Unit)?
                }
            };
            path.pop();
        }
        Ok(result)
    }
    fn solve(&mut self) -> Result<()> {
        let mut work = 0usize;
        for _ in 0..self.domains.len() + 1 {
            let mut changed = false;
            for rule in &self.rules {
                let cost = match rule {
                    Constraint::Same(a, b) => self.domains[*a].len() + self.domains[*b].len(),
                    Constraint::Allowed(a, b) => self.domains[*a].len() + b.len(),
                    Constraint::Relation(_, _, rows) => rows.len(),
                    Constraint::Call(_, inputs, _) => inputs.len() + 1,
                };
                work = work.saturating_add(cost + 1);
                if work > 4_000_000 {
                    return Err("type inference work budget exceeded".into());
                }
                let updates = match rule {
                    Constraint::Call(name, inputs, output) => {
                        if inputs.iter().all(|v| self.domains[*v].len() == 1) {
                            let actual: Vec<_> = inputs
                                .iter()
                                .map(|v| *self.domains[*v].first().unwrap())
                                .collect();
                            if let Some(result) =
                                crate::tensor::signature(name, &actual, &self.types)
                                    .or_else(|| crate::intrinsics::native_signature(name, &actual))
                            {
                                let ty = result?;
                                vec![(
                                    *output,
                                    self.domains[*output]
                                        .intersection(&[ty].into())
                                        .copied()
                                        .collect(),
                                )]
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    }
                    Constraint::Same(a, b) => {
                        let common = self.domains[*a]
                            .intersection(&self.domains[*b])
                            .copied()
                            .collect::<Domain>();
                        vec![(*a, common.clone()), (*b, common)]
                    }
                    Constraint::Allowed(a, allowed) => vec![(
                        *a,
                        self.domains[*a].intersection(allowed).copied().collect(),
                    )],
                    Constraint::Relation(a, b, rows) => {
                        let rows: Vec<_> = rows
                            .iter()
                            .filter(|(x, y)| {
                                self.domains[*a].contains(x) && self.domains[*b].contains(y)
                            })
                            .collect();
                        vec![
                            (*a, rows.iter().map(|(x, _)| *x).collect()),
                            (*b, rows.iter().map(|(_, y)| *y).collect()),
                        ]
                    }
                };
                for (i, domain) in updates {
                    if domain.is_empty() {
                        let location = self
                            .signatures
                            .iter()
                            .find_map(|(id, (inputs, output))| {
                                let d = code::find(&self.program.code_modules, *id)?;
                                if *output == i {
                                    Some(format!("{} result", d.name))
                                } else {
                                    inputs.iter().position(|v| *v == i).map(|j| {
                                        format!("{} input {}", d.name, d.parameters[j].name)
                                    })
                                }
                            })
                            .unwrap_or_else(|| "expression".into());
                        return Err(format!("{location}: incompatible type constraints; a learned proposal cannot override them"));
                    }
                    if self.domains[i] != domain {
                        self.domains[i] = domain;
                        changed = true;
                    }
                }
            }
            if !changed {
                return Ok(());
            }
        }
        Err("type inference iteration budget exceeded".into())
    }
}
pub fn analyze(program: &mut Program, choices: &[Choice], strict: bool) -> Result<Report> {
    let types = program.data_types();
    if types.len() > 512 {
        return Err("type inference aggregate budget exceeded".into());
    }
    let all = [
        Type::I64,
        Type::F32,
        Type::Bool,
        Type::Event,
        Type::Unit,
        Type::Addr,
    ]
    .into_iter()
    .chain(types.iter().map(|d| Type::Data(d.id)))
    .collect();
    let mut s = Solver {
        program,
        types,
        all,
        domains: Vec::new(),
        rules: Vec::new(),
        signatures: BTreeMap::new(),
        targets: Vec::new(),
        holes: Vec::new(),
    };
    let modules = mira_core::code::symbols(&program.code_modules);
    for (_, d) in &modules {
        let mut inputs = Vec::new();
        for (index, p) in d.parameters.iter().enumerate() {
            let v = s.variable(p.ty)?;
            s.domains[v].remove(&Type::Unit);
            if p.ty == Type::Infer {
                s.targets.push((
                    Target::Parameter {
                        definition: d.id,
                        index,
                    },
                    v,
                ));
            }
            inputs.push(v);
        }
        let output = s.variable(if d.annotated { d.output } else { Type::Infer })?;
        if !d.annotated || d.output == Type::Infer {
            s.targets
                .push((Target::Result { definition: d.id }, output));
        }
        s.signatures.insert(d.id, (inputs, output));
    }
    for (_, d) in &modules {
        let env: BTreeMap<String, Var> = d
            .parameters
            .iter()
            .zip(&s.signatures[&d.id].0)
            .map(|(p, v)| (p.name.clone(), *v))
            .collect();
        let body = s.block(&d.body, d.id, &mut env.clone(), &mut Vec::new())?;
        s.same(body, s.signatures[&d.id].1);
    }
    fn port(s: &mut Solver<'_>, space: &Space, id: Id, name: &str, input: bool) -> Result<Var> {
        if id == space.id {
            return s.variable(
                if input { &space.outputs } else { &space.inputs }
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or("inference boundary missing")?
                    .ty,
            );
        }
        let cell = space.cell(id)?;
        if let Definition::Code(def) | Definition::Adaptive(def) = cell.definition {
            let d =
                code::find(&s.program.code_modules, def).ok_or("inference definition missing")?;
            if input {
                let index = d
                    .parameters
                    .iter()
                    .position(|p| p.name == name)
                    .ok_or("inference input missing")?;
                return Ok(s.signatures[&def].0[index]);
            }
            if name == "out" && !d.output_ports {
                return Ok(s.signatures[&def].1);
            }
        }
        s.variable(
            s.program
                .ports(cell)
                .iter()
                .find(|p| p.name == name && (p.direction == Direction::In) == input)
                .ok_or("inference port missing")?
                .ty,
        )
    }
    fn graph(s: &mut Solver<'_>, space: &Space) -> Result<()> {
        for link in &space.links {
            let a = port(s, space, link.from.cell, &link.from.port, false)?;
            let b = port(s, space, link.to.cell, &link.to.port, true)?;
            s.same(a, b);
        }
        for cell in &space.cells {
            if let Some(inner) = &cell.space {
                graph(s, inner)?;
            }
        }
        Ok(())
    }
    graph(&mut s, &program.root)?;
    for choice in choices {
        if choice.ty == Type::Infer {
            return Err("a type proposal must select a concrete type".into());
        }
        let v = s
            .targets
            .iter()
            .find(|(target, _)| target == &choice.target)
            .ok_or("unknown type proposal target")?
            .1;
        s.allowed(v, &[choice.ty]);
    }
    s.solve()?;
    let requirements = s
        .targets
        .iter()
        .map(|(target, v)| Requirement {
            target: target.clone(),
            candidates: s.domains[*v].iter().copied().collect(),
        })
        .collect::<Vec<_>>();
    if strict {
        if !s.holes.is_empty() {
            return Err("unfilled code holes: inspect the draft before compilation".into());
        }
        for r in &requirements {
            if matches!(r.target, Target::Parameter { .. }) && r.candidates.len() != 1 {
                return Err(format!("ambiguous inferred type for {:?}: {:?}; provide an example/constraint or a checked proposal",r.target,r.candidates));
            }
        }
    }
    let resolved = s
        .signatures
        .iter()
        .map(|(id, (inputs, output))| {
            (
                *id,
                (
                    inputs
                        .iter()
                        .map(|v| {
                            if s.domains[*v].len() == 1 {
                                *s.domains[*v].first().unwrap()
                            } else {
                                Type::Infer
                            }
                        })
                        .collect::<Vec<_>>(),
                    if s.domains[*output].len() == 1 {
                        *s.domains[*output].first().unwrap()
                    } else {
                        Type::Infer
                    },
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    fn apply(defs: &mut [CodeModule], resolved: &BTreeMap<Id, (Vec<Type>, Type)>) {
        fn bindings(stmts: &mut [Statement]) {
            for stmt in stmts {
                match stmt {
                    Statement::Bind { ty, .. } if *ty == Some(Type::Infer) => *ty = None,
                    Statement::If { yes, no, .. } => {
                        bindings(yes);
                        bindings(no);
                    }
                    Statement::For { body, .. } | Statement::While { body, .. } => bindings(body),
                    _ => {}
                }
            }
        }
        for d in defs {
            bindings(&mut d.body);
            let (inputs, output) = &resolved[&d.id];
            for (p, ty) in d.parameters.iter_mut().zip(inputs) {
                if p.ty == Type::Infer {
                    p.ty = *ty;
                }
            }
            if !d.annotated || d.output == Type::Infer {
                d.output = *output;
            }
            apply(&mut d.locals, resolved);
        }
    }
    drop(s);
    apply(&mut program.code_modules, &resolved);
    Ok(Report { requirements })
}
pub(crate) fn needed(program: &Program) -> bool {
    fn expr(e: &Expr) -> bool {
        match e {
            Expr::Hole(_) => true,
            Expr::Unary(_, e) | Expr::View(_, e) | Expr::Field(e, _) => expr(e),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => expr(a) || expr(b),
            Expr::Choose(a, b, c) => expr(a) || expr(b) || expr(c),
            Expr::Call(_, args) | Expr::Construct(_, args) => args.iter().any(expr),
            _ => false,
        }
    }
    fn block(stmts: &[Statement]) -> bool {
        stmts.iter().any(|s| match s {
            Statement::Bind {
                ty: Some(Type::Infer),
                ..
            } => true,
            Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e),
            Statement::Set { target, value, .. } => expr(target) || expr(value),
            Statement::If { condition, yes, no } => expr(condition) || block(yes) || block(no),
            Statement::While { condition, body } => expr(condition) || block(body),
            Statement::For {
                start, end, body, ..
            } => expr(start) || expr(end) || block(body),
        })
    }
    code::symbols(&program.code_modules).iter().any(|(_, d)| {
        d.parameters.iter().any(|p| p.ty == Type::Infer)
            || d.output == Type::Infer
            || block(&d.body)
    })
}

/// Resolve a proposal on an isolated draft, then use the existing compiler's
/// full type/effect/composition checks. The original is unchanged on failure.
pub fn checked(program: &Program, choices: &[Choice]) -> Result<Program> {
    let mut draft = program.clone();
    analyze(&mut draft, choices, true)?;
    crate::composition::infer(&mut draft)?;
    draft.require_complete()?;
    draft.validate()?;
    crate::access::validate_source(&draft)?;
    Ok(draft)
}

/// Fill one typed hole with existing Mira code, then validate the whole candidate.
/// This is intentionally a complete-program gate, not speculative execution.
pub fn fill(
    program: &Program,
    target: &Target,
    value: Expr,
    choices: &[Choice],
) -> Result<Program> {
    let Target::Hole {
        definition,
        path,
        label,
    } = target
    else {
        return Err("fill needs a hole target".into());
    };
    let mut draft = program.clone();
    let d =
        code::find_mut(&mut draft.code_modules, *definition).ok_or("hole definition missing")?;
    let expression = code::expression_mut(d, path)?;
    if *expression != Expr::Hole(label.clone()) {
        return Err("stale hole location".into());
    }
    *expression = value;
    checked(&draft, choices)
}
