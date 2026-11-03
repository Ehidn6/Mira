//! Typed numeric legalization. Source operators retain one syntax across backends.
use mira_core::{code::*, *};
use std::collections::BTreeMap;
pub(crate) fn lower(defs: &mut [CodeModule], types: &[data::Definition]) -> Result<()> {
    let signatures = defs.to_vec();
    fn expr(
        e: &mut Expr,
        env: &BTreeMap<String, Type>,
        defs: &[CodeModule],
        types: &[data::Definition],
    ) -> Result<()> {
        match e {
            Expr::Unary(op, a) => {
                let ty = crate::behavior::expression_type(a, env, defs, types)?;
                expr(a, env, defs, types)?;
                if ty == Type::F32 && *op == Unary::Neg {
                    *op = Unary::FNeg;
                }
            }
            Expr::Binary(op, a, b) => {
                let ty = crate::behavior::expression_type(a, env, defs, types)?;
                expr(a, env, defs, types)?;
                expr(b, env, defs, types)?;
                if ty == Type::Addr {
                    let name = match op {
                        Binary::Lt => Some("#addr.lt"),
                        Binary::Le => Some("#addr.le"),
                        Binary::Gt => Some("#addr.gt"),
                        Binary::Ge => Some("#addr.ge"),
                        _ => None,
                    };
                    if let Some(name) = name {
                        *e = Expr::Call(name.into(), vec![(**a).clone(), (**b).clone()]);
                    }
                    return Ok(());
                }
                if ty == Type::F32 {
                    *op = match *op {
                        Binary::Add => Binary::FAdd,
                        Binary::Sub => Binary::FSub,
                        Binary::Mul => Binary::FMul,
                        Binary::Div => Binary::FDiv,
                        Binary::Eq => Binary::FEq,
                        Binary::Ne => Binary::FNe,
                        Binary::Lt => Binary::FLt,
                        Binary::Le => Binary::FLe,
                        Binary::Gt => Binary::FGt,
                        Binary::Ge => Binary::FGe,
                        other => other,
                    };
                }
            }
            Expr::Call(_, args) | Expr::Construct(_, args) => {
                for a in args {
                    expr(a, env, defs, types)?;
                }
            }
            Expr::Field(a, _) | Expr::View(_, a) => expr(a, env, defs, types)?,
            Expr::Index(a, b) => {
                expr(a, env, defs, types)?;
                expr(b, env, defs, types)?;
            }
            Expr::Choose(c, a, b) => {
                expr(c, env, defs, types)?;
                expr(a, env, defs, types)?;
                expr(b, env, defs, types)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn assign(op: &mut Assign, ty: Type) {
        if ty == Type::F32 {
            *op = match *op {
                Assign::Add => Assign::FAdd,
                Assign::Sub => Assign::FSub,
                Assign::Mul => Assign::FMul,
                Assign::Div => Assign::FDiv,
                other => other,
            };
        }
    }
    fn body(
        statements: &mut [Statement],
        env: &mut BTreeMap<String, Type>,
        defs: &[CodeModule],
        types: &[data::Definition],
    ) -> Result<()> {
        for statement in statements {
            match statement {
                Statement::Expr(e) => expr(e, env, defs, types)?,
                Statement::Bind {
                    name,
                    ty,
                    value,
                    op,
                    ..
                } => {
                    let inferred = crate::behavior::expression_type(value, env, defs, types)?;
                    expr(value, env, defs, types)?;
                    let actual = env.get(name).copied().or(*ty).unwrap_or(inferred);
                    assign(op, actual);
                    env.insert(name.clone(), actual);
                }
                Statement::Set { target, value, op } => {
                    let ty = crate::behavior::expression_type(target, env, defs, types)?;
                    expr(target, env, defs, types)?;
                    expr(value, env, defs, types)?;
                    assign(op, ty);
                }
                Statement::If { condition, yes, no } => {
                    expr(condition, env, defs, types)?;
                    body(yes, &mut env.clone(), defs, types)?;
                    body(no, &mut env.clone(), defs, types)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expr(condition, env, defs, types)?;
                    body(inner, &mut env.clone(), defs, types)?;
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body: inner,
                } => {
                    expr(start, env, defs, types)?;
                    expr(end, env, defs, types)?;
                    let mut innerenv = env.clone();
                    innerenv.insert(name.clone(), Type::I64);
                    body(inner, &mut innerenv, defs, types)?;
                }
            }
        }
        Ok(())
    }
    for d in defs {
        let key = crate::compilation_cache::numeric_key(d, &signatures, types)?;
        if let Some(lowered) = crate::compilation_cache::numeric_get(&key, &signatures) {
            *d = lowered;
            continue;
        }
        let mut env = d
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.ty))
            .collect();
        body(&mut d.body, &mut env, &signatures, types)?;
        crate::compilation_cache::numeric_insert(key, d, &signatures)?;
    }
    Ok(())
}
