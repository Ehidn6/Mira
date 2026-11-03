//! Deterministic admission of a bounded neural region. No ambient kernel effects.
use mira_core::{code::*, *};
use std::collections::BTreeSet;
pub(crate) fn verify_neural(name: &str, defs: &[CodeModule]) -> Result<()> {
    fn expression(e: &Expr, defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        match e {
            Expr::View(..) => return Err("neural regions cannot create raw memory views".into()),
            Expr::Call(n, args) => {
                if n == "sys.read_only" {
                    let [Expr::Name(target), bytes] = args.as_slice() else {
                        return Err("read-only scope requires a symbol and byte limit".into());
                    };
                    let d = defs
                        .iter()
                        .find(|d| d.name == *target)
                        .ok_or("read-only entry missing")?;
                    visit(d, defs, seen)?;
                    expression(bytes, defs, seen)?;
                    return Ok(());
                }
                if let Some(d) = defs.iter().find(|d| d.name == *n) {
                    visit(d, defs, seen)?;
                } else {
                    let private = n.starts_with("#buffer.")
                        || crate::intrinsics::aligned_type(n).is_some()
                        || n.starts_with("#data.identity.")
                        || n.starts_with("#kernel.slot.")
                        || n.starts_with("#f32.")
                        || matches!(
                            n.as_str(),
                            "#graph.require"
                                | "#kernel.require"
                                | "#kernel.flags"
                                | "#kernel.cli"
                                | "#kernel.sti"
                                | "#kernel.region_required"
                        );
                    let math = matches!(
                        n.as_str(),
                        "math.sqrt" | "math.to_f32" | "math.bits" | "math.from_bits"
                    );
                    let slot_access = matches!(n.as_str(), "sys.load64" | "sys.store64")
                        && matches!(args.first(),Some(Expr::Call(address,a)) if address.starts_with("#kernel.slot.") && a.is_empty());
                    if !private
                        && !math
                        && !crate::intrinsics::pure_integer(n)
                        && !crate::intrinsics::pure_address(n)
                        && !slot_access
                        && !matches!(n.as_str(), "sys.next_tick" | "sys.current_tick")
                    {
                        return Err(format!("{n}: effect is forbidden inside a neural region"));
                    }
                }
                for a in args {
                    expression(a, defs, seen)?;
                }
            }
            Expr::Unary(_, a) | Expr::Field(a, _) => expression(a, defs, seen)?,
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                expression(a, defs, seen)?;
                expression(b, defs, seen)?;
            }
            Expr::Choose(c, a, b) => {
                expression(c, defs, seen)?;
                expression(a, defs, seen)?;
                expression(b, defs, seen)?;
            }
            Expr::Construct(_, args) => {
                for a in args {
                    expression(a, defs, seen)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn body(stmts: &[Statement], defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => expression(e, defs, seen)?,
                Statement::Set { target, value, .. } => {
                    expression(target, defs, seen)?;
                    expression(value, defs, seen)?;
                }
                Statement::If { condition, yes, no } => {
                    expression(condition, defs, seen)?;
                    body(yes, defs, seen)?;
                    body(no, defs, seen)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expression(condition, defs, seen)?;
                    body(inner, defs, seen)?;
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expression(start, defs, seen)?;
                    expression(end, defs, seen)?;
                    body(inner, defs, seen)?;
                }
            }
        }
        Ok(())
    }
    fn visit(d: &CodeModule, defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        if seen.insert(d.id) {
            body(&d.body, defs, seen)?;
        }
        Ok(())
    }
    let d = defs
        .iter()
        .find(|d| d.name == name)
        .ok_or("neural entry missing")?;
    if !d.parameters.is_empty() || d.output != Type::Unit {
        return Err("neural entry must have signature () -> unit".into());
    }
    visit(d, defs, &mut BTreeSet::new())
}

/// Check only reachable kernel code. Unused templates do not imply execution.
pub(crate) fn verify_kernel(entry: Id, defs: &[CodeModule]) -> Result<()> {
    fn expressions(
        e: &Expr,
        in_region: bool,
        defs: &[CodeModule],
        seen: &mut BTreeSet<(Id, bool)>,
    ) -> Result<()> {
        match e {
            Expr::Call(n, args) => {
                if n == "#kernel.region_required" && !in_region {
                    return Err("native Adaptive activation requires sys.neural_try(entry, operations, scratch_bytes)".into());
                }
                if matches!(
                    n.as_str(),
                    "sys.neural_try" | "sys.scratch" | "sys.read_only"
                ) || crate::intrinsics::symbol_reference(n)
                {
                    if let Some(Expr::Name(target)) = args.first() {
                        let d = defs
                            .iter()
                            .find(|d| d.name == *target)
                            .ok_or("callback missing")?;
                        visit(d, in_region || n == "sys.neural_try", defs, seen)?;
                    }
                    for a in args.iter().skip(1) {
                        expressions(a, in_region, defs, seen)?;
                    }
                } else {
                    if let Some(d) = defs.iter().find(|d| d.name == *n) {
                        visit(d, in_region, defs, seen)?;
                    }
                    for a in args {
                        expressions(a, in_region, defs, seen)?;
                    }
                }
            }
            Expr::Unary(_, a) | Expr::Field(a, _) | Expr::View(_, a) => {
                expressions(a, in_region, defs, seen)?
            }
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                expressions(a, in_region, defs, seen)?;
                expressions(b, in_region, defs, seen)?;
            }
            Expr::Choose(c, a, b) => {
                expressions(c, in_region, defs, seen)?;
                expressions(a, in_region, defs, seen)?;
                expressions(b, in_region, defs, seen)?;
            }
            Expr::Construct(_, args) => {
                for a in args {
                    expressions(a, in_region, defs, seen)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn body(
        stmts: &[Statement],
        region: bool,
        defs: &[CodeModule],
        seen: &mut BTreeSet<(Id, bool)>,
    ) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => {
                    expressions(e, region, defs, seen)?
                }
                Statement::Set { target, value, .. } => {
                    expressions(target, region, defs, seen)?;
                    expressions(value, region, defs, seen)?;
                }
                Statement::If { condition, yes, no } => {
                    expressions(condition, region, defs, seen)?;
                    body(yes, region, defs, seen)?;
                    body(no, region, defs, seen)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expressions(condition, region, defs, seen)?;
                    body(inner, region, defs, seen)?;
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expressions(start, region, defs, seen)?;
                    expressions(end, region, defs, seen)?;
                    body(inner, region, defs, seen)?;
                }
            }
        }
        Ok(())
    }
    fn visit(
        d: &CodeModule,
        region: bool,
        defs: &[CodeModule],
        seen: &mut BTreeSet<(Id, bool)>,
    ) -> Result<()> {
        if seen.insert((d.id, region)) {
            body(&d.body, region, defs, seen)?;
        }
        Ok(())
    }
    visit(
        defs.iter()
            .find(|d| d.id == entry)
            .ok_or("kernel entry missing")?,
        false,
        defs,
        &mut BTreeSet::new(),
    )
}

/// Scope-local aggregates can only reach fresh objects or flat persistent buffers.
/// No raw aggregate views or aggregate return values may escape this boundary.
pub(crate) fn verify_scratch(name: &str, defs: &[CodeModule]) -> Result<()> {
    fn expr(e: &Expr, defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        match e {
            Expr::View(..) => return Err("scratch entries cannot create raw aggregate views; use scalar sys.load/store or persistent buffers".into()),
            Expr::Call(n, args) => {
                let callback = matches!(n.as_str(), "sys.scratch" | "sys.neural_try" | "sys.read_only") || crate::intrinsics::handler_kind(n).is_some();
                if crate::intrinsics::dynamic_native(n) || n == "sys.procedure_address" {
                    return Err(format!("{n}: scratch entries cannot expose temporary addresses or call unbounded foreign code"));
                }
                let target = if callback { match args.first() {Some(Expr::Name(s)) => s, _ => return Err("callback requires a symbol".into())} } else { n };
                if let Some(d) = defs.iter().find(|d| d.name == *target) { visit(d,defs,seen)?; }
                for a in args.iter().skip(usize::from(callback)) {expr(a,defs,seen)?;}
            }
            Expr::Unary(_,a) | Expr::Field(a,_) => expr(a,defs,seen)?,
            Expr::Index(a,b) | Expr::Binary(_,a,b) => {expr(a,defs,seen)?;expr(b,defs,seen)?;}
            Expr::Choose(c,a,b) => {expr(c,defs,seen)?;expr(a,defs,seen)?;expr(b,defs,seen)?;}
            Expr::Construct(_,args) => {for a in args {expr(a,defs,seen)?;}}
            _ => {}
        }
        Ok(())
    }
    fn body(stmts: &[Statement], defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e, defs, seen)?,
                Statement::Set { target, value, .. } => {
                    expr(target, defs, seen)?;
                    expr(value, defs, seen)?;
                }
                Statement::If { condition, yes, no } => {
                    expr(condition, defs, seen)?;
                    body(yes, defs, seen)?;
                    body(no, defs, seen)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expr(condition, defs, seen)?;
                    body(inner, defs, seen)?;
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expr(start, defs, seen)?;
                    expr(end, defs, seen)?;
                    body(inner, defs, seen)?;
                }
            }
        }
        Ok(())
    }
    fn visit(d: &CodeModule, defs: &[CodeModule], seen: &mut BTreeSet<Id>) -> Result<()> {
        if seen.insert(d.id) {
            body(&d.body, defs, seen)?;
        }
        Ok(())
    }
    let d = defs
        .iter()
        .find(|d| d.name == name)
        .ok_or("scratch entry missing")?;
    if !d.parameters.is_empty() || matches!(d.output, Type::Data(_)) {
        return Err("scratch entry cannot return a temporary reference or accept inputs".into());
    }
    visit(d, defs, &mut BTreeSet::new())
}
