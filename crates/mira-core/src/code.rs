//! Portable Mira behavior syntax. Local operations are not persistent instances.
use crate::{Id, Type};
use alloc::{boxed::Box, string::String, vec::Vec};
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    FAdd,
    FSub,
    FMul,
    FDiv,
    FEq,
    FNe,
    FLt,
    FLe,
    FGt,
    FGe,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    FNeg,
    Neg,
    Not,
    Plus,
    BitNot,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// An incomplete expression, not a default runtime value or a binding.
    Hole(String),
    Int(i64),
    Float(u32),
    Bool(bool),
    Name(String),
    Unary(Unary, Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Choose(Box<Expr>, Box<Expr>, Box<Expr>),
    Unit,
    Construct(Id, Vec<Expr>),
    View(Id, Box<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assign {
    FAdd,
    FSub,
    FMul,
    FDiv,
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    Set {
        target: Expr,
        op: Assign,
        value: Expr,
    },
    Bind {
        name: String,
        ty: Option<Type>,
        immutable: bool,
        op: Assign,
        value: Expr,
    },
    Expr(Expr),
    If {
        condition: Expr,
        yes: Vec<Statement>,
        no: Vec<Statement>,
    },
    While {
        condition: Expr,
        body: Vec<Statement>,
    },
    For {
        name: String,
        start: Expr,
        end: Expr,
        body: Vec<Statement>,
    },
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub ty: Type,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CodeModule {
    pub data_types: Vec<crate::data::Definition>,
    pub id: Id,
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub output: Type,
    /// Explicit record projection onto named graph outputs; ordinary records
    /// retain their single `out` port. The aggregate return ABI is unchanged.
    #[serde(default)]
    pub output_ports: bool,
    /// Explicit compile-time program. Never a runtime cell behavior.
    #[serde(default)]
    pub compile_time: bool,
    pub annotated: bool,
    pub body: Vec<Statement>,
    pub locals: Vec<CodeModule>,
}

/// Find a definition by identity, independently of its depth in Z.
pub fn find(definitions: &[CodeModule], id: Id) -> Option<&CodeModule> {
    definitions.iter().find_map(|d| {
        if d.id == id {
            Some(d)
        } else {
            find(&d.locals, id)
        }
    })
}

/// Resolve a relative or qualified path using the same lexical definition tree.
/// A deeper declaration never leaks into an unrelated scope by its short name.
pub fn resolve<'a>(definitions: &'a [CodeModule], owner: Id, name: &str) -> Option<&'a CodeModule> {
    fn descend<'a>(defs: &'a [CodeModule], path: &str) -> Option<&'a CodeModule> {
        let (head, tail) = path
            .split_once('.')
            .map_or((path, None), |(a, b)| (a, Some(b)));
        let d = defs.iter().find(|d| d.name == head)?;
        match tail {
            Some(tail) => descend(&d.locals, tail),
            None => Some(d),
        }
    }
    fn within<'a>(defs: &'a [CodeModule], owner: Id, path: &str) -> Option<Option<&'a CodeModule>> {
        for d in defs {
            if d.id == owner {
                return Some(descend(&d.locals, path).or_else(|| descend(defs, path)));
            }
            if let Some(found) = within(&d.locals, owner, path) {
                return Some(found.or_else(|| descend(defs, path)));
            }
        }
        None
    }
    within(definitions, owner, name)
        .flatten()
        .or_else(|| descend(definitions, name))
}

/// Qualified names are projections of the definition tree, never stored aliases.
pub fn symbols(definitions: &[CodeModule]) -> Vec<(String, &CodeModule)> {
    fn visit<'a>(defs: &'a [CodeModule], prefix: &str, out: &mut Vec<(String, &'a CodeModule)>) {
        for d in defs {
            let name = if prefix.is_empty() {
                d.name.clone()
            } else {
                alloc::format!("{prefix}.{}", d.name)
            };
            visit(&d.locals, &name, out);
            out.push((name, d));
        }
    }
    let mut out = Vec::new();
    visit(definitions, "", &mut out);
    out
}

pub fn path(definitions: &[CodeModule], id: Id) -> Option<String> {
    symbols(definitions)
        .into_iter()
        .find_map(|(name, d)| (d.id == id).then_some(name))
}

pub fn find_mut(definitions: &mut [CodeModule], id: Id) -> Option<&mut CodeModule> {
    for definition in definitions {
        if definition.id == id {
            return Some(definition);
        }
        if let Some(found) = find_mut(&mut definition.locals, id) {
            return Some(found);
        }
    }
    None
}

/// Expression locations are local paths inside a definition, not global IDs or
/// runtime cells. The same paths serve reflection, draft diagnostics and edits.
#[derive(Clone, Debug)]
pub struct ExpressionRef<'a> {
    pub path: Vec<usize>,
    pub expression: &'a Expr,
}
pub fn children(expression: &Expr) -> Vec<&Expr> {
    match expression {
        Expr::Unary(_, e) | Expr::View(_, e) | Expr::Field(e, _) => alloc::vec![e],
        Expr::Binary(_, a, b) | Expr::Index(a, b) => alloc::vec![a, b],
        Expr::Choose(a, b, c) => alloc::vec![a, b, c],
        Expr::Call(_, args) | Expr::Construct(_, args) => args.iter().collect(),
        _ => Vec::new(),
    }
}
pub fn expression_kind(expression: &Expr) -> i64 {
    match expression {
        Expr::Hole(_) => 0,
        Expr::Int(_) => 1,
        Expr::Float(_) => 2,
        Expr::Bool(_) => 3,
        Expr::Unit => 4,
        Expr::Name(_) => 5,
        Expr::Unary(..) => 6,
        Expr::Binary(..) => 7,
        Expr::Call(..) => 8,
        Expr::Choose(..) => 9,
        Expr::Construct(..) => 10,
        Expr::View(..) => 11,
        Expr::Field(..) => 12,
        Expr::Index(..) => 13,
    }
}
pub fn expressions(module: &CodeModule) -> crate::Result<Vec<ExpressionRef<'_>>> {
    fn expr<'a>(
        e: &'a Expr,
        path: &mut Vec<usize>,
        out: &mut Vec<ExpressionRef<'a>>,
    ) -> crate::Result<()> {
        if out.len() >= crate::MAX_ELEMENTS || path.len() > crate::MAX_DEPTH * 4 {
            return Err("code inspection budget exceeded".into());
        }
        out.push(ExpressionRef {
            path: path.clone(),
            expression: e,
        });
        for (i, e) in children(e).into_iter().enumerate() {
            path.push(i);
            expr(e, path, out)?;
            path.pop();
        }
        Ok(())
    }
    fn body<'a>(
        stmts: &'a [Statement],
        path: &mut Vec<usize>,
        out: &mut Vec<ExpressionRef<'a>>,
    ) -> crate::Result<()> {
        for (i, s) in stmts.iter().enumerate() {
            path.push(i);
            match s {
                Statement::Expr(e) | Statement::Bind { value: e, .. } => {
                    path.push(0);
                    expr(e, path, out)?;
                    path.pop();
                }
                Statement::Set { target, value, .. } => {
                    for (j, e) in [target, value].iter().enumerate() {
                        path.push(j);
                        expr(e, path, out)?;
                        path.pop();
                    }
                }
                Statement::If { condition, yes, no } => {
                    path.push(0);
                    expr(condition, path, out)?;
                    path.pop();
                    path.push(1);
                    body(yes, path, out)?;
                    path.pop();
                    path.push(2);
                    body(no, path, out)?;
                    path.pop();
                }
                Statement::While {
                    condition,
                    body: stmts,
                } => {
                    path.push(0);
                    expr(condition, path, out)?;
                    path.pop();
                    path.push(1);
                    body(stmts, path, out)?;
                    path.pop();
                }
                Statement::For {
                    start,
                    end,
                    body: stmts,
                    ..
                } => {
                    for (j, e) in [start, end].iter().enumerate() {
                        path.push(j);
                        expr(e, path, out)?;
                        path.pop();
                    }
                    path.push(2);
                    body(stmts, path, out)?;
                    path.pop();
                }
            }
            path.pop();
        }
        Ok(())
    }
    let mut out = Vec::new();
    body(&module.body, &mut Vec::new(), &mut out)?;
    Ok(out)
}
/// Follow an inspected path in the same AST; no parsing or second code model.
pub fn expression_mut<'a>(
    module: &'a mut CodeModule,
    path: &[usize],
) -> crate::Result<&'a mut Expr> {
    fn expr<'a>(e: &'a mut Expr, path: &[usize]) -> Option<&'a mut Expr> {
        let Some((&index, tail)) = path.split_first() else {
            return Some(e);
        };
        let child = match e {
            Expr::Unary(_, e) | Expr::View(_, e) | Expr::Field(e, _) if index == 0 => e.as_mut(),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => match index {
                0 => a.as_mut(),
                1 => b.as_mut(),
                _ => return None,
            },
            Expr::Choose(a, b, c) => match index {
                0 => a.as_mut(),
                1 => b.as_mut(),
                2 => c.as_mut(),
                _ => return None,
            },
            Expr::Call(_, args) | Expr::Construct(_, args) => args.get_mut(index)?,
            _ => return None,
        };
        expr(child, tail)
    }
    fn body<'a>(stmts: &'a mut [Statement], path: &[usize]) -> Option<&'a mut Expr> {
        let (&index, path) = path.split_first()?;
        let (&slot, tail) = path.split_first()?;
        match stmts.get_mut(index)? {
            Statement::Expr(e) | Statement::Bind { value: e, .. } if slot == 0 => expr(e, tail),
            Statement::Set { target, value, .. } => expr(
                match slot {
                    0 => target,
                    1 => value,
                    _ => return None,
                },
                tail,
            ),
            Statement::If { condition, yes, no } => match slot {
                0 => expr(condition, tail),
                1 => body(yes, tail),
                2 => body(no, tail),
                _ => None,
            },
            Statement::While {
                condition,
                body: stmts,
            } => match slot {
                0 => expr(condition, tail),
                1 => body(stmts, tail),
                _ => None,
            },
            Statement::For {
                start,
                end,
                body: stmts,
                ..
            } => match slot {
                0 => expr(start, tail),
                1 => expr(end, tail),
                2 => body(stmts, tail),
                _ => None,
            },
            _ => None,
        }
    }
    if path.len() > crate::MAX_DEPTH * 4 {
        return Err("code path budget exceeded".into());
    }
    body(&mut module.body, path).ok_or_else(|| "code expression path missing".into())
}
