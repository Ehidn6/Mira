//! Lower already checked aggregate operations to the existing word ABI. Runtime
//! operations remain typed Core procedures, compiled by the general backend.
//! This pass neither parses another language nor exposes erased references in source.
use mira_core::{code::*, *};
use std::collections::BTreeMap;
fn word(ty: Type) -> Result<Type> {
    match ty {
        Type::I64 | Type::Bool | Type::Unit => Ok(ty),
        Type::Addr | Type::Data(_) => Ok(Type::I64),
        _ => Err("stage-1 lowering does not admit this numeric type".into()),
    }
}
pub(crate) struct Lowered {
    pub bodies: BTreeMap<Id, CodeModule>,
    pub signatures: Vec<CodeModule>,
    pub runtime: Vec<CodeModule>,
}
struct Builder<'a> {
    definitions: &'a [CodeModule],
    types: &'a [data::Definition],
    cursor: Id,
    runtime: Vec<CodeModule>,
    keys: BTreeMap<String, (String, Id)>,
    temporaries: usize,
}
impl Builder<'_> {
    fn helper(
        &mut self,
        key: String,
        inputs: Vec<Type>,
        output: Type,
        body: Vec<Statement>,
    ) -> Result<String> {
        if let Some((name, _)) = self.keys.get(&key) {
            return Ok(name.clone());
        }
        let id = self.cursor;
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or("stage-1 runtime identity exhausted")?;
        let name = if key == "load-place" || key.starts_with("store-place:") {
            format!("#stage1.runtime.memory.{id}")
        } else {
            format!("#stage1.runtime.{id}")
        };
        self.runtime.push(CodeModule {
            owner: None,
            id,
            name: name.clone(),
            data_types: vec![],
            parameters: inputs
                .into_iter()
                .enumerate()
                .map(|(i, ty)| Parameter {
                    name: format!("a{i}"),
                    ty,
                })
                .collect(),
            output,
            output_ports: false,
            compile_time: false,
            annotated: true,
            body,
            locals: vec![],
        });
        self.keys.insert(key, (name.clone(), id));
        Ok(name)
    }
    fn ty(&self, e: &Expr, env: &BTreeMap<String, Type>) -> Result<Type> {
        crate::behavior::expression_type(e, env, self.definitions, self.types)
    }
    fn expression(&mut self, e: &Expr, env: &BTreeMap<String, Type>) -> Result<Expr> {
        word(self.ty(e, env)?)?;
        Ok(match e {
            Expr::Construct(id, args) => {
                if args.len() > 8 {
                    return Err("constructor exceeds the admitted word call profile".into());
                }
                let inputs = args
                    .iter()
                    .map(|e| self.ty(e, env))
                    .collect::<Result<Vec<_>>>()?;
                for ty in &inputs {
                    word(*ty)?;
                }
                let body = Expr::Construct(
                    *id,
                    (0..args.len())
                        .map(|i| Expr::Name(format!("a{i}")))
                        .collect(),
                );
                let helper = self.helper(
                    format!("construct:{id}:{inputs:?}"),
                    inputs,
                    Type::Data(*id),
                    vec![Statement::Expr(body)],
                )?;
                Expr::Call(
                    helper,
                    args.iter()
                        .map(|e| self.expression(e, env))
                        .collect::<Result<_>>()?,
                )
            }
            Expr::Field(base, name) => {
                let ty = self.ty(base, env)?;
                let output = self.ty(e, env)?;
                let helper = self.helper(
                    format!("field:{ty:?}:{name}"),
                    vec![ty],
                    output,
                    vec![Statement::Expr(Expr::Field(
                        Box::new(Expr::Name("a0".into())),
                        name.clone(),
                    ))],
                )?;
                Expr::Call(helper, vec![self.expression(base, env)?])
            }
            Expr::Index(base, index) => {
                let ty = self.ty(base, env)?;
                let output = self.ty(e, env)?;
                let helper = self.helper(
                    format!("index:{ty:?}"),
                    vec![ty, Type::I64],
                    output,
                    vec![Statement::Expr(Expr::Index(
                        Box::new(Expr::Name("a0".into())),
                        Box::new(Expr::Name("a1".into())),
                    ))],
                )?;
                Expr::Call(
                    helper,
                    vec![self.expression(base, env)?, self.expression(index, env)?],
                )
            }
            Expr::Call(name, args) => {
                if name == "sys.address_of" {
                    return self.place(&args[0], env);
                }
                if crate::intrinsics::symbol_reference(name)
                    || matches!(
                        name.as_str(),
                        "sys.scratch" | "sys.read_only" | "sys.neural_try"
                    )
                {
                    return Err("scope/symbol calls stay in the general backend".into());
                }
                let target = if self.definitions.iter().any(|d| d.name == *name) {
                    name.clone()
                } else {
                    let inputs = args
                        .iter()
                        .map(|e| self.ty(e, env))
                        .collect::<Result<Vec<_>>>()?;
                    for ty in &inputs {
                        word(*ty)?;
                    }
                    let result = self.ty(e, env)?;
                    self.helper(
                        format!("intrinsic:{name}:{inputs:?}"),
                        inputs,
                        result,
                        vec![Statement::Expr(Expr::Call(
                            name.clone(),
                            (0..args.len())
                                .map(|i| Expr::Name(format!("a{i}")))
                                .collect(),
                        ))],
                    )?
                };
                Expr::Call(
                    target,
                    args.iter()
                        .map(|e| self.expression(e, env))
                        .collect::<Result<_>>()?,
                )
            }
            Expr::Unary(op, e) => Expr::Unary(*op, Box::new(self.expression(e, env)?)),
            Expr::Binary(op, a, b) => {
                if self.ty(a, env)? == Type::Addr {
                    let output = self.ty(e, env)?;
                    let helper = self.helper(
                        format!("address-binary:{op:?}"),
                        vec![Type::Addr, Type::Addr],
                        output,
                        vec![Statement::Expr(Expr::Binary(
                            *op,
                            Box::new(Expr::Name("a0".into())),
                            Box::new(Expr::Name("a1".into())),
                        ))],
                    )?;
                    Expr::Call(
                        helper,
                        vec![self.expression(a, env)?, self.expression(b, env)?],
                    )
                } else {
                    Expr::Binary(
                        *op,
                        Box::new(self.expression(a, env)?),
                        Box::new(self.expression(b, env)?),
                    )
                }
            }
            Expr::Choose(c, a, b) => Expr::Choose(
                Box::new(self.expression(c, env)?),
                Box::new(self.expression(a, env)?),
                Box::new(self.expression(b, env)?),
            ),
            Expr::Int(_) | Expr::Bool(_) | Expr::Unit | Expr::Name(_) => e.clone(),
            _ => return Err("operation remains in general backend".into()),
        })
    }
    fn place(&mut self, e: &Expr, env: &BTreeMap<String, Type>) -> Result<Expr> {
        let (inputs, args, place) = match e {
            Expr::Field(base, field) => {
                let ty = self.ty(base, env)?;
                (
                    vec![ty],
                    vec![self.expression(base, env)?],
                    Expr::Field(Box::new(Expr::Name("a0".into())), field.clone()),
                )
            }
            Expr::Index(base, index) => {
                let ty = self.ty(base, env)?;
                (
                    vec![ty, Type::I64],
                    vec![self.expression(base, env)?, self.expression(index, env)?],
                    Expr::Index(
                        Box::new(Expr::Name("a0".into())),
                        Box::new(Expr::Name("a1".into())),
                    ),
                )
            }
            _ => return Err("unaddressable stage-1 place".into()),
        };
        let helper = self.helper(
            format!("place:{place:?}:{inputs:?}"),
            inputs,
            Type::Addr,
            vec![Statement::Expr(Expr::Call(
                "sys.address_of".into(),
                vec![place],
            ))],
        )?;
        Ok(Expr::Call(helper, args))
    }
    fn block(
        &mut self,
        body: &[Statement],
        env: &mut BTreeMap<String, Type>,
    ) -> Result<Vec<Statement>> {
        let mut out = Vec::new();
        for statement in body {
            match statement {
                Statement::Expr(e) => out.push(Statement::Expr(self.expression(e, env)?)),
                Statement::Bind {
                    name,
                    ty,
                    immutable,
                    op,
                    value,
                } => {
                    let value_type = self.ty(value, env)?;
                    let lowered = self.expression(value, env)?;
                    if !env.contains_key(name) {
                        env.insert(name.clone(), ty.unwrap_or(value_type));
                    }
                    out.push(Statement::Bind {
                        name: name.clone(),
                        ty: ty.map(word).transpose()?,
                        immutable: *immutable,
                        op: *op,
                        value: lowered,
                    });
                }
                Statement::Set { target, op, value } => {
                    let target_type = self.ty(target, env)?;
                    if !matches!(target_type, Type::I64 | Type::Bool) {
                        return Err("aggregate/reference stores stay in the general backend".into());
                    }
                    let location = self.place(target, env)?;
                    self.temporaries += 1;
                    let temp = "#$stage1_place".to_string();
                    out.push(Statement::Bind {
                        name: temp.clone(),
                        ty: None,
                        immutable: false,
                        op: Assign::Set,
                        value: location,
                    });
                    let mut rhs = self.expression(value, env)?;
                    if *op != Assign::Set {
                        let old = "#$stage1_old".to_string();
                        let read = self.helper(
                            "load-place".into(),
                            vec![Type::Addr],
                            Type::I64,
                            vec![Statement::Expr(Expr::Call(
                                "sys.load64".into(),
                                vec![Expr::Name("a0".into())],
                            ))],
                        )?;
                        out.push(Statement::Bind {
                            name: old.clone(),
                            ty: None,
                            immutable: false,
                            op: Assign::Set,
                            value: Expr::Call(read, vec![Expr::Name(temp.clone())]),
                        });
                        let binary = match op {
                            Assign::Add => Binary::Add,
                            Assign::Sub => Binary::Sub,
                            Assign::Mul => Binary::Mul,
                            Assign::Div => Binary::Div,
                            Assign::Rem => Binary::Rem,
                            _ => return Err("noninteger word compound assignment".into()),
                        };
                        rhs = Expr::Binary(binary, Box::new(Expr::Name(old)), Box::new(rhs));
                    }
                    let stored = if target_type == Type::Bool {
                        Expr::Choose(
                            Box::new(Expr::Name("a1".into())),
                            Box::new(Expr::Int(1)),
                            Box::new(Expr::Int(0)),
                        )
                    } else {
                        Expr::Name("a1".into())
                    };
                    let helper = self.helper(
                        format!("store-place:{target_type:?}"),
                        vec![Type::Addr, target_type],
                        Type::Unit,
                        vec![Statement::Expr(Expr::Call(
                            "sys.store64".into(),
                            vec![Expr::Name("a0".into()), stored],
                        ))],
                    )?;
                    out.push(Statement::Expr(Expr::Call(
                        helper,
                        vec![Expr::Name(temp), rhs],
                    )));
                }
                Statement::If { condition, yes, no } => out.push(Statement::If {
                    condition: self.expression(condition, env)?,
                    yes: self.block(yes, &mut env.clone())?,
                    no: self.block(no, &mut env.clone())?,
                }),
                Statement::While { condition, body } => out.push(Statement::While {
                    condition: self.expression(condition, env)?,
                    body: self.block(body, &mut env.clone())?,
                }),
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let start = self.expression(start, env)?;
                    let end = self.expression(end, env)?;
                    let mut inner = env.clone();
                    inner.insert(name.clone(), Type::I64);
                    out.push(Statement::For {
                        name: name.clone(),
                        start,
                        end,
                        body: self.block(body, &mut inner)?,
                    });
                }
            }
        }
        Ok(out)
    }
}
pub(crate) fn lower(definitions: &[CodeModule], types: &[data::Definition]) -> Result<Lowered> {
    let maximum = definitions
        .iter()
        .map(|d| d.id)
        .chain(types.iter().map(|t| t.id))
        .max()
        .unwrap_or(0);
    let mut b = Builder {
        definitions,
        types,
        cursor: maximum.checked_add(1).ok_or("stage-1 identity exhausted")?,
        runtime: vec![],
        keys: BTreeMap::new(),
        temporaries: 0,
    };
    let mut bodies = BTreeMap::new();
    for d in definitions {
        if d.compile_time
            || d.output_ports
            || !d.locals.is_empty()
            || d.parameters.iter().any(|p| word(p.ty).is_err())
            || word(d.output).is_err()
        {
            continue;
        }
        let mut env = d
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.ty))
            .collect();
        // Only unsupported profile features defer to the general active compiler;
        // once a word body is admitted its actual compilation errors are propagated.
        let previous = b.temporaries;
        if let Ok(mut body) = b.block(&d.body, &mut env) {
            if b.temporaries != previous {
                let mut prefix = Vec::new();
                for name in ["#$stage1_place", "#$stage1_old"] {
                    prefix.push(Statement::Bind {
                        name: name.into(),
                        ty: Some(Type::I64),
                        immutable: false,
                        op: Assign::Set,
                        value: Expr::Int(0),
                    });
                }
                prefix.append(&mut body);
                body = prefix;
            }
            let mut lowered = d.clone();
            lowered.body = body;
            lowered.data_types.clear();
            for p in &mut lowered.parameters {
                p.ty = word(p.ty)?;
            }
            lowered.output = word(d.output)?;
            bodies.insert(d.id, lowered);
        }
    }
    let signatures = definitions
        .iter()
        .chain(&b.runtime)
        .filter_map(|d| {
            let mut d = d.clone();
            for p in &mut d.parameters {
                p.ty = word(p.ty).ok()?;
            }
            d.output = word(d.output).ok()?;
            Some(d)
        })
        .collect();
    Ok(Lowered {
        bodies,
        signatures,
        runtime: b.runtime,
    })
}
