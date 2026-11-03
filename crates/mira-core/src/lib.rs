//! Mira's platform-independent structural model. No renderer or operating system.
#![no_std]
extern crate alloc;
pub mod code;
pub mod data;
pub mod engine;
pub mod information;
pub mod inspect;
pub mod intelligence;
pub mod learning;
pub mod neural;
pub mod system;
pub mod transform;
pub mod vm;
use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    format,
    string::{String, ToString},
    vec::Vec,
};
pub type Id = u64;
pub type Result<T> = core::result::Result<T, String>;
pub const MAX_DEPTH: usize = 32;
pub const MAX_ELEMENTS: usize = 20_000;
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Type {
    /// Unresolved source inference; never an executable data layout.
    Infer,
    I64,
    F32,
    Bool,
    Event,
    Unit,
    Addr,
    Data(Id),
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port<'a> {
    pub name: &'a str,
    pub ty: Type,
    pub direction: Direction,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}
impl Comparison {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Eq => "==",
            Self::Ne => "!=",
        }
    }
    pub fn test(self, a: i64, b: i64) -> bool {
        match self {
            Self::Lt => a < b,
            Self::Le => a <= b,
            Self::Gt => a > b,
            Self::Ge => a >= b,
            Self::Eq => a == b,
            Self::Ne => a != b,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Definition {
    /// An executable Z graph; its boundary is the cell interface.
    Graph,
    Const,
    /// Previous tick value; parameter is the initial state.
    Delay,
    /// Scalar neural operator; parameter is an explicit positive fixed-point scale.
    Neural,
    /// User-defined typed neural behavior with persistent named state/parameters.
    Adaptive(Id),
    ConstBool,
    Add,
    Mul,
    Branch(Comparison, Type),
    AsEvent(Type),
    Code(Id),
}
impl Definition {
    pub fn name(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Code(..) => "code",
            Self::Const => "const",
            Self::Delay => "delay",
            Self::Neural => "neural",
            Self::Adaptive(_) => "adaptive",
            Self::ConstBool => "bool",
            Self::Add => "add",
            Self::Mul => "mul",
            Self::Branch(c, ty) => match (c, ty) {
                (Comparison::Lt, _) => "branch_lt",
                (Comparison::Le, _) => "branch_le",
                (Comparison::Gt, _) => "branch_gt",
                (Comparison::Ge, _) => "branch_ge",
                (Comparison::Eq, Type::Bool) => "branch_eq_bool",
                (Comparison::Ne, Type::Bool) => "branch_ne_bool",
                (Comparison::Eq, _) => "branch_eq",
                (Comparison::Ne, _) => "branch_ne",
            },
            Self::AsEvent(Type::Bool) => "as_event_bool",
            Self::AsEvent(Type::Event) => "as_event_event",
            Self::AsEvent(_) => "as_event",
        }
    }
    pub fn input_type(self) -> Option<Type> {
        match self {
            Self::Graph | Self::Code(_) | Self::Adaptive(_) => None,
            Self::Const | Self::ConstBool => None,
            Self::Add | Self::Mul | Self::Delay | Self::Neural => Some(Type::I64),
            Self::Branch(_, t) | Self::AsEvent(t) => Some(t),
        }
    }
    pub fn ports(self) -> &'static [Port<'static>] {
        const fn p(name: &'static str, ty: Type, direction: Direction) -> Port<'static> {
            Port {
                name,
                ty,
                direction,
            }
        }
        use Direction::{In, Out};
        use Type::{Bool, Event, I64};
        match self {
            Self::Graph
            | Self::Branch(_, Type::Infer)
            | Self::AsEvent(Type::Infer)
            | Self::Code(_)
            | Self::Adaptive(_)
            | Self::AsEvent(Type::Unit)
            | Self::AsEvent(Type::F32)
            | Self::Branch(_, Type::Unit) => &[],
            Self::Const => const { &[p("out", I64, Out)] },
            Self::ConstBool => const { &[p("out", Bool, Out)] },
            Self::Add | Self::Mul | Self::Delay | Self::Neural => {
                const { &[p("in", I64, In), p("out", I64, Out)] }
            }
            Self::Branch(_, I64) => {
                const { &[p("in", I64, In), p("yes", I64, Out), p("no", I64, Out)] }
            }
            Self::Branch(_, Bool) => {
                const { &[p("in", Bool, In), p("yes", Bool, Out), p("no", Bool, Out)] }
            }
            Self::Branch(_, Event | Type::F32 | Type::Addr | Type::Data(_)) => &[],
            Self::AsEvent(I64) => const { &[p("in", I64, In), p("out", Event, Out)] },
            Self::AsEvent(Bool) => const { &[p("in", Bool, In), p("out", Event, Out)] },
            Self::AsEvent(Type::Data(_)) => &[],
            Self::AsEvent(Type::Addr) => const { &[p("in", Type::Addr, In), p("out", Event, Out)] },
            Self::AsEvent(Event) => const { &[p("in", Event, In), p("out", Event, Out)] },
        }
    }
    pub fn port(self, name: &str) -> Option<Port<'static>> {
        self.ports().iter().find(|p| p.name == name).copied()
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Instance {
    pub id: Id,
    pub name: String,
    pub definition: Definition,
    pub parameter: i64,
    pub position: [f32; 3],
    pub space: Option<Space>,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub cell: Id,
    pub port: String,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub id: Id,
    pub from: Endpoint,
    pub to: Endpoint,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Space {
    pub id: Id,
    pub name: String,
    pub position: [f32; 3],
    pub cells: Vec<Instance>,
    pub links: Vec<Link>,
    pub inputs: Vec<code::Parameter>,
    pub outputs: Vec<code::Parameter>,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Program {
    #[serde(default)]
    pub global_types: Vec<data::Definition>,
    pub root: Space,
    pub code_modules: Vec<code::CodeModule>,
    /// Persisted high-water mark: deleting an element never makes its ID reusable.
    pub next_identity: Id,
}
impl Space {
    pub fn new(id: Id, name: String) -> Self {
        Self {
            id,
            name,
            position: [0.; 3],
            cells: Vec::new(),
            links: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }
    pub fn find_cell(&self, id: Id) -> Option<&Instance> {
        self.cells.iter().find(|c| c.id == id).or_else(|| {
            self.cells
                .iter()
                .filter_map(|c| c.space.as_ref())
                .find_map(|s| s.find_cell(id))
        })
    }
    pub fn cell(&self, id: Id) -> Result<&Instance> {
        self.cells
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| format!("{}/#{id}: cell outside this space or absent", self.name))
    }
    /// An explicit path stays inside this owned Z subtree; short names never
    /// search unrelated descendants or silently pick one of several matches.
    pub fn resolve_cell(&self, path: &str) -> Option<&Instance> {
        let (head, tail) = path.split_once('.').map_or((path, None), |(a,b)| (a, Some(b)));
        let cell = self.cells.iter().find(|cell| cell.name == head)?;
        match tail {
            None => Some(cell),
            Some(tail) => cell.space.as_ref()?.resolve_cell(tail),
        }
    }
    pub fn find(&self, id: Id) -> Option<&Space> {
        if self.id == id {
            Some(self)
        } else {
            self.cells
                .iter()
                .filter_map(|c| c.space.as_ref())
                .find_map(|s| s.find(id))
        }
    }
    pub fn find_mut(&mut self, id: Id) -> Option<&mut Space> {
        if self.id == id {
            Some(self)
        } else {
            self.cells
                .iter_mut()
                .filter_map(|c| c.space.as_mut())
                .find_map(|s| s.find_mut(id))
        }
    }
    /// Stable topological order. Missing inputs are allowed while editing; execution checks them.
    pub fn order(&self) -> Result<Vec<Id>> {
        let mut incoming: BTreeMap<Id, usize> = self.cells.iter().map(|c| (c.id, 0)).collect();
        let mut outgoing: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
        let delayed: BTreeSet<_> = self
            .cells
            .iter()
            .filter(|c| c.definition == Definition::Delay)
            .map(|c| c.id)
            .collect();
        for l in &self.links {
            if l.to.cell == self.id || l.from.cell == self.id || delayed.contains(&l.to.cell) {
                continue;
            }
            *incoming.get_mut(&l.to.cell).ok_or("link target absent")? += 1;
            outgoing.entry(l.from.cell).or_default().push(l.to.cell);
        }
        let mut ready: VecDeque<Id> = incoming
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order = Vec::new();
        while let Some(id) = ready.pop_front() {
            order.push(id);
            for to in outgoing.get(&id).into_iter().flatten() {
                let n = incoming.get_mut(to).ok_or("link target absent")?;
                *n -= 1;
                if *n == 0 {
                    ready.push_back(*to);
                }
            }
        }
        if order.len() != self.cells.len() {
            return Err(format!(
                "{}: instantaneous cycle; every feedback path must cross a delay cell",
                self.name
            ));
        }
        Ok(order)
    }
}
pub fn name_ok(s: &str) -> bool {
    !matches!(s, "space" | "at")
        && s.split('-').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Resolve a stored type without cloning it: port names borrow the same source
/// definitions as signatures, including module-local record declarations.
pub fn stored_type<'a>(
    definitions: &'a [code::CodeModule],
    globals: &'a [data::Definition],
    id: Id,
) -> Option<&'a data::Definition> {
    globals.iter().find(|t| t.id == id).or_else(|| {
        definitions.iter().find_map(|m| {
            m.data_types
                .iter()
                .find(|t| t.id == id)
                .or_else(|| stored_type(&m.locals, &[], id))
        })
    })
}
pub fn port_result_fields<'a>(
    definitions: &'a [code::CodeModule],
    globals: &'a [data::Definition],
    module: &code::CodeModule,
) -> Result<Option<&'a [code::Parameter]>> {
    if !module.output_ports {
        return Ok(None);
    }
    let Type::Data(id) = module.output else {
        return Err("named output ports require a record result".into());
    };
    match &stored_type(definitions, globals, id)
        .ok_or("port result type missing")?
        .kind
    {
        data::Kind::Record(fields) if !fields.is_empty() => Ok(Some(fields)),
        _ => Err("named output ports require a nonempty record result".into()),
    }
}
pub fn ports_for<'a>(
    definitions: &'a [code::CodeModule],
    global_types: &'a [data::Definition],
    cell: &'a Instance,
) -> Vec<Port<'a>> {
    if cell.definition == Definition::Graph {
        return cell
            .space
            .as_ref()
            .map(|s| {
                s.inputs
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
                    .collect()
            })
            .unwrap_or_default();
    }
    if let Definition::Code(id) | Definition::Adaptive(id) = cell.definition {
        if let Some(d) = code::find(definitions, id) {
            let mut ports: Vec<_> = d
                .parameters
                .iter()
                .filter(|p| {
                    !matches!(cell.definition, Definition::Adaptive(_))
                        || !(p.name.starts_with("state_") || p.name.starts_with("param_"))
                })
                .map(|p| Port {
                    name: p.name.as_str(),
                    ty: p.ty,
                    direction: Direction::In,
                })
                .collect();
            if d.output_ports {
                if let Ok(Some(fields)) = port_result_fields(definitions, global_types, d) {
                    ports.extend(fields.iter().map(|field| Port {
                        name: &field.name,
                        ty: field.ty,
                        direction: Direction::Out,
                    }));
                }
            } else if d.output != Type::Unit {
                ports.push(Port {
                    name: "out",
                    ty: d.output,
                    direction: Direction::Out,
                });
            }
            return ports;
        }
        Vec::new()
    } else if let Definition::AsEvent(ty) = cell.definition {
        if ty == Type::Unit {
            Vec::new()
        } else {
            alloc::vec![
                Port {
                    name: "in",
                    ty,
                    direction: Direction::In
                },
                Port {
                    name: "out",
                    ty: Type::Event,
                    direction: Direction::Out
                }
            ]
        }
    } else {
        cell.definition.ports().to_vec()
    }
}
impl Program {
    /// Reject incomplete source before any runtime side effect or publication.
    pub fn require_complete(&self) -> Result<()> {
        fn local_types(body: &[code::Statement]) -> bool {
            body.iter().any(|s| match s {
                code::Statement::Bind { ty: Some(Type::Infer), .. } => true,
                code::Statement::If { yes, no, .. } => local_types(yes) || local_types(no),
                code::Statement::While { body, .. } | code::Statement::For { body, .. } => local_types(body),
                _ => false,
            })
        }
        for (_, d) in code::symbols(&self.code_modules) {
            if d.output == Type::Infer || d.parameters.iter().any(|p| p.ty == Type::Infer)
                || local_types(&d.body) {
                return Err(format!("{}: unresolved type", d.name));
            }
            if code::expressions(d)?.iter().any(|n| matches!(n.expression, code::Expr::Hole(_))) {
                return Err(format!("{}: unfilled code hole", d.name));
            }
        }
        fn graph(s: &Space) -> bool {
            s.inputs.iter().chain(&s.outputs).any(|p| p.ty == Type::Infer)
                || s.cells.iter().any(|c| {
                    matches!(c.definition, Definition::AsEvent(Type::Infer) | Definition::Branch(_, Type::Infer))
                        || c.space.as_ref().is_some_and(graph)
                })
        }
        if graph(&self.root) || self.data_types().iter().any(|d| d.member_types().contains(&Type::Infer)) {
            return Err("unresolved data/port type".into());
        }
        Ok(())
    }
    pub fn parent_cell(&self, cell: Id) -> Option<&Instance> {
        fn parent(space: &Space, cell: Id, owner: Option<Id>) -> Option<Id> {
            if space.cells.iter().any(|c| c.id == cell) { return owner; }
            space.cells.iter().find_map(|c| c.space.as_ref().and_then(|s| parent(s, cell, Some(c.id))))
        }
        self.root.find_cell(parent(&self.root, cell, None)?)
    }

    /// Explicit parent steps follow instance ownership, not global name search.
    pub fn relative_scope<'a>(&self, mut owner: Id, mut path: &'a str) -> Option<(&Space, &'a str)> {
        while let Some(tail) = path.strip_prefix("parent.") {
            owner = self.parent_cell(owner)?.id;
            path = tail;
        }
        Some((self.root.find_cell(owner)?.space.as_ref()?, path))
    }
    pub fn component_target(&self, owner: Id, path: &str) -> Option<(&Instance, Option<&code::CodeModule>)> {
        let (space, path) = self.relative_scope(owner, path)?;
        let (head, mut tail) = path.split_once('.').map_or((path, ""), |(a,b)| (a,b));
        let mut cell = space.cells.iter().find(|c| c.name == head)?;
        while !tail.is_empty() {
            let (head, rest) = tail.split_once('.').map_or((tail, ""), |(a,b)| (a,b));
            if let Some(child) = cell.space.as_ref().and_then(|s| s.cells.iter().find(|c| c.name == head)) {
                cell = child; tail = rest;
            } else if let Definition::Code(id) = cell.definition {
                let definition = code::find(&self.code_modules, id)?;
                return Some((cell, Some(code::resolve(&definition.locals, 0, tail)?)));
            } else { return None; }
        }
        Some((cell, None))
    }
    /// Private compiled targets already resolved an explicit path. Admission
    /// stays within the same ancestor-owned subtree as repeated parent steps.
    pub fn visible_cell(&self, mut owner: Id, target: Id) -> Option<&Instance> {
        loop {
            if let Some(cell) = self.root.find_cell(owner)?.space.as_ref().and_then(|s| s.find_cell(target)) { return Some(cell); }
            owner = self.parent_cell(owner)?.id;
        }
    }
    pub fn data_types(&self) -> Vec<data::Definition> {
        fn collect(modules: &[code::CodeModule], out: &mut Vec<data::Definition>) {
            for m in modules {
                out.extend(m.data_types.clone());
                collect(&m.locals, out);
            }
        }
        let mut types = self.global_types.clone();
        collect(&self.code_modules, &mut types);
        types
    }
    pub fn ports<'a>(&'a self, cell: &'a Instance) -> Vec<Port<'a>> {
        ports_for(&self.code_modules, &self.global_types, cell)
    }
    pub fn validate(&self) -> Result<()> {
        fn identity(ids: &mut BTreeSet<Id>, id: Id) -> Result<()> {
            if id == 0 || !ids.insert(id) {
                return Err(format!("#{id}: zero or duplicate identity"));
            }
            if ids.len() > MAX_ELEMENTS {
                return Err("element budget exceeded".to_string());
            }
            Ok(())
        }
        fn walk(
            s: &Space,
            ids: &mut BTreeSet<Id>,
            depth: usize,
            path: &str,
            definitions: &[code::CodeModule],
            global_types: &[data::Definition],
            owned: bool,
        ) -> Result<()> {
            if depth > MAX_DEPTH {
                return Err("space depth budget exceeded".to_string());
            }
            identity(ids, s.id)?;
            if !name_ok(&s.name) || !s.position.iter().all(|x| x.is_finite()) {
                return Err(format!("{path}: invalid name or position"));
            }
            let mut names = BTreeSet::new();
            for port in s.inputs.iter().chain(&s.outputs) {
                if !name_ok(&port.name) || !names.insert(port.name.as_str()) {
                    return Err(format!(
                        "{path}/{}: invalid or duplicate interface port",
                        port.name
                    ));
                }
            }
            for c in &s.cells {
                identity(ids, c.id)?;
                if c.definition == Definition::Neural && c.parameter <= 0 {
                    return Err("neural fixed-point scale must be positive".into());
                }
                if matches!(
                    c.definition,
                    Definition::ConstBool | Definition::Branch(_, Type::Bool)
                ) && !matches!(c.parameter, 0 | 1)
                {
                    return Err(format!("{path}/{}: invalid bool value", c.name));
                }
                if matches!(
                    c.definition,
                    Definition::Branch(_, Type::Event)
                        | Definition::Branch(
                            Comparison::Lt | Comparison::Le | Comparison::Gt | Comparison::Ge,
                            Type::Bool
                        )
                ) {
                    return Err(format!(
                        "{path}/{}: comparison is not defined for this type",
                        c.name
                    ));
                }

                if !name_ok(&c.name) || !names.insert(c.name.as_str()) {
                    return Err(format!("{path}/{}: invalid or duplicate name", c.name));
                }
                if !c.position.iter().all(|x| x.is_finite()) {
                    return Err(format!("{path}/{}: nonfinite position", c.name));
                }
            }
            let mut occupied = BTreeSet::new();
            let cells: BTreeMap<_, _> = s.cells.iter().map(|c| (c.id, c)).collect();
            for l in &s.links {
                identity(ids, l.id)?;
                let resolve = |e: &Endpoint, source: bool| -> Result<(Type, Direction)> {
                    if e.cell == s.id {
                        let parameters = if source { &s.inputs } else { &s.outputs };
                        let port = parameters
                            .iter()
                            .find(|p| p.name == e.port)
                            .ok_or_else(|| format!("{path}/{}: unknown boundary port", e.port))?;
                        Ok((
                            port.ty,
                            if source {
                                Direction::Out
                            } else {
                                Direction::In
                            },
                        ))
                    } else {
                        let c = *cells.get(&e.cell).ok_or("link cell missing")?;
                        let port = ports_for(definitions, global_types, c)
                            .into_iter()
                            .find(|p| p.name == e.port)
                            .ok_or_else(|| format!("{path}/{}.{}: unknown port", c.name, e.port))?;
                        Ok((port.ty, port.direction))
                    }
                };
                let (at, ad) = resolve(&l.from, true)?;
                let (bt, bd) = resolve(&l.to, false)?;
                if ad != Direction::Out || bd != Direction::In {
                    return Err(format!("{path}/link#{}: expected output -> input", l.id));
                }
                if at != bt {
                    return Err(format!("{path}/link#{}: incompatible port types", l.id));
                }
                if !occupied.insert((l.to.cell, l.to.port.as_str()))
                    && !(owned && l.to.cell == s.id)
                {
                    return Err(format!(
                        "{path}/{}.{}: input has multiple producers",
                        l.to.cell, l.to.port
                    ));
                }
            }
            for c in &s.cells {
                if let Some(inner) = &c.space {
                    walk(
                        inner,
                        ids,
                        depth + 1,
                        &format!("{path}/{}/{}", c.name, inner.name),
                        definitions,
                        global_types,
                        true,
                    )?;
                }
            }
            s.order()?;

            Ok(())
        }
        let mut ids = BTreeSet::new();
        fn definitions(
            modules: &[code::CodeModule],
            ids: &mut BTreeSet<Id>,
            depth: usize,
        ) -> Result<()> {
            if depth > MAX_DEPTH {
                return Err("code module nesting budget exceeded".into());
            }
            let mut names = BTreeSet::new();
            for module in modules {
                identity(ids, module.id)?;
                for ty in &module.data_types {
                    identity(ids, ty.id)?;
                }

                if !name_ok(&module.name) || !names.insert(module.name.as_str()) {
                    return Err(format!(
                        "{}: invalid or duplicate code definition",
                        module.name
                    ));
                }
                definitions(&module.locals, ids, depth + 1)?;
            }
            Ok(())
        }
        for ty in &self.global_types {
            identity(&mut ids, ty.id)?;
        }
        definitions(&self.code_modules, &mut ids, 0)?;
        let types = self.data_types();
        fn check_type(ty: Type, types: &[data::Definition]) -> Result<()> {
            if let Type::Data(id) = ty {
                data::find(types, id)?;
            }
            Ok(())
        }
        fn signatures(modules: &[code::CodeModule], types: &[data::Definition]) -> Result<()> {
            for m in modules {
                check_type(m.output, types)?;
                if m.output_ports {
                    let Type::Data(id) = m.output else {
                        return Err("named output ports require a record result".into());
                    };
                    let data::Kind::Record(fields) = &data::find(types, id)?.kind else {
                        return Err("named output ports require a record result".into());
                    };
                    if fields.is_empty()
                        || fields.iter().any(|field| {
                            field.ty == Type::Unit
                                || m.parameters.iter().any(|p| p.name == field.name)
                        })
                    {
                        return Err("named output ports must be nonempty, non-unit and distinct from input names".into());
                    }
                }
                for p in &m.parameters {
                    check_type(p.ty, types)?;
                }
                if types.iter().any(|t| t.name == m.name) {
                    return Err("data type and code definition names must differ".into());
                }
                signatures(&m.locals, types)?;
            }
            Ok(())
        }
        signatures(&self.code_modules, &types)?;
        fn cell_types(s: &Space, program: &Program, types: &[data::Definition]) -> Result<()> {
            for c in &s.cells {
                if let Definition::Adaptive(id) = c.definition {
                    if crate::code::find(&program.code_modules, id)
                        .is_some_and(|m| m.output_ports)
                    {
                        return Err("adaptive slot behaviors use one result; use a code cell for named output ports".into());
                    }
                }
                for p in program.ports(c) {
                    check_type(p.ty, types)?;
                }
                if let Some(inner) = &c.space {
                    cell_types(inner, program, types)?;
                }
            }

            Ok(())
        }
        cell_types(&self.root, self, &types)?;
        let mut names = BTreeSet::new();
        for ty in &types {
            if !name_ok(&ty.name)
                || matches!(
                    ty.name.as_str(),
                    "i64"
                        | "bool"
                        | "event"
                        | "unit"
                        | "addr"
                        | "sys"
                        | "cells"
                        | "true"
                        | "false"
                        | "struct"
                        | "type"
                        | "if"
                        | "else"
                        | "for"
                        | "while"
                        | "const"
                )
                || !names.insert(&ty.name)
            {
                return Err(format!("{}: invalid or duplicate data type", ty.name));
            }
            if matches!(
                ty.kind,
                data::Kind::Array {
                    element: Type::Unit,
                    ..
                }
            ) {
                return Err("unit cannot be an array element".into());
            }
            if let data::Kind::Tensor { element, shape } = &ty.kind {
                if shape.is_empty()
                    || shape.len() > 8
                    || shape.contains(&0)
                    || !matches!(element, Type::F32 | Type::I64 | Type::Bool)
                {
                    return Err("invalid tensor layout".into());
                }
            }
            let limit = if matches!(ty.kind, data::Kind::Record(_)) {
                4096
            } else {
                data::MAX_WORDS
            };
            if ty.len() > limit {
                return Err(format!("data layout exceeds {limit} words"));
            }
            let mut fields = BTreeSet::new();
            if let data::Kind::Record(items) = &ty.kind {
                for field in items {
                    if !name_ok(&field.name) || !fields.insert(&field.name) {
                        return Err("invalid or duplicate record field".into());
                    }
                }
            }
            for member in ty.member_types() {
                if member == Type::Unit {
                    return Err("unit cannot be a data member".into());
                }
                if let Type::Data(id) = member {
                    data::find(&types, id)?;
                }
            }
        }

        fn references(s: &Space, definitions: &[code::CodeModule]) -> Result<()> {
            for cell in &s.cells {
                if cell.definition == Definition::Graph && cell.space.is_none() {
                    return Err(format!("{}: graph cell needs a Z body", cell.name));
                }
                if let Definition::Code(id) | Definition::Adaptive(id) = cell.definition {
                    let definition = code::find(definitions, id)
                        .ok_or_else(|| format!("{}: missing behavior #{id}", cell.name))?;
                    if definition.compile_time {
                        return Err(format!(
                            "{}: compile-time macro {} cannot be a runtime cell",
                            cell.name, definition.name
                        ));
                    }
                }
            }
            for cell in &s.cells {
                if let Some(inner) = &cell.space {
                    references(inner, definitions)?;
                }
            }

            Ok(())
        }
        references(&self.root, &self.code_modules)?;
        walk(
            &self.root,
            &mut ids,
            0,
            &self.root.name,
            &self.code_modules,
            &self.global_types,
            false,
        )?;
        if self.next_identity <= *ids.last().ok_or("empty identity set")? {
            return Err("identity counter must exceed every live ID".to_string());
        }
        Ok(())
    }
    pub fn next_id(&self) -> Result<Id> {
        fn max(s: &Space) -> Id {
            s.cells
                .iter()
                .map(|c| c.id)
                .chain(s.links.iter().map(|l| l.id))
                .chain(s.cells.iter().filter_map(|c| c.space.as_ref()).map(max))
                .fold(s.id, core::cmp::max)
        }
        Ok(core::cmp::max(
            self.next_identity,
            max(&self.root)
                .max({
                    fn max_code(m: &code::CodeModule) -> Id {
                        m.locals
                            .iter()
                            .map(max_code)
                            .chain(m.data_types.iter().map(|t| t.id))
                            .fold(m.id, core::cmp::max)
                    }
                    self.code_modules
                        .iter()
                        .map(max_code)
                        .chain(self.global_types.iter().map(|t| t.id))
                        .max()
                        .unwrap_or(0)
                })
                .checked_add(1)
                .ok_or_else(|| "identity budget exhausted".to_string())?,
        ))
    }
    pub fn allocate_id(&mut self) -> Result<Id> {
        let id = self.next_id()?;
        self.next_identity = id.checked_add(1).ok_or("identity exhausted")?;
        Ok(id)
    }
    pub fn owner_of(&self, space: Id) -> Option<Id> {
        fn walk(s: &Space, id: Id) -> Option<Id> {
            for c in &s.cells {
                if let Some(inner) = &c.space {
                    if inner.id == id {
                        return Some(c.id);
                    }
                    if let Some(owner) = walk(inner, id) {
                        return Some(owner);
                    }
                }
            }
            None
        }
        walk(&self.root, space)
    }
    pub fn cell_mut(&mut self, id: Id) -> Option<&mut Instance> {
        fn walk(s: &mut Space, id: Id) -> Option<&mut Instance> {
            for c in &mut s.cells {
                if c.id == id {
                    return Some(c);
                }
                if let Some(inner) = &mut c.space {
                    if let Some(c) = walk(inner, id) {
                        return Some(c);
                    }
                }
            }
            None
        }
        walk(&mut self.root, id)
    }
    pub fn ensure_space(&mut self, id: Id) -> Result<Id> {
        let cell = self.root.find_cell(id).ok_or("cell absent")?.clone();
        if let Some(inner) = cell.space {
            return Ok(inner.id);
        }
        let inputs: Vec<_> = self
            .ports(&cell)
            .iter()
            .filter(|p| p.direction == Direction::In)
            .map(|p| code::Parameter {
                name: p.name.into(),
                ty: p.ty,
            })
            .collect();
        let outputs: Vec<_> = self
            .ports(&cell)
            .iter()
            .filter(|p| p.direction == Direction::Out)
            .map(|p| code::Parameter {
                name: p.name.into(),
                ty: p.ty,
            })
            .collect();
        let sid = self.allocate_id()?;
        let mut inner = Space::new(sid, "inner".into());
        inner.inputs = inputs;
        inner.outputs = outputs;
        self.cell_mut(id).ok_or("cell absent")?.space = Some(inner);
        Ok(sid)
    }
    /// Edits are all-or-nothing; callers retain the returned old model for undo.
    pub fn edit(&mut self, f: impl FnOnce(&mut Program) -> Result<()>) -> Result<Program> {
        let mut draft = self.clone();
        f(&mut draft)?;
        draft.next_identity = core::cmp::max(self.next_identity, draft.next_id()?);
        draft.validate()?;
        Ok(core::mem::replace(self, draft))
    }
}
