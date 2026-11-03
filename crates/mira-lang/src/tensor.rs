//! Shape-checked tensor/relationship operations lowered to ordinary Mira IR.
use mira_core::{code::*, *};
use std::collections::BTreeMap;
/// Literal UTF-8 bytes initialize an existing word array; no separate string heap.
pub(crate) fn text_type(id: Id, args: &[Expr], types: &[data::Definition]) -> Option<Result<Id>> {
    let [Expr::Call(name, args)] = args else {
        return None;
    };
    if name != "array.literal_utf8" {
        return None;
    }
    Some((|| {
        let definition = data::find(types, id)?;
        let data::Kind::Array {
            element: Type::I64,
            length,
        } = definition.kind
        else {
            return Err("text literal requires an i64 array".into());
        };
        if args.len() > length {
            return Err(format!(
                "UTF-8 text requires {} words, array has {length}",
                args.len()
            ));
        }
        let bytes = args
            .iter()
            .map(|e| match e {
                Expr::Int(n) if (0..=255).contains(n) => Ok(*n as u8),
                _ => Err("text initializer must contain literal bytes"),
            })
            .collect::<core::result::Result<Vec<_>, _>>()?;
        core::str::from_utf8(&bytes).map_err(|_| "text initializer is not UTF-8")?;
        Ok(id)
    })())
}
fn layout(ty: Type, types: &[data::Definition]) -> Result<(Type, Vec<usize>)> {
    let Type::Data(id) = ty else {
        return Err("tensor value required".into());
    };
    match &data::find(types, id)?.kind {
        data::Kind::Tensor { element, shape } => Ok((*element, shape.clone())),
        data::Kind::Array { element, length } => Ok((*element, vec![*length])),
        _ => Err("dense tensor/array required".into()),
    }
}
fn fshape(ty: Type, types: &[data::Definition]) -> Result<Vec<usize>> {
    let (el, shape) = layout(ty, types)?;
    if el != Type::F32 {
        return Err("operation requires f32 tensor elements".into());
    }
    Ok(shape)
}
fn count(shape: &[usize]) -> usize {
    shape.iter().product()
}
/// Physical extent of one named aggregate in the current word-based ABI.
/// Data-valued members occupy reference words rather than inline child storage.
fn layout_constant(name: &str, types: &[data::Definition]) -> Option<Result<i64>> {
    if let Some((base, field)) = name.split_once(".offset_of.") {
        return Some((|| {
            let definition = types
                .iter()
                .find(|d| d.name == base)
                .ok_or_else(|| format!("{base}: unknown aggregate layout type"))?;
            let (index, _) = definition.field(field).ok_or_else(|| {
                format!("{base}: offset_of requires a declared record field, got {field}")
            })?;
            index
                .checked_mul(8)
                .and_then(|n| i64::try_from(n).ok())
                .ok_or_else(|| "field offset exceeds addressable layout".into())
        })());
    }
    let (base, size) = if let Some(base) = name.strip_suffix(".size_bytes") {
        (base, true)
    } else if let Some(base) = name.strip_suffix(".alignment") {
        (base, false)
    } else {
        return None;
    };
    Some((|| {
        let definition = types
            .iter()
            .find(|d| d.name == base)
            .ok_or("unknown aggregate layout type")?;
        if !size {
            return Ok(8);
        }
        definition
            .len()
            .max(1)
            .checked_mul(8)
            .and_then(|n| i64::try_from(n).ok())
            .ok_or_else(|| "aggregate byte size exceeds addressable layout".into())
    })())
}
pub(crate) fn is_layout_query(name: &str) -> bool {
    name.ends_with(".size_bytes") || name.ends_with(".alignment") || name.contains(".offset_of.")
}
pub(crate) fn signature(
    n: &str,
    args: &[Type],
    types: &[data::Definition],
) -> Option<Result<Type>> {
    if let Some(base) = n.strip_suffix(".aligned") {
        return Some((|| {
            if args != [Type::I64] {
                return Err("Type.aligned expects one i64 alignment".into());
            }
            let ty = types
                .iter()
                .find(|t| t.name == base)
                .ok_or("unknown aligned allocation type")?;
            if ty.member_types().iter().any(|t| matches!(t, Type::Data(_))) {
                return Err(
                    "aligned zero construction cannot initialize aggregate references".into(),
                );
            }
            Ok(Type::Data(ty.id))
        })());
    }
    if let Some(value) = layout_constant(n, types) {
        return Some(if args.is_empty() {
            value.map(|_| Type::I64)
        } else {
            Err("layout query takes no arguments".into())
        });
    }
    if let Some(base) = n.strip_suffix(".persist") {
        return Some((|| {
            if !args.is_empty() {
                return Err("persistent buffer accessor takes no inputs".into());
            }
            let t = types
                .iter()
                .find(|t| t.name == base)
                .ok_or("unknown persistent buffer type")?;
            if t.len() > 65536 {
                return Err("persistent buffer exceeds word limit".into());
            }
            if t.member_types()
                .iter()
                .any(|ty| !matches!(ty, Type::I64 | Type::F32 | Type::Bool))
            {
                return Err("persistent buffers cannot contain activation references".into());
            }
            Ok(Type::Data(t.id))
        })());
    }
    if !n.starts_with("tensor.") && !n.starts_with("relation.") {
        return None;
    }
    if n.starts_with("relation.") {
        return Some(relation_signature(n, args, types));
    }
    Some((|| {
        let arity = match n {
            "tensor.sum" => 1,
            "tensor.copy_into" | "tensor.normalize_rows_into" => 2,
            "tensor.fill" => 2,
            "tensor.add_into"
            | "tensor.sub_into"
            | "tensor.mul_into"
            | "tensor.div_into"
            | "tensor.scale_into"
            | "tensor.matmul_into"
            | "tensor.gather_into"
            | "tensor.aggregate_into" => 3,
            _ => return Err(format!("{n}: unknown bulk operation")),
        };
        if args.len() != arity {
            return Err(format!("{n}: expected {arity} arguments"));
        }
        match n {
            "tensor.sum" => {
                fshape(args[0], types)?;
                return Ok(Type::F32);
            }
            "tensor.fill" => {
                fshape(args[0], types)?;
                if args[1] != Type::F32 {
                    return Err("fill value must be f32".into());
                }
            }
            "tensor.copy_into" => {
                if layout(args[0], types)? != layout(args[1], types)? {
                    return Err("copy tensor shapes/types differ".into());
                }
            }
            "tensor.normalize_rows_into" => {
                let s = fshape(args[0], types)?;
                if s.len() != 2 || s != fshape(args[1], types)? {
                    return Err("normalization requires equal rank-2 shapes".into());
                }
            }
            "tensor.add_into" | "tensor.sub_into" | "tensor.mul_into" | "tensor.div_into" => {
                let s = fshape(args[0], types)?;
                if s != fshape(args[1], types)? || s != fshape(args[2], types)? {
                    return Err("elementwise tensor shapes differ".into());
                }
            }
            "tensor.scale_into" => {
                if fshape(args[0], types)? != fshape(args[1], types)? || args[2] != Type::F32 {
                    return Err("scale requires equal tensor shapes and f32 factor".into());
                }
            }
            "tensor.matmul_into" => {
                let out = fshape(args[0], types)?;
                let a = fshape(args[1], types)?;
                let b = fshape(args[2], types)?;
                if out.len() != 2
                    || a.len() != 2
                    || b.len() != 2
                    || a[1] != b[0]
                    || out != vec![a[0], b[1]]
                {
                    return Err("incompatible matmul shapes".into());
                }
            }
            "tensor.gather_into" | "tensor.aggregate_into" => {
                let out = fshape(args[0], types)?;
                let data = fshape(args[1], types)?;
                let (el, indices) = layout(args[2], types)?;
                if out.len() != 2
                    || data.len() != 2
                    || indices.len() != 1
                    || el != Type::I64
                    || out[1] != data[1]
                {
                    return Err("gather/aggregation shape or index type mismatch".into());
                }
                if n == "tensor.gather_into" && out[0] != indices[0]
                    || n == "tensor.aggregate_into" && data[0] != indices[0]
                {
                    return Err("index count differs from message count".into());
                }
            }
            _ => unreachable!(),
        }
        Ok(Type::Unit)
    })())
}
#[derive(Clone)]
struct RelationLayout {
    source: Type,
    target: Type,
    source_indices: Type,
    target_indices: Type,
    source_rows: usize,
    target_rows: usize,
    source_width: usize,
    edges: usize,
    active: Option<Type>,
}
fn relation_layout(ty: Type, types: &[data::Definition]) -> Result<RelationLayout> {
    let Type::Data(id) = ty else {
        return Err("typed relation record required".into());
    };
    let relation = data::find(types, id)?;
    let field = |name| {
        relation
            .field(name)
            .map(|(_, ty)| ty)
            .ok_or_else(|| format!("relation needs {name}"))
    };
    let source = field("sources")?;
    let target = field("targets")?;
    let source_indices = field("source_indices")?;
    let target_indices = field("target_indices")?;
    let source_shape = fshape(source, types)?;
    let target_shape = fshape(target, types)?;
    let (se, ss) = layout(source_indices, types)?;
    let (de, ds) = layout(target_indices, types)?;
    if source_shape.len() != 2
        || target_shape.len() != 2
        || se != Type::I64
        || de != Type::I64
        || ss.len() != 1
        || ss != ds
    {
        return Err("invalid typed relation endpoints/indices".into());
    }
    if let Some((_, features)) = relation.field("features") {
        let fs = fshape(features, types)?;
        if fs.len() != 2 || fs[0] != ss[0] {
            return Err("relation edge feature count mismatch".into());
        }
    }
    let active = relation.field("active").map(|(_, ty)| ty);
    if let Some(ty) = active {
        let (element, shape) = layout(ty, types)?;
        if element != Type::Bool || shape != ss {
            return Err("relation active mask must have one bool per edge slot".into());
        }
    }
    Ok(RelationLayout {
        source,
        target,
        source_indices,
        target_indices,
        source_rows: source_shape[0],
        target_rows: target_shape[0],
        source_width: source_shape[1],
        edges: ss[0],
        active,
    })
}
fn relation_signature(name: &str, args: &[Type], types: &[data::Definition]) -> Result<Type> {
    let (arity, relation_arg) = match name {
        "relation.messages_into" => (2, 1),
        "relation.aggregate_into" | "relation.neighbors_into" => (3, 1),
        "relation.active_count" => (1, 0),
        "relation.add_edge" => (3, 0),
        "relation.remove_edge" => (2, 0),
        _ => return Err(format!("{name}: unknown relation operation")),
    };
    if args.len() != arity {
        return Err(format!("{name}: expected {arity} arguments"));
    }
    let relation = relation_layout(args[relation_arg], types)?;
    match name {
        "relation.messages_into" => {
            signature(
                "tensor.gather_into",
                &[args[0], relation.source, relation.source_indices],
                types,
            )
            .unwrap()?;
            Ok(Type::Unit)
        }
        "relation.aggregate_into" => {
            if args[0] != relation.target {
                return Err("aggregation output must have the declared target node type".into());
            }
            signature(
                "tensor.aggregate_into",
                &[args[0], args[2], relation.target_indices],
                types,
            )
            .unwrap()?;
            Ok(Type::Unit)
        }
        "relation.neighbors_into" => {
            let (element, shape) = layout(args[0], types)?;
            if element != Type::I64
                || shape.len() != 1
                || shape[0] < relation.edges
                || args[2] != Type::I64
            {
                return Err("neighbors require an i64 output with capacity for all edge slots and an i64 target".into());
            }
            Ok(Type::I64)
        }
        "relation.active_count" => Ok(Type::I64),
        "relation.add_edge" | "relation.remove_edge" => {
            if relation.active.is_none() {
                return Err("structural relation changes require an active bool mask".into());
            }
            if args.iter().skip(1).any(|ty| *ty != Type::I64) {
                return Err("relation node and edge indices must be i64".into());
            }
            Ok(if name == "relation.add_edge" {
                Type::I64
            } else {
                Type::Bool
            })
        }
        _ => unreachable!(),
    }
}
fn n(s: &str) -> Expr {
    Expr::Name(s.into())
}
fn i(v: usize) -> Expr {
    Expr::Int(v as i64)
}
fn f(v: f32) -> Expr {
    Expr::Float(v.to_bits())
}
fn bin(op: Binary, a: Expr, b: Expr) -> Expr {
    Expr::Binary(op, Box::new(a), Box::new(b))
}
fn idx(a: Expr, k: Expr) -> Expr {
    Expr::Index(Box::new(a), Box::new(k))
}
fn call(s: &str, args: Vec<Expr>) -> Expr {
    Expr::Call(s.into(), args)
}
fn bind(s: &str, v: Expr) -> Statement {
    Statement::Bind {
        name: s.into(),
        ty: None,
        immutable: false,
        op: Assign::Set,
        value: v,
    }
}
fn set(a: Expr, v: Expr) -> Statement {
    Statement::Set {
        target: a,
        op: Assign::Set,
        value: v,
    }
}
fn repeat(s: &str, len: usize, body: Vec<Statement>) -> Statement {
    Statement::For {
        name: s.into(),
        start: Expr::Int(0),
        end: i(len),
        body,
    }
}
fn rfield(arg: usize, name: &str) -> Expr {
    Expr::Field(Box::new(n(&format!("a{arg}"))), name.into())
}
fn require(condition: Expr) -> Statement {
    Statement::Expr(call("#graph.require", vec![condition]))
}
fn within(index: Expr, size: usize) -> Expr {
    bin(
        Binary::And,
        bin(Binary::Ge, index.clone(), i(0)),
        bin(Binary::Lt, index, i(size)),
    )
}
fn same_storage(a: Expr, at: Type, b: Expr, bt: Type) -> Expr {
    let (Type::Data(aid), Type::Data(bid)) = (at, bt) else {
        unreachable!()
    };
    bin(
        Binary::Eq,
        call(&format!("#data.identity.{aid}"), vec![a]),
        call(&format!("#data.identity.{bid}"), vec![b]),
    )
}
fn separate(a: Expr, at: Type, b: Expr, bt: Type) -> Statement {
    require(Expr::Unary(
        Unary::Not,
        Box::new(same_storage(a, at, b, bt)),
    ))
}
fn relation_body(op: &str, args: &[Type], types: &[data::Definition]) -> Result<Vec<Statement>> {
    let r = usize::from(matches!(
        op,
        "relation.messages_into" | "relation.aggregate_into" | "relation.neighbors_into"
    ));
    let relation = relation_layout(args[r], types)?;
    let field = |name| rfield(r, name);
    let active = || {
        relation
            .active
            .map_or(Expr::Bool(true), |_| idx(field("active"), n("edge")))
    };
    let checks = vec![
        require(within(
            idx(field("source_indices"), n("edge")),
            relation.source_rows,
        )),
        require(within(
            idx(field("target_indices"), n("edge")),
            relation.target_rows,
        )),
    ];
    let validate = repeat(
        "edge",
        relation.edges,
        vec![Statement::If {
            condition: active(),
            yes: checks,
            no: vec![],
        }],
    );
    let mut body = Vec::new();
    // Removing a corrupt edge is allowed: it only clears a checked mask slot.
    if op != "relation.remove_edge" {
        body.push(validate);
    }
    match op {
        "relation.active_count" => {
            body.push(bind("found", i(0)));
            body.push(repeat(
                "edge",
                relation.edges,
                vec![Statement::If {
                    condition: active(),
                    yes: vec![bind("found", bin(Binary::Add, n("found"), i(1)))],
                    no: vec![],
                }],
            ));
            body.push(Statement::Expr(n("found")));
        }
        "relation.add_edge" => {
            body.push(require(within(n("a1"), relation.source_rows)));
            body.push(require(within(n("a2"), relation.target_rows)));
            body.push(separate(
                field("source_indices"),
                relation.source_indices,
                field("target_indices"),
                relation.target_indices,
            ));
            body.push(bind("slot", Expr::Int(-1)));
            body.push(repeat(
                "edge",
                relation.edges,
                vec![Statement::If {
                    condition: bin(
                        Binary::And,
                        bin(Binary::Eq, n("slot"), Expr::Int(-1)),
                        Expr::Unary(Unary::Not, Box::new(active())),
                    ),
                    yes: vec![bind("slot", n("edge"))],
                    no: vec![],
                }],
            ));
            // The active flag is published last. Edge feature contents are intentionally
            // unchanged; the caller initializes its own state/parameters in neural_try.
            body.push(Statement::If {
                condition: bin(Binary::Ge, n("slot"), i(0)),
                yes: vec![
                    set(idx(field("source_indices"), n("slot")), n("a1")),
                    set(idx(field("target_indices"), n("slot")), n("a2")),
                    set(idx(field("active"), n("slot")), Expr::Bool(true)),
                ],
                no: vec![],
            });
            body.push(Statement::Expr(n("slot")));
        }
        "relation.remove_edge" => {
            body.push(require(within(n("a1"), relation.edges)));
            body.push(bind("was_active", idx(field("active"), n("a1"))));
            body.push(set(idx(field("active"), n("a1")), Expr::Bool(false)));
            body.push(Statement::Expr(n("was_active")));
        }
        "relation.messages_into" => {
            let width = relation.source_width;
            body.push(separate(
                n("a0"),
                args[0],
                field("sources"),
                relation.source,
            ));
            let destination = idx(
                n("a0"),
                bin(Binary::Add, bin(Binary::Mul, n("edge"), i(width)), n("j")),
            );
            let source = idx(
                field("sources"),
                bin(
                    Binary::Add,
                    bin(
                        Binary::Mul,
                        idx(field("source_indices"), n("edge")),
                        i(width),
                    ),
                    n("j"),
                ),
            );
            body.push(repeat(
                "edge",
                relation.edges,
                vec![repeat(
                    "j",
                    width,
                    vec![set(
                        destination,
                        Expr::Choose(Box::new(active()), Box::new(source), Box::new(f(0.0))),
                    )],
                )],
            ));
        }
        "relation.aggregate_into" => {
            let shape = fshape(args[0], types)?;
            body.push(distinct(0, args[0], 2, args[2]));
            body.push(repeat(
                "j",
                count(&shape),
                vec![set(idx(n("a0"), n("j")), f(0.0))],
            ));
            let destination = idx(
                n("a0"),
                bin(
                    Binary::Add,
                    bin(
                        Binary::Mul,
                        idx(field("target_indices"), n("edge")),
                        i(shape[1]),
                    ),
                    n("j"),
                ),
            );
            let source = idx(
                n("a2"),
                bin(
                    Binary::Add,
                    bin(Binary::Mul, n("edge"), i(shape[1])),
                    n("j"),
                ),
            );
            body.push(repeat(
                "edge",
                relation.edges,
                vec![Statement::If {
                    condition: active(),
                    yes: vec![repeat(
                        "j",
                        shape[1],
                        vec![Statement::Set {
                            target: destination,
                            op: Assign::Add,
                            value: source,
                        }],
                    )],
                    no: vec![],
                }],
            ));
        }
        "relation.neighbors_into" => {
            body.push(require(within(n("a2"), relation.target_rows)));
            body.push(separate(
                n("a0"),
                args[0],
                field("source_indices"),
                relation.source_indices,
            ));
            body.push(separate(
                n("a0"),
                args[0],
                field("target_indices"),
                relation.target_indices,
            ));
            body.push(bind("found", i(0)));
            body.push(repeat(
                "edge",
                relation.edges,
                vec![Statement::If {
                    condition: bin(
                        Binary::And,
                        active(),
                        bin(Binary::Eq, idx(field("target_indices"), n("edge")), n("a2")),
                    ),
                    yes: vec![
                        set(
                            idx(n("a0"), n("found")),
                            idx(field("source_indices"), n("edge")),
                        ),
                        bind("found", bin(Binary::Add, n("found"), i(1))),
                    ],
                    no: vec![],
                }],
            ));
            body.push(Statement::Expr(n("found")));
        }
        _ => return Err("unknown relation lowering".into()),
    }
    Ok(body)
}
fn distinct(a: usize, at: Type, b: usize, bt: Type) -> Statement {
    let aid = if let Type::Data(id) = at { id } else { 0 };
    let bid = if let Type::Data(id) = bt { id } else { 0 };
    Statement::Expr(call(
        "#graph.require",
        vec![bin(
            Binary::Ne,
            call(&format!("#data.identity.{aid}"), vec![n(&format!("a{a}"))]),
            call(&format!("#data.identity.{bid}"), vec![n(&format!("a{b}"))]),
        )],
    ))
}
fn build(op: &str, args: &[Type], types: &[data::Definition], id: Id) -> Result<CodeModule> {
    let output = signature(op, args, types).ok_or("unknown tensor operator")??;
    let len = if op.starts_with("relation.") {
        0
    } else {
        count(&layout(args[0], types)?.1)
    };
    let target = |index: Expr| idx(n("a0"), index);
    let body = match op {
        "tensor.copy_into" => vec![repeat(
            "j",
            len,
            vec![set(target(n("j")), idx(n("a1"), n("j")))],
        )],
        "tensor.fill" => vec![repeat("j", len, vec![set(target(n("j")), n("a1"))])],
        "tensor.sum" => vec![
            bind("sum", f(0.0)),
            repeat(
                "j",
                len,
                vec![Statement::Bind {
                    name: "sum".into(),
                    ty: None,
                    immutable: false,
                    op: Assign::Add,
                    value: idx(n("a0"), n("j")),
                }],
            ),
            Statement::Expr(n("sum")),
        ],
        "tensor.add_into" | "tensor.sub_into" | "tensor.mul_into" | "tensor.div_into" => {
            vec![repeat(
                "j",
                len,
                vec![set(
                    target(n("j")),
                    bin(
                        match op {
                            "tensor.add_into" => Binary::Add,
                            "tensor.sub_into" => Binary::Sub,
                            "tensor.mul_into" => Binary::Mul,
                            _ => Binary::Div,
                        },
                        idx(n("a1"), n("j")),
                        idx(n("a2"), n("j")),
                    ),
                )],
            )]
        }
        "tensor.scale_into" => vec![repeat(
            "j",
            len,
            vec![set(
                target(n("j")),
                bin(Binary::Mul, idx(n("a1"), n("j")), n("a2")),
            )],
        )],
        "tensor.matmul_into" => {
            let a = fshape(args[1], types)?;
            let b = fshape(args[2], types)?;
            vec![
                distinct(0, args[0], 1, args[1]),
                distinct(0, args[0], 2, args[2]),
                repeat(
                    "row",
                    a[0],
                    vec![repeat(
                        "col",
                        b[1],
                        vec![
                            bind("sum", f(0.0)),
                            repeat(
                                "k",
                                a[1],
                                vec![Statement::Bind {
                                    name: "sum".into(),
                                    ty: None,
                                    immutable: false,
                                    op: Assign::Add,
                                    value: bin(
                                        Binary::Mul,
                                        idx(
                                            n("a1"),
                                            bin(
                                                Binary::Add,
                                                bin(Binary::Mul, n("row"), i(a[1])),
                                                n("k"),
                                            ),
                                        ),
                                        idx(
                                            n("a2"),
                                            bin(
                                                Binary::Add,
                                                bin(Binary::Mul, n("k"), i(b[1])),
                                                n("col"),
                                            ),
                                        ),
                                    ),
                                }],
                            ),
                            set(
                                target(bin(
                                    Binary::Add,
                                    bin(Binary::Mul, n("row"), i(b[1])),
                                    n("col"),
                                )),
                                n("sum"),
                            ),
                        ],
                    )],
                ),
            ]
        }
        "tensor.gather_into" | "tensor.aggregate_into" => {
            let o = fshape(args[0], types)?;
            let a = fshape(args[1], types)?;
            let m = layout(args[2], types)?.1[0];
            let gather = op == "tensor.gather_into";
            let mut body = vec![distinct(0, args[0], 1, args[1])];
            // Validate all indices before destination writes, including empty neighborhoods.
            body.push(repeat(
                "edge",
                m,
                vec![Statement::Expr(call(
                    "#graph.require",
                    vec![bin(
                        Binary::And,
                        bin(Binary::Ge, idx(n("a2"), n("edge")), Expr::Int(0)),
                        bin(
                            Binary::Lt,
                            idx(n("a2"), n("edge")),
                            i(if gather { a[0] } else { o[0] }),
                        ),
                    )],
                ))],
            ));
            if !gather {
                body.push(repeat("j", len, vec![set(target(n("j")), f(0.0))]));
            }
            let to = bin(
                Binary::Add,
                bin(
                    Binary::Mul,
                    if gather {
                        n("edge")
                    } else {
                        idx(n("a2"), n("edge"))
                    },
                    i(o[1]),
                ),
                n("j"),
            );
            let from = bin(
                Binary::Add,
                bin(
                    Binary::Mul,
                    if gather {
                        idx(n("a2"), n("edge"))
                    } else {
                        n("edge")
                    },
                    i(a[1]),
                ),
                n("j"),
            );
            body.push(repeat(
                "edge",
                m,
                vec![repeat(
                    "j",
                    o[1],
                    vec![Statement::Set {
                        target: target(to),
                        op: if gather { Assign::Set } else { Assign::Add },
                        value: idx(n("a1"), from),
                    }],
                )],
            ));
            body
        }
        "tensor.normalize_rows_into" => {
            let shape = fshape(args[0], types)?;
            let at = bin(Binary::Add, bin(Binary::Mul, n("row"), i(shape[1])), n("j"));
            vec![repeat(
                "row",
                shape[0],
                vec![
                    bind("norm", f(0.0)),
                    repeat(
                        "j",
                        shape[1],
                        vec![Statement::Bind {
                            name: "norm".into(),
                            ty: None,
                            immutable: false,
                            op: Assign::Add,
                            value: bin(
                                Binary::Mul,
                                idx(n("a1"), at.clone()),
                                idx(n("a1"), at.clone()),
                            ),
                        }],
                    ),
                    bind("norm", call("math.sqrt", vec![n("norm")])),
                    repeat(
                        "j",
                        shape[1],
                        vec![set(
                            target(at.clone()),
                            Expr::Choose(
                                Box::new(bin(Binary::Eq, n("norm"), f(0.0))),
                                Box::new(f(0.0)),
                                Box::new(bin(Binary::Div, idx(n("a1"), at), n("norm"))),
                            ),
                        )],
                    ),
                ],
            )]
        }
        name if name.starts_with("relation.") => relation_body(name, args, types)?,
        _ => return Err("unknown tensor lowering".into()),
    };
    Ok(CodeModule {
        id,
        name: format!("#{id}"),
        parameters: args
            .iter()
            .enumerate()
            .map(|(j, ty)| Parameter {
                name: format!("a{j}"),
                ty: *ty,
            })
            .collect(),
        output,
        annotated: true,
        output_ports: false,
        compile_time: false,
        body,
        locals: vec![],
        data_types: vec![],
    })
}
pub(crate) fn lower(defs: &mut Vec<CodeModule>, types: &[data::Definition]) -> Result<()> {
    let mut cache = BTreeMap::<String, String>::new();
    let mut next = defs
        .iter()
        .map(|d| d.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("tensor function identity exhausted")?;
    fn expr(
        e: &mut Expr,
        env: &BTreeMap<String, Type>,
        defs: &[CodeModule],
        types: &[data::Definition],
        cache: &mut BTreeMap<String, String>,
        next: &mut Id,
        added: &mut Vec<CodeModule>,
    ) -> Result<()> {
        match e {
            Expr::Call(op, args) => {
                if let Some(value) = layout_constant(op, types) {
                    if !args.is_empty() {
                        return Err("layout query takes no arguments".into());
                    }
                    *e = Expr::Int(value?);
                    return Ok(());
                }
                if crate::intrinsics::symbol_reference(op)
                    || matches!(
                        op.as_str(),
                        "sys.neural_try" | "sys.scratch" | "sys.read_only"
                    )
                {
                    // Callback symbols are metadata, but their budget/size expressions
                    // are ordinary code and may contain aggregate layout queries.
                    for a in args.iter_mut().skip(1) {
                        expr(a, env, defs, types, cache, next, added)?;
                    }
                    return Ok(());
                }
                let argtypes = args
                    .iter()
                    .map(|a| crate::behavior::expression_type(a, env, defs, types))
                    .collect::<Result<Vec<_>>>()?;
                for a in args.iter_mut() {
                    expr(a, env, defs, types, cache, next, added)?;
                }
                if let Some(base) = op.strip_suffix(".aligned") {
                    let ty = types
                        .iter()
                        .find(|t| t.name == base)
                        .ok_or("unknown aligned allocation type")?;
                    *op = format!("#data.aligned.{}", ty.id);
                    return Ok(());
                }
                if op.starts_with("tensor.") || op.starts_with("relation.") {
                    let key = format!("{op}:{argtypes:?}");
                    let replacement = if let Some(n) = cache.get(&key) {
                        n.clone()
                    } else {
                        let id = *next;
                        *next = next
                            .checked_add(1)
                            .ok_or("tensor function identity exhausted")?;
                        let d = build(op, &argtypes, types, id)?;
                        let n = d.name.clone();
                        cache.insert(key, n.clone());
                        added.push(d);
                        n
                    };
                    *op = replacement;
                }
            }
            Expr::Unary(_, a) | Expr::View(_, a) | Expr::Field(a, _) => {
                expr(a, env, defs, types, cache, next, added)?
            }
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                expr(a, env, defs, types, cache, next, added)?;
                expr(b, env, defs, types, cache, next, added)?;
            }
            Expr::Construct(id, args) => {
                if let Some(validated) = text_type(*id, args, types) {
                    validated?;
                    let Expr::Call(_, bytes) = &args[0] else {
                        unreachable!()
                    };
                    let mut values = bytes.clone();
                    values.resize(data::find(types, *id)?.len(), Expr::Int(0));
                    *args = values;
                    return Ok(());
                }
                for a in args {
                    expr(a, env, defs, types, cache, next, added)?;
                }
            }
            Expr::Choose(c, a, b) => {
                expr(c, env, defs, types, cache, next, added)?;
                expr(a, env, defs, types, cache, next, added)?;
                expr(b, env, defs, types, cache, next, added)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn body(
        stmts: &mut [Statement],
        env: &mut BTreeMap<String, Type>,
        defs: &[CodeModule],
        types: &[data::Definition],
        cache: &mut BTreeMap<String, String>,
        next: &mut Id,
        added: &mut Vec<CodeModule>,
    ) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Expr(e) => expr(e, env, defs, types, cache, next, added)?,
                Statement::Bind {
                    name, ty, value, ..
                } => {
                    let value_type = crate::behavior::expression_type(value, env, defs, types)?;
                    expr(value, env, defs, types, cache, next, added)?;
                    if !env.contains_key(name) {
                        env.insert(name.clone(), ty.unwrap_or(value_type));
                    }
                }
                Statement::Set { target, value, .. } => {
                    expr(target, env, defs, types, cache, next, added)?;
                    expr(value, env, defs, types, cache, next, added)?;
                }
                Statement::If { condition, yes, no } => {
                    expr(condition, env, defs, types, cache, next, added)?;
                    body(yes, &mut env.clone(), defs, types, cache, next, added)?;
                    body(no, &mut env.clone(), defs, types, cache, next, added)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expr(condition, env, defs, types, cache, next, added)?;
                    body(inner, &mut env.clone(), defs, types, cache, next, added)?;
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body: inner,
                } => {
                    expr(start, env, defs, types, cache, next, added)?;
                    expr(end, env, defs, types, cache, next, added)?;
                    let mut innerenv = env.clone();
                    innerenv.insert(name.clone(), Type::I64);
                    body(inner, &mut innerenv, defs, types, cache, next, added)?;
                }
            }
        }
        Ok(())
    }
    let mut cursor = 0;
    while cursor < defs.len() {
        let mut d = defs[cursor].clone();
        let mut env = d
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.ty))
            .collect();
        let mut added = vec![];
        body(
            &mut d.body,
            &mut env,
            defs,
            types,
            &mut cache,
            &mut next,
            &mut added,
        )?;
        defs[cursor] = d;
        defs.extend(added);
        cursor += 1;
    }
    Ok(())
}

pub(crate) fn persistent_references(defs: &[CodeModule]) -> Result<BTreeMap<Id, Id>> {
    fn expr(e: &Expr, out: &mut BTreeMap<Id, Id>) -> Result<()> {
        match e {
            Expr::Call(n, args) => {
                if let Some(rest) = n.strip_prefix("#buffer.") {
                    let (cell, ty) = rest.split_once('.').ok_or("invalid buffer reference")?;
                    let cell = cell.parse().map_err(|_| "invalid buffer owner")?;
                    let ty = ty.parse().map_err(|_| "invalid buffer type")?;
                    if out.insert(cell, ty).is_some_and(|old| old != ty) {
                        return Err(
                            "one persistent cell cannot own incompatible buffer types".into()
                        );
                    }
                }
                for a in args {
                    expr(a, out)?;
                }
            }
            Expr::Unary(_, a) | Expr::Field(a, _) | Expr::View(_, a) => expr(a, out)?,
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                expr(a, out)?;
                expr(b, out)?;
            }
            Expr::Choose(c, a, b) => {
                expr(c, out)?;
                expr(a, out)?;
                expr(b, out)?;
            }
            Expr::Construct(_, args) => {
                for a in args {
                    expr(a, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn body(stmts: &[Statement], out: &mut BTreeMap<Id, Id>) -> Result<()> {
        for s in stmts {
            match s {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => expr(e, out)?,
                Statement::Set { target, value, .. } => {
                    expr(target, out)?;
                    expr(value, out)?;
                }
                Statement::If { condition, yes, no } => {
                    expr(condition, out)?;
                    body(yes, out)?;
                    body(no, out)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expr(condition, out)?;
                    body(inner, out)?;
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expr(start, out)?;
                    expr(end, out)?;
                    body(inner, out)?;
                }
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    for d in defs {
        body(&d.body, &mut out)?;
    }
    Ok(out)
}
