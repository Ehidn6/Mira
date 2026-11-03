//! Read-only structural queries over the Program being compiled. Lowering emits
//! ordinary bounded Mira IR, so hosted and native code see the same snapshot.
use mira_core::{code::*, *};

pub(crate) const MAX_ROWS: usize = 32_768;
pub(crate) const QUERIES: &[(&str, usize)] = &[
    ("reflect.root", 0),
    ("reflect.definition_count", 0),
    ("reflect.definition_at", 1),
    ("reflect.code_count", 1),
    ("reflect.code_kind", 2),
    ("reflect.code_child_count", 2),
    ("reflect.code_child", 3),
    ("reflect.code_value", 2),
    ("reflect.code_name_byte", 3),
    ("reflect.parameter_count", 1),
    ("reflect.parameter_type", 2),
    ("reflect.result_type", 1),
    ("reflect.protocol_version", 0),
    ("reflect.revision_word", 1),
    ("reflect.rule_count", 0),
    ("reflect.rule_length", 1),
    ("reflect.rule_byte", 2),
    ("reflect.self", 0),
    ("reflect.cell_count", 1),
    ("reflect.cell_at", 2),
    ("reflect.inner", 1),
    ("reflect.parent", 1),
    ("reflect.behavior", 1),
    ("reflect.link_count", 1),
    ("reflect.link_at", 2),
    ("reflect.link_source", 1),
    ("reflect.link_target", 1),
    ("reflect.link_source_port", 1),
    ("reflect.link_target_port", 1),
    ("reflect.input_count", 1),
    ("reflect.output_count", 1),
    ("reflect.port_type", 3),
    ("reflect.port_data_type", 3),
    ("reflect.name_byte", 2),
    ("reflect.port_name_byte", 4),
];
fn word(id: Id) -> Result<i64> {
    i64::try_from(id).map_err(|_| "reflection: identity exceeds the current i64 query ABI".into())
}
fn type_code(ty: Type) -> i64 {
    match ty {
        Type::Infer => 0,
        Type::I64 => 1,
        Type::F32 => 2,
        Type::Bool => 3,
        Type::Event => 4,
        Type::Unit => 5,
        Type::Addr => 6,
        Type::Data(_) => 7,
    }
}
/// Constants have no hidden host state; self is bound to an actual instance.
pub(crate) fn constant(name: &str, program: &Program, owner: Option<Id>) -> Result<Option<i64>> {
    match name {
        "reflect.root" => word(program.root.id).map(Some),
        "reflect.definition_count" => Ok(Some(
            mira_core::code::symbols(&program.code_modules).len() as i64,
        )),
        "reflect.protocol_version" => Ok(Some(mira_core::inspect::PROTOCOL_VERSION)),
        "reflect.rule_count" => Ok(Some(mira_core::inspect::ENTRY_RULES.len() as i64)),
        "reflect.self" => word(owner.ok_or("reflect.self requires a cell instance")?).map(Some),
        _ => Ok(None),
    }
}
fn push(rows: &mut Vec<(Vec<i64>, i64)>, key: Vec<i64>, value: i64) -> Result<()> {
    if rows.len() >= MAX_ROWS {
        return Err("reflection query exceeds structural row budget".into());
    }
    rows.push((key, value));
    Ok(())
}
fn bytes(rows: &mut Vec<(Vec<i64>, i64)>, key: &[i64], name: &str) -> Result<()> {
    for (i, b) in name.bytes().enumerate() {
        let mut key = key.to_vec();
        key.push(i as i64);
        push(rows, key, b as i64)?;
    }
    Ok(())
}
fn ports(rows: &mut Vec<(Vec<i64>, i64)>, query: &str, id: Id, ports: &[Port<'_>]) -> Result<()> {
    let id = word(id)?;
    for (direction, dir) in [(Direction::In, 0), (Direction::Out, 1)] {
        let selected: Vec<_> = ports.iter().filter(|p| p.direction == direction).collect();
        if query == "reflect.input_count" && dir == 0 || query == "reflect.output_count" && dir == 1
        {
            push(rows, vec![id], selected.len() as i64)?;
        }
        for (index, p) in selected.iter().enumerate() {
            let key = vec![id, dir, index as i64];
            match query {
                "reflect.port_type" => push(rows, key, type_code(p.ty))?,
                "reflect.port_data_type" => push(
                    rows,
                    key,
                    if let Type::Data(id) = p.ty {
                        word(id)?
                    } else {
                        0
                    },
                )?,
                "reflect.port_name_byte" => bytes(rows, &key, p.name)?,
                _ => {}
            }
        }
    }
    Ok(())
}
fn endpoint_port(
    program: &Program,
    space: &Space,
    endpoint: &Endpoint,
    source: bool,
) -> Result<i64> {
    if endpoint.cell == space.id {
        let ports = if source {
            &space.inputs
        } else {
            &space.outputs
        };
        return ports
            .iter()
            .position(|p| p.name == endpoint.port)
            .map(|n| n as i64)
            .ok_or("reflection boundary endpoint missing".into());
    }
    program
        .ports(space.cell(endpoint.cell)?)
        .iter()
        .filter(|p| {
            p.direction
                == if source {
                    Direction::Out
                } else {
                    Direction::In
                }
        })
        .position(|p| p.name == endpoint.port)
        .map(|n| n as i64)
        .ok_or("reflection cell endpoint missing".into())
}
fn query_rows(
    name: &str,
    program: &Program,
    source_revision: Option<&blake3::Hash>,
) -> Result<Vec<(Vec<i64>, i64)>> {
    let mut rows = Vec::new();
    if name.starts_with("reflect.code_")
        || matches!(
            name,
            "reflect.definition_at"
                | "reflect.parameter_count"
                | "reflect.parameter_type"
                | "reflect.result_type"
        )
    {
        for (index, (_, d)) in mira_core::code::symbols(&program.code_modules)
            .iter()
            .enumerate()
        {
            let id = word(d.id)?;
            match name {
                "reflect.definition_at" => push(&mut rows, vec![index as i64], id)?,
                "reflect.parameter_count" => push(&mut rows, vec![id], d.parameters.len() as i64)?,
                "reflect.parameter_type" => {
                    for (i, p) in d.parameters.iter().enumerate() {
                        push(&mut rows, vec![id, i as i64], type_code(p.ty))?;
                    }
                }
                "reflect.result_type" => push(&mut rows, vec![id], type_code(d.output))?,
                _ => {
                    let nodes = mira_core::code::expressions(d)?;
                    if name == "reflect.code_count" {
                        push(&mut rows, vec![id], nodes.len() as i64)?;
                        continue;
                    }
                    let paths: std::collections::BTreeMap<_, _> = nodes
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (n.path.clone(), i as i64))
                        .collect();
                    for (i, node) in nodes.iter().enumerate() {
                        let key = vec![id, i as i64];
                        match name {
                            "reflect.code_kind" => {
                                push(&mut rows, key, expression_kind(node.expression))?
                            }
                            "reflect.code_child_count" => {
                                push(&mut rows, key, children(node.expression).len() as i64)?
                            }
                            "reflect.code_child" => {
                                for (slot, _) in children(node.expression).iter().enumerate() {
                                    let mut path = node.path.clone();
                                    path.push(slot);
                                    push(&mut rows, vec![id, i as i64, slot as i64], paths[&path])?;
                                }
                            }
                            "reflect.code_value" => {
                                if let Some(v) = match node.expression {
                                    Expr::Int(v) => Some(*v),
                                    Expr::Float(v) => Some(*v as i64),
                                    Expr::Bool(v) => Some(i64::from(*v)),
                                    _ => None,
                                } {
                                    push(&mut rows, key, v)?;
                                }
                            }
                            "reflect.code_name_byte" => {
                                if let Expr::Name(n)
                                | Expr::Call(n, _)
                                | Expr::Field(_, n)
                                | Expr::Hole(n) = node.expression
                                {
                                    bytes(&mut rows, &key, n)?;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        return Ok(rows);
    }
    if name == "reflect.revision_word" {
        let digest = source_revision
            .copied()
            .unwrap_or_else(|| crate::inspect::revision_digest(program));
        for (index, word) in digest.as_bytes().chunks_exact(8).enumerate() {
            push(
                &mut rows,
                vec![index as i64],
                i64::from_le_bytes(word.try_into().unwrap()),
            )?;
        }
        return Ok(rows);
    }
    if matches!(name, "reflect.rule_length" | "reflect.rule_byte") {
        for (i, rule) in mira_core::inspect::ENTRY_RULES.iter().enumerate() {
            if name == "reflect.rule_length" {
                push(&mut rows, vec![i as i64], rule.len() as i64)?;
            } else {
                bytes(&mut rows, &[i as i64], rule)?;
            }
        }
        return Ok(rows);
    }
    for info in mira_core::inspect::spaces(program) {
        let s = info.space;
        let sid = word(s.id)?;
        match name {
            "reflect.parent" => push(
                &mut rows,
                vec![sid],
                info.owner.map(word).transpose()?.unwrap_or(0),
            )?,
            "reflect.cell_count" => push(&mut rows, vec![sid], s.cells.len() as i64)?,
            "reflect.cell_at" => {
                for (i, c) in s.cells.iter().enumerate() {
                    push(&mut rows, vec![sid, i as i64], word(c.id)?)?;
                }
            }
            "reflect.link_count" => push(&mut rows, vec![sid], s.links.len() as i64)?,
            "reflect.link_at" => {
                for (i, l) in s.links.iter().enumerate() {
                    push(&mut rows, vec![sid, i as i64], word(l.id)?)?;
                }
            }
            "reflect.name_byte" => bytes(&mut rows, &[sid], &s.name)?,
            _ => {}
        }
        let boundary: Vec<_> = s
            .inputs
            .iter()
            .map(|p| Port {
                name: &p.name,
                ty: p.ty,
                direction: Direction::In,
            })
            .chain(s.outputs.iter().map(|p| Port {
                name: &p.name,
                ty: p.ty,
                direction: Direction::Out,
            }))
            .collect();
        ports(&mut rows, name, s.id, &boundary)?;
        for c in &s.cells {
            match name {
                "reflect.parent" => push(&mut rows, vec![word(c.id)?], sid)?,
                "reflect.inner" => push(
                    &mut rows,
                    vec![word(c.id)?],
                    c.space
                        .as_ref()
                        .map(|s| word(s.id))
                        .transpose()?
                        .unwrap_or(0),
                )?,
                "reflect.behavior" => push(
                    &mut rows,
                    vec![word(c.id)?],
                    match c.definition {
                        Definition::Code(id) | Definition::Adaptive(id) => word(id)?,
                        _ => 0,
                    },
                )?,
                "reflect.name_byte" => bytes(&mut rows, &[word(c.id)?], &c.name)?,
                _ => {}
            }
            ports(&mut rows, name, c.id, &program.ports(c))?;
        }
        for l in &s.links {
            let value = match name {
                "reflect.parent" => Some(sid),
                "reflect.link_source" => Some(word(l.from.cell)?),
                "reflect.link_target" => Some(word(l.to.cell)?),
                "reflect.link_source_port" => Some(endpoint_port(program, s, &l.from, true)?),
                "reflect.link_target_port" => Some(endpoint_port(program, s, &l.to, false)?),
                _ => None,
            };
            if let Some(v) = value {
                push(&mut rows, vec![word(l.id)?], v)?;
            }
        }
    }
    // Behavior IDs belong to this same Program identity space. Let source
    // tools resolve them without inventing another set of editor identities.
    if name == "reflect.name_byte" {
        for (_, definition) in mira_core::code::symbols(&program.code_modules) {
            bytes(&mut rows, &[word(definition.id)?], &definition.name)?;
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}
pub(crate) fn evaluate(
    name: &str,
    program: &Program,
    owner: Option<Id>,
    args: &[i64],
) -> Result<i64> {
    let arity = QUERIES
        .iter()
        .find(|(n, _)| *n == name)
        .ok_or("unknown reflection query")?
        .1;
    if args.len() != arity {
        return Err("reflection query arity mismatch".into());
    }
    if let Some(value) = constant(name, program, owner)? {
        return Ok(value);
    }
    let rows = query_rows(name, program, None)?;
    Ok(rows
        .binary_search_by(|row| row.0.as_slice().cmp(args))
        .map(|i| rows[i].1)
        .unwrap_or(-1))
}

/// Query rows are derived compiler data, never another stored Program model.
pub(crate) fn lower(
    name: &str,
    program: &Program,
    source_revision: Option<&blake3::Hash>,
    id: Id,
    label: String,
) -> Result<(CodeModule, usize)> {
    let arity = QUERIES
        .iter()
        .find(|(n, _)| *n == name)
        .ok_or("unknown reflection query")?
        .1;
    let rows = query_rows(name, program, source_revision)?;
    let count = rows.len();
    // Balanced search avoids linear recursion/depth for large graphs. Arguments
    // are procedure parameters: side effects in each argument occur just once.
    fn tree(rows: &[(Vec<i64>, i64)]) -> Expr {
        if rows.is_empty() {
            return Expr::Int(-1);
        }
        let m = rows.len() / 2;
        let (key, value) = &rows[m];
        let mut equal = Expr::Bool(true);
        let mut less = Expr::Bool(false);
        for (i, k) in key.iter().enumerate().rev() {
            let a = Expr::Name(format!("arg{i}"));
            let b = Expr::Int(*k);
            let eq = Expr::Binary(Binary::Eq, Box::new(a.clone()), Box::new(b.clone()));
            equal = Expr::Binary(Binary::And, Box::new(eq.clone()), Box::new(equal));
            less = Expr::Choose(
                Box::new(eq),
                Box::new(less),
                Box::new(Expr::Binary(Binary::Lt, Box::new(a), Box::new(b))),
            );
        }
        Expr::Choose(
            Box::new(equal),
            Box::new(Expr::Int(*value)),
            Box::new(Expr::Choose(
                Box::new(less),
                Box::new(tree(&rows[..m])),
                Box::new(tree(&rows[m + 1..])),
            )),
        )
    }
    Ok((
        CodeModule {
            owner: None,
            id,
            name: label,
            parameters: (0..arity)
                .map(|i| Parameter {
                    name: format!("arg{i}"),
                    ty: Type::I64,
                })
                .collect(),
            output: Type::I64,
            annotated: true,
            output_ports: false,
            compile_time: false,
            body: vec![Statement::Expr(tree(&rows))],
            locals: vec![],
            data_types: vec![],
        },
        count,
    ))
}
