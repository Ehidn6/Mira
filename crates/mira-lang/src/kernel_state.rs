//! Native lowering of programmable Adaptive cells into the same Mira behavior IR.
//! Persistent storage is part of the boot payload, never a hosted runtime.
use mira_core::{code::*, system::Parameters, *};
use std::collections::BTreeMap;

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call(name.into(), args)
}
fn name(n: &str) -> Expr {
    Expr::Name(n.into())
}
fn bind(n: &str, value: Expr) -> Statement {
    Statement::Bind {
        name: n.into(),
        ty: None,
        immutable: false,
        op: Assign::Set,
        value,
    }
}
fn binary(op: Binary, a: Expr, b: Expr) -> Expr {
    Expr::Binary(op, Box::new(a), Box::new(b))
}
fn address(index: usize) -> Expr {
    call(&format!("#kernel.slot.{index}"), vec![])
}
fn read(index: usize, ty: Type) -> Expr {
    let value = call("sys.load64", vec![address(index)]);
    if ty == Type::Bool {
        binary(Binary::Ne, value, Expr::Int(0))
    } else {
        value
    }
}
fn word(value: Expr, ty: Type) -> Expr {
    if ty == Type::Bool {
        Expr::Choose(
            Box::new(value),
            Box::new(Expr::Int(1)),
            Box::new(Expr::Int(0)),
        )
    } else {
        value
    }
}
fn store(index: usize, value: Expr, ty: Type) -> Statement {
    Statement::Expr(call("sys.store64", vec![address(index), word(value, ty)]))
}
fn require(condition: Expr) -> Statement {
    Statement::Expr(call("#kernel.require", vec![condition]))
}
fn restore_flags() -> Statement {
    Statement::If {
        condition: binary(
            Binary::Ne,
            binary(Binary::BitAnd, name("#flags"), Expr::Int(512)),
            Expr::Int(0),
        ),
        yes: vec![Statement::Expr(call("#kernel.sti", vec![]))],
        no: vec![],
    }
}
fn slot(values: &mut Vec<i64>, value: i64) -> usize {
    let i = values.len();
    values.push(value);
    i
}

pub(crate) fn lower(
    program: &Program,
    settings: &BTreeMap<Id, Parameters>,
) -> Result<(Program, Vec<i64>)> {
    crate::access::validate_source(program)?;
    program.validate()?;
    let mut checked = program.clone();
    crate::composition::infer(&mut checked)?;
    if checked.code_modules != program.code_modules {
        return Err("kernel code signatures are stale".into());
    }
    let mut result = program.clone();
    // Slot zero is the explicit native activation epoch; zero denotes an empty cache.
    let mut values = vec![1];
    let cells = inspect::cells(program)
        .iter()
        .filter_map(|c| {
            if let Definition::Adaptive(module) = c.cell.definition {
                Some((c.cell.id, module))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for (cell, module) in cells {
        let behavior = mira_core::code::find(&program.code_modules, module)
            .ok_or("adaptive behavior missing")?;
        if !matches!(behavior.output, Type::I64 | Type::Bool) {
            return Err("native adaptive output currently requires i64 or bool".into());
        }
        let config = settings.get(&cell).cloned().unwrap_or_default();
        let mut named = BTreeMap::new();
        let mut body = vec![
            require(call("#kernel.region_required", vec![])),
            bind("#flags", call("#kernel.flags", vec![])),
            Statement::Expr(call("#kernel.cli", vec![])),
            bind("#epoch", read(0, Type::I64)),
        ];
        let mut parameters = vec![];
        let mut hidden = BTreeMap::new();
        for p in &behavior.parameters {
            if !matches!(p.ty, Type::I64 | Type::Bool) {
                return Err("native adaptive inputs/slots currently require i64 or bool".into());
            }
            named.insert(p.name.clone(), (name(&p.name), p.ty));
            if p.name.starts_with("state_") || p.name.starts_with("param_") {
                let initial = config.slots.get(&p.name).copied().unwrap_or(0);
                if p.ty == Type::Bool && !matches!(initial, 0 | 1) {
                    return Err(format!("{}: invalid initial boolean slot", p.name));
                }
                let index = slot(&mut values, initial);
                hidden.insert(p.name.clone(), (index, p.ty));
            } else {
                parameters.push(p.clone());
            }
        }
        let cache_epoch = slot(&mut values, 0);
        let busy = slot(&mut values, 0);
        let cached_output = slot(&mut values, 0);
        let input_cache = parameters
            .iter()
            .map(|p| (p.clone(), slot(&mut values, 0)))
            .collect::<Vec<_>>();
        let mut cached = vec![];
        for (p, index) in &input_cache {
            cached.push(require(binary(
                Binary::Eq,
                name(&p.name),
                read(*index, p.ty),
            )));
        }
        cached.push(bind("#result", read(cached_output, behavior.output)));
        cached.push(restore_flags());
        cached.push(Statement::Expr(name("#result")));

        let mut fresh = vec![
            require(binary(Binary::Eq, read(busy, Type::I64), Expr::Int(0))),
            store(busy, Expr::Int(1), Type::I64),
        ];
        if behavior.output == Type::Bool {
            if let Some((value, ty)) = named.get("target") {
                if *ty == Type::I64 {
                    fresh.push(require(binary(
                        Binary::Or,
                        binary(Binary::Eq, value.clone(), Expr::Int(0)),
                        binary(Binary::Eq, value.clone(), Expr::Int(1)),
                    )));
                }
            }
        }
        for (n, (index, ty)) in &hidden {
            fresh.push(bind(n, read(*index, *ty)));
        }
        fresh.push(restore_flags());
        fresh.push(bind(
            "#result",
            call(
                &mira_core::code::path(&program.code_modules, behavior.id).ok_or("adaptive behavior path missing")?,
                behavior.parameters.iter().map(|p| name(&p.name)).collect(),
            ),
        ));
        named.insert("result".into(), (name("#result"), behavior.output));
        let has_target = named.contains_key("target");
        let mut writes = vec![];
        for (n, update) in &config.updates {
            let &(index, ty) = hidden.get(n).ok_or_else(|| {
                format!("{n}: native update slot is not a declared state_/param_ input")
            })?;
            if n.starts_with("param_") && (!has_target || !config.trainable) {
                continue;
            }
            let update = mira_core::code::find(&program.code_modules, *update)
                .ok_or("adaptive update behavior missing")?;
            if update.output != ty {
                return Err(format!("{n}: update output differs from slot type"));
            }
            let args = update
                .parameters
                .iter()
                .map(|p| {
                    let (value, actual) = named.get(&p.name).ok_or_else(|| {
                        format!(
                            "{}: update input {} unavailable (training needs a target input)",
                            update.name, p.name
                        )
                    })?;
                    if *actual != p.ty {
                        return Err(format!("{}: update input type mismatch", p.name));
                    }
                    Ok(value.clone())
                })
                .collect::<Result<Vec<_>>>()?;
            let next = format!("#next.{index}");
            fresh.push(bind(&next, call(&mira_core::code::path(&program.code_modules, update.id).ok_or("adaptive update path missing")?, args)));
            writes.push(store(index, name(&next), ty));
        }
        fresh.push(Statement::Expr(call("#kernel.cli", vec![])));
        // Changing the activation from inside a learning rule must never publish a stale snapshot.
        fresh.push(require(binary(
            Binary::Eq,
            read(0, Type::I64),
            name("#epoch"),
        )));
        // All update functions observe old values; publish only after they all finish.
        fresh.extend(writes);
        for (p, index) in &input_cache {
            fresh.push(store(*index, name(&p.name), p.ty));
        }
        fresh.push(store(cached_output, name("#result"), behavior.output));
        fresh.push(store(cache_epoch, name("#epoch"), Type::I64));
        fresh.push(store(busy, Expr::Int(0), Type::I64));
        fresh.push(restore_flags());
        fresh.push(Statement::Expr(name("#result")));
        body.push(Statement::If {
            condition: binary(Binary::Eq, read(cache_epoch, Type::I64), name("#epoch")),
            yes: cached,
            no: fresh,
        });
        let id = result.allocate_id()?;
        let mut wrapper_name = format!("__mira_adaptive_{cell}");
        while result.code_modules.iter().any(|m| m.name == wrapper_name) {
            wrapper_name.push('_');
        }
        result.code_modules.push(CodeModule {
            id,
            name: wrapper_name,
            parameters,
            output: behavior.output,
            annotated: true,
            output_ports: false,
            compile_time: false,
            body,
            locals: vec![],
            data_types: vec![],
        });
        result
            .cell_mut(cell)
            .ok_or("adaptive cell disappeared")?
            .definition = Definition::Code(id);
    }
    crate::composition::infer(&mut result)?;
    Ok((result, values))
}
