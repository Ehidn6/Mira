//! Bind code to its owning cell. Private procedures are compiler details, not cells.
use mira_core::{code::*, *};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Executable {
    pub bound_modules: BTreeMap<(Option<Id>, Id), Id>,
    pub host_entries: BTreeSet<Id>,
    pub definitions: Vec<CodeModule>,
    pub entries: BTreeMap<Id, Id>,
    pub graph_entries: BTreeMap<Id, (Id, String)>,
    pub graph_slots: BTreeMap<Id, Vec<(Id, String)>>,
    pub data_types: Vec<data::Definition>,
}
#[derive(Default)]
pub(crate) struct References {
    pub calls: BTreeMap<Id, BTreeSet<Id>>,
    pub modules: BTreeMap<Id, BTreeSet<Id>>,
    pub graphs: BTreeMap<Id, BTreeSet<String>>,
}
struct Builder<'a> {
    references: References,
    program: &'a Program,
    source_revision: Option<&'a blake3::Hash>,
    next: Id,
    modules: BTreeMap<(Option<Id>, Id), Id>,
    entries: BTreeMap<Id, Id>,
    definitions: Vec<CodeModule>,
    origins: BTreeMap<Id, Id>,
    external: BTreeMap<String, (Vec<Type>, Type)>,
    wrappers: BTreeSet<Id>,
    graphs: BTreeMap<(Id, String), Id>,
    binding_depth: usize,
    allocated: usize,
    reflection: BTreeMap<String, Id>,
    reflection_rows: usize,
    derivatives: BTreeMap<Id, Id>,
}
fn source_error(mut error: String, origins: &BTreeMap<Id, Id>, program: &Program) -> String {
    fn find(defs: &[CodeModule], id: Id) -> Option<&str> {
        defs.iter().find_map(|d| {
            if d.id == id {
                Some(d.name.as_str())
            } else {
                find(&d.locals, id)
            }
        })
    }
    // Match whole labels: replacing entry_1 must not corrupt entry_10.
    let mut entries = origins.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(id, _)| std::cmp::Reverse(id.to_string().len()));
    for (generated, original) in entries {
        if let Some(name) = find(&program.code_modules, *original) {
            error = error.replace(&label(*generated), name);
        }
    }
    error
}
fn label(id: Id) -> String {
    format!("__entry_{id}")
}
fn each_expr(
    body: &mut [Statement],
    f: &mut impl FnMut(&mut Expr, &BTreeSet<String>) -> Result<()>,
    vars: &mut BTreeSet<String>,
) -> Result<()> {
    fn expr(
        e: &mut Expr,
        vars: &BTreeSet<String>,
        f: &mut impl FnMut(&mut Expr, &BTreeSet<String>) -> Result<()>,
    ) -> Result<()> {
        match e {
            Expr::Construct(_, args) | Expr::Call(_, args) => {
                for a in args {
                    expr(a, vars, f)?;
                }
            }
            Expr::View(_, e) | Expr::Field(e, _) | Expr::Unary(_, e) => expr(e, vars, f)?,
            Expr::Index(a, b) => {
                expr(a, vars, f)?;
                expr(b, vars, f)?;
            }
            Expr::Binary(_, a, b) => {
                expr(a, vars, f)?;
                expr(b, vars, f)?;
            }
            Expr::Choose(c, a, b) => {
                expr(c, vars, f)?;
                expr(a, vars, f)?;
                expr(b, vars, f)?;
            }
            _ => {}
        }
        f(e, vars)
    }
    for s in body {
        match s {
            Statement::Set { target, value, .. } => {
                expr(target, vars, f)?;
                expr(value, vars, f)?;
            }
            Statement::Expr(e) => expr(e, vars, f)?,
            Statement::Bind { name, value, .. } => {
                expr(value, vars, f)?;
                vars.insert(name.clone());
            }
            Statement::If { condition, yes, no } => {
                expr(condition, vars, f)?;
                each_expr(yes, f, &mut vars.clone())?;
                each_expr(no, f, &mut vars.clone())?;
            }
            Statement::While { condition, body } => {
                expr(condition, vars, f)?;
                each_expr(body, f, &mut vars.clone())?;
            }
            Statement::For {
                name,
                start,
                end,
                body,
            } => {
                expr(start, vars, f)?;
                expr(end, vars, f)?;
                let mut inner = vars.clone();
                inner.insert(name.clone());
                each_expr(body, f, &mut inner)?;
            }
        }
    }
    Ok(())
}
fn uses_children(d: &CodeModule) -> bool {
    let mut found = false;
    let mut body = d.body.clone();
    let _ = each_expr(
        &mut body,
        &mut |e, _| {
            if matches!(e, Expr::Call(n, _) if (n.starts_with("cells.") || n.starts_with("graph.") || n == "reflect.self" || n.ends_with(".persist")))
            {
                found = true;
            }
            Ok(())
        },
        &mut BTreeSet::new(),
    );
    found
}
impl Builder<'_> {
    /// A component is a normal code cell. Its operations are existing local
    /// definitions; invoking one binds it to that component's state and Z body.
    fn cell_target(&self, owner: Id, path: &str) -> Result<(Id, Option<Id>)> {
        let (cell, method) = self.program.component_target(owner, path).ok_or_else(|| format!("{path}: component or operation not found"))?;
        Ok((cell.id, method.map(|m| m.id)))
    }

    fn operation(&mut self, caller: Id, path: &str) -> Result<Id> {
        let (cell, method) = self.cell_target(caller, path)?;
        self.references.calls.entry(caller).or_default().insert(cell);
        if let Some(method) = method { self.module(Some(cell), method) } else { self.entry(cell) }
    }
    fn allocate(&mut self) -> Result<Id> {
        self.allocated += 1;
        if self.allocated > MAX_ELEMENTS * 4 {
            return Err("cell call compilation budget exceeded".into());
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("private procedure identity exhausted")?;
        if self.definitions.len() + self.modules.len() + self.entries.len() > MAX_ELEMENTS * 4 {
            return Err("cell call compilation budget exceeded".into());
        }
        Ok(id)
    }
    fn module(&mut self, owner: Option<Id>, original: Id) -> Result<Id> {
        if let Some(owner) = owner {
            self.references
                .modules
                .entry(owner)
                .or_default()
                .insert(original);
        }
        if let Some(id) = self.modules.get(&(owner, original)) {
            return Ok(*id);
        }
        self.binding_depth += 1;
        if self.binding_depth > 256 {
            return Err("cell call binding depth exceeded".into());
        }
        let mut d = mira_core::code::find(&self.program.code_modules, original)
            .ok_or("code module missing")?
            .clone();
        if d.compile_time {
            return Err(format!("{}: macro cannot be called during runtime", d.name));
        }
        let id = self.allocate()?;
        self.modules.insert((owner, original), id);
        self.origins.insert(id, original);
        d.id = id;
        d.name = label(id);
        // Bind only called definitions to this instance. Unused local persistent
        // accessors must not acquire the enclosing component's storage identity.
        d.locals.clear();
        self.rewrite(&mut d, owner)?;
        self.definitions.push(d);
        self.binding_depth -= 1;
        Ok(id)
    }
    fn rewrite(
        &mut self,
        d: &mut CodeModule,
        owner: Option<Id>,
    ) -> Result<()> {
        let original = self.origins.get(&d.id).copied().unwrap_or(d.id);
        let mut vars = d.parameters.iter().map(|p| p.name.clone()).collect();
        each_expr(
            &mut d.body,
            &mut |e, vars| {
                if let Expr::Call(name, args) = e {
                    if name.starts_with("meta.") {
                        return Err(format!(
                            "{name}: structural operations are only available inside a macro expansion"
                        ));
                    }
                    if name == "diff.derivative" {
                        let [Expr::Name(target), _] = args.as_slice() else {
                            return Err(
                                "diff.derivative expects (unary_f32_behavior, f32_value)".into()
                            );
                        };
                        if vars.contains(target) {
                            return Err(
                                "diff.derivative requires a behavior symbol, not a local value"
                                    .into(),
                            );
                        }
                        let target = mira_core::code::resolve(&self.program.code_modules, original, target)
                            .ok_or_else(|| format!("diff.derivative: behavior {target} not found"))?
                            .id;
                        let id = if let Some(id) = self.derivatives.get(&target) {
                            *id
                        } else {
                            let id = self.allocate()?;
                            let mut helper = crate::differentiation::differentiate(
                                self.program,
                                target,
                                id,
                                label(id),
                            )?;
                            self.external
                                .insert(label(id), (vec![Type::F32], Type::F32));
                            self.rewrite(&mut helper, None)?;
                            self.definitions.push(helper);
                            self.derivatives.insert(target, id);
                            id
                        };
                        args.remove(0);
                        *name = label(id);
                    } else if name.starts_with("diff.") {
                        return Err(format!("unknown differentiation operation: {name}"));
                    } else if name.starts_with("reflect.") {
                        let arity = crate::reflection::QUERIES
                            .iter()
                            .find(|(n, _)| *n == name)
                            .ok_or_else(|| format!("unknown graph query: {name}"))?
                            .1;
                        if args.len() != arity {
                            return Err(format!("{name}: expected {arity} arguments"));
                        }
                        if let Some(value) = crate::reflection::constant(name, self.program, owner)?
                        {
                            *e = Expr::Int(value);
                        } else {
                            let id = if let Some(id) = self.reflection.get(name) {
                                *id
                            } else {
                                let id = self.allocate()?;
                                let (helper, rows) = crate::reflection::lower(
                                    name,
                                    self.program,
                                    self.source_revision,
                                    id,
                                    label(id),
                                )?;
                                self.reflection_rows = self
                                    .reflection_rows
                                    .checked_add(rows)
                                    .ok_or("reflection row overflow")?;
                                if self.reflection_rows > crate::reflection::MAX_ROWS * 4 {
                                    return Err("program reflection budget exceeded".into());
                                }
                                self.external
                                    .insert(label(id), (vec![Type::I64; arity], Type::I64));
                                self.definitions.push(helper);
                                self.reflection.insert(name.clone(), id);
                                id
                            };
                            *name = label(id);
                        }
                    } else if let Some(base) = name.strip_suffix(".persist") {
                        let owner = owner.ok_or("persistent buffer requires a cell instance")?;
                        let ty = self
                            .program
                            .data_types()
                            .iter()
                            .find(|t| t.name == base)
                            .map(|t| t.id)
                            .ok_or("persistent buffer type missing")?;
                        if !args.is_empty() {
                            return Err("persistent buffer accessor takes no arguments".into());
                        }
                        *name = format!("#buffer.{owner}.{ty}");
                    } else if crate::intrinsics::symbol_reference(name)
                        || matches!(
                            name.as_str(),
                            "sys.neural_try" | "sys.scratch" | "sys.read_only"
                        )
                    {
                        let Some(Expr::Name(target)) = args.first_mut() else {
                            return Err("handler expects one behavior symbol".into());
                        };
                        if !vars.contains(target) {
                            if let Some(path) = target.strip_prefix("cells.") {
                                *target = label(self.operation(owner.ok_or("component operation needs an instance")?, path)?);
                                return Ok(());
                            }
                            if let Some(definition) =
                                mira_core::code::resolve(&self.program.code_modules, original, target)
                            {
                                if definition.compile_time {
                                    return Err("compile-time definition cannot have a runtime entry address".into());
                                }
                                *target = label(self.module(owner, definition.id)?);
                            }
                        }
                    } else if let Some(child) = name.strip_prefix("cells.") {
                        let owner = owner.ok_or("child call needs a cell instance")?;
                        let (child, method) = self.cell_target(owner, child)?;
                        if let Some(method) = method {
                            self.references.calls.entry(owner).or_default().insert(child);
                            *name = label(self.module(Some(child), method)?);
                            return Ok(());
                        }
                        let ports = self
                            .program
                            .ports(self.program.root.find_cell(child).unwrap());
                        let outs: Vec<_> = ports
                            .iter()
                            .filter(|p| p.direction == Direction::Out)
                            .collect();
                        let unit_call = outs.is_empty()
                            && matches!(self.program.root.find_cell(child).unwrap().definition,
                            Definition::Code(id) if mira_core::code::find(&self.program.code_modules, id).is_some_and(|m| m.output == Type::Unit));
                        let named_call = matches!(self.program.root.find_cell(child).unwrap().definition,
                            Definition::Code(id) if mira_core::code::find(&self.program.code_modules, id).is_some_and(|m| m.output_ports));
                        if !unit_call && !named_call && (outs.len() != 1 || outs[0].name != "out") {
                            return Err(format!(
                                "{name}: a code call requires one result port named out; route selective outputs with wires"
                            ));
                        }
                        self.references
                            .calls
                            .entry(owner)
                            .or_default()
                            .insert(child);
                        *name = label(self.entry(child)?);
                    } else if let Some(output) = name.strip_prefix("graph.") {
                        let owner = owner.ok_or("graph call needs a cell instance")?;
                        let (space, output) = self.program.relative_scope(owner, output).ok_or("graph path has no Z scope")?;
                        let graph_owner = self.program.owner_of(space.id).ok_or("graph owner missing")?;
                        *name = label(self.graph(graph_owner, output)?);
                    } else if !vars.contains(name) {
                        if let Some(target) = mira_core::code::resolve(&self.program.code_modules, original, name)
                            .map(|d| d.id)
                        {
                            *name = label(self.module(owner, target)?);
                        }
                    }
                }
                Ok(())
            },
            &mut vars,
        )?;
        Ok(())
    }
    fn entry(&mut self, cell: Id) -> Result<Id> {
        if let Some(id) = self.entries.get(&cell) {
            return Ok(*id);
        }
        let c = self
            .program
            .root
            .find_cell(cell)
            .ok_or("called cell missing")?;
        if c.definition == Definition::Graph {
            return self.graph(cell, "out");
        }
        if let Definition::Code(module) = c.definition {
            let id = self.module(Some(cell), module)?;
            self.entries.insert(cell, id);
            return Ok(id);
        }
        let id = self.allocate()?;
        self.entries.insert(cell, id);
        self.wrappers.insert(cell);
        let ports = self.program.ports(c);
        let inputs = ports
            .iter()
            .filter(|p| p.direction == Direction::In)
            .map(|p| p.ty)
            .collect();
        let output = ports
            .iter()
            .find(|p| p.direction == Direction::Out && p.name == "out")
            .map_or(Type::Unit, |p| p.ty);
        self.external.insert(label(id), (inputs, output));
        Ok(id)
    }
    fn graph(&mut self, cell: Id, output: &str) -> Result<Id> {
        self.references
            .graphs
            .entry(cell)
            .or_default()
            .insert(output.to_owned());
        let key = (cell, output.to_owned());
        if let Some(id) = self.graphs.get(&key) {
            return Ok(*id);
        }
        let c = self
            .program
            .root
            .find_cell(cell)
            .ok_or("graph owner missing")?;
        let space = c
            .space
            .as_ref()
            .ok_or_else(|| format!("{}: no internal Z graph", c.name))?;
        let ty = space
            .outputs
            .iter()
            .find(|p| p.name == output)
            .ok_or_else(|| format!("{}: graph output {output} not found", c.name))?
            .ty;
        let inputs = space.inputs.iter().map(|p| p.ty).collect();
        let id = self.allocate()?;
        self.graphs.insert(key, id);
        self.external.insert(label(id), (inputs, ty));
        Ok(id)
    }
    fn wrapper(&mut self, cell: Id, graph: Option<(&str, Id)>) -> Result<CodeModule> {
        let c = self
            .program
            .root
            .find_cell(cell)
            .ok_or("called cell missing")?
            .clone();
        let id = graph.map_or_else(|| self.entries[&cell], |(_, id)| id);
        let ports = if graph.is_some() {
            let space = c.space.as_ref().ok_or("missing internal graph")?;
            space
                .inputs
                .iter()
                .map(|p| Port {
                    name: p.name.as_str(),
                    ty: p.ty,
                    direction: Direction::In,
                })
                .chain(space.outputs.iter().map(|p| Port {
                    name: p.name.as_str(),
                    ty: p.ty,
                    direction: Direction::Out,
                }))
                .collect()
        } else {
            self.program.ports(&c)
        };
        let result_name = graph.map_or("out", |(output, _)| output);
        let parameters: Vec<_> = ports
            .iter()
            .filter(|p| p.direction == Direction::In)
            .map(|p| Parameter {
                name: p.name.into(),
                ty: p.ty,
            })
            .collect();
        let output = ports
            .iter()
            .find(|p| p.direction == Direction::Out && p.name == result_name)
            .ok_or("called cell requires a result port")?
            .ty;
        let args = parameters
            .iter()
            .map(|p| Expr::Name(p.name.clone()))
            .collect::<Vec<_>>();
        let body = if graph.is_some() {
            let s = c.space.as_ref().ok_or("called cell has no graph")?;
            let terminals: Vec<_> = s
                .links
                .iter()
                .filter(|l| l.to.cell == s.id && l.to.port == result_name)
                .map(|l| l.from.clone())
                .collect();
            if terminals.is_empty() {
                return Err(format!(
                    "{}: graph output {result_name} is not connected",
                    c.name
                ));
            }
            let mut needed = BTreeSet::new();
            let mut pending = terminals.clone();
            while let Some(e) = pending.pop() {
                if e.cell == s.id || !needed.insert(e.cell) {
                    continue;
                }
                let op = s.cell(e.cell)?;
                for port in self
                    .program
                    .ports(op)
                    .iter()
                    .filter(|p| p.direction == Direction::In)
                {
                    let link = s
                        .links
                        .iter()
                        .find(|l| l.to.cell == op.id && l.to.port == port.name)
                        .ok_or_else(|| {
                            format!("{}/{}.{}: missing input", c.name, op.name, port.name)
                        })?;
                    pending.push(link.from.clone());
                }
            }
            // Each delivery has a separate presence bit. Zero/false are payloads,
            // never sentinels. Payload slots are read only under their presence guard.
            let name = |e: &Endpoint| format!("#{}.{}", e.cell, e.port);
            let value = |e: &Endpoint| {
                if e.cell == s.id {
                    Expr::Name(e.port.clone())
                } else {
                    Expr::Name(name(e))
                }
            };
            let present = |e: &Endpoint| {
                if e.cell == s.id {
                    Expr::Bool(true)
                } else {
                    Expr::Name(format!("{}?", name(e)))
                }
            };
            let bind = |name: String, ty: Option<Type>, value: Expr| Statement::Bind {
                name,
                ty,
                immutable: false,
                op: Assign::Set,
                value,
            };
            let binary = |op, a, b| Expr::Binary(op, Box::new(a), Box::new(b));
            let mut body = Vec::new();
            for op in s.order()? {
                if !needed.contains(&op) {
                    continue;
                }
                for port in self
                    .program
                    .ports(s.cell(op)?)
                    .iter()
                    .filter(|p| p.direction == Direction::Out)
                {
                    let key = format!("#{op}.{}", port.name);
                    // Private storage only: typed zero is never read while absent.
                    body.push(bind(key.clone(), Some(port.ty), if port.ty == Type::Bool {
                        Expr::Bool(false)
                    } else {
                        Expr::Int(0)
                    }));
                    body.push(bind(format!("{key}?"), Some(Type::Bool), Expr::Bool(false)));
                }
            }
            for op in s.order()? {
                if !needed.contains(&op) {
                    continue;
                }
                let child = s.cell(op)?;
                let sources: Vec<_> = self
                    .program
                    .ports(child)
                    .iter()
                    .filter(|p| p.direction == Direction::In)
                    .map(|p| {
                        s.links
                            .iter()
                            .find(|l| l.to.cell == op && l.to.port == p.name)
                            .map(|l| l.from.clone())
                            .ok_or_else(|| "graph input missing".into())
                    })
                    .collect::<Result<_>>()?;
                let guard = sources
                    .iter()
                    .fold(Expr::Bool(true), |a, e| binary(Binary::And, a, present(e)));
                let emit = |port: &str, payload: Expr| {
                    vec![
                        bind(format!("#{op}.{port}"), None, payload),
                        bind(format!("#{op}.{port}?"), None, Expr::Bool(true)),
                    ]
                };
                let operation = if let Definition::Branch(cmp, ty) = child.definition {
                    let op = match cmp {
                        Comparison::Lt => Binary::Lt,
                        Comparison::Le => Binary::Le,
                        Comparison::Gt => Binary::Gt,
                        Comparison::Ge => Binary::Ge,
                        Comparison::Eq => Binary::Eq,
                        Comparison::Ne => Binary::Ne,
                    };
                    let parameter = if ty == Type::Bool {
                        Expr::Bool(child.parameter != 0)
                    } else {
                        Expr::Int(child.parameter)
                    };
                    vec![Statement::If {
                        condition: binary(op, value(&sources[0]), parameter),
                        yes: emit("yes", value(&sources[0])),
                        no: emit("no", value(&sources[0])),
                    }]
                } else {
                    if child.definition == Definition::Graph {
                        let mut operations = Vec::new();
                        for p in self
                            .program
                            .ports(child)
                            .iter()
                            .filter(|p| p.direction == Direction::Out)
                        {
                            let entry = self.graph(op, p.name)?;
                            operations.extend(emit(
                                p.name,
                                Expr::Call(label(entry), sources.iter().map(&value).collect()),
                            ));
                        }
                        operations
                    } else {
                        let entry = self.entry(op)?;
                        let named = matches!(child.definition, Definition::Code(id) if mira_core::code::find(&self.program.code_modules, id).is_some_and(|m| m.output_ports));
                        let call = Expr::Call(label(entry), sources.iter().map(&value).collect());
                        if named {
                            let result = format!("#result.{op}");
                            let mut operations = vec![bind(result.clone(), None, call)];
                            for port in self
                                .program
                                .ports(child)
                                .iter()
                                .filter(|p| p.direction == Direction::Out)
                            {
                                operations.extend(emit(
                                    port.name,
                                    Expr::Field(
                                        Box::new(Expr::Name(result.clone())),
                                        port.name.into(),
                                    ),
                                ));
                            }
                            operations
                        } else {
                            emit("out", call)
                        }
                    }
                };
                body.push(Statement::If {
                    condition: guard,
                    yes: operation,
                    no: vec![],
                });
            }
            body.push(bind(
                "#result".into(),
                Some(output),
                match output {
                    Type::Bool => Expr::Bool(false),
                    Type::F32 => Expr::Float(0),
                    Type::Data(id) => Expr::Construct(id, vec![]),
                    _ => Expr::Int(0),
                },
            ));
            body.push(bind("#count".into(), Some(Type::I64), Expr::Int(0)));
            for terminal in &terminals {
                body.push(Statement::If {
                    condition: present(terminal),
                    yes: vec![
                        bind("#result".into(), None, value(terminal)),
                        bind(
                            "#count".into(),
                            None,
                            binary(Binary::Add, Expr::Name("#count".into()), Expr::Int(1)),
                        ),
                    ],
                    no: vec![],
                });
            }
            body.push(Statement::Expr(Expr::Call(
                "#graph.require".into(),
                vec![binary(
                    Binary::Eq,
                    Expr::Name("#count".into()),
                    Expr::Int(1),
                )],
            )));
            body.push(Statement::Expr(Expr::Name("#result".into())));
            body
        } else {
            vec![Statement::Expr(match c.definition {
                Definition::Const => Expr::Int(c.parameter),
                Definition::ConstBool => Expr::Bool(c.parameter != 0),
                Definition::Add => Expr::Binary(
                    Binary::Add,
                    Box::new(args[0].clone()),
                    Box::new(Expr::Int(c.parameter)),
                ),
                Definition::Mul => Expr::Binary(
                    Binary::Mul,
                    Box::new(args[0].clone()),
                    Box::new(Expr::Int(c.parameter)),
                ),
                Definition::AsEvent(_) => Expr::Int(1), // internal event token representation
                _ => return Err(format!("{}: unsupported scalar cell call", c.name)),
            })]
        };
        Ok(CodeModule {
            owner: None,
            data_types: vec![],
            id,
            name: label(id),
            parameters,
            output,
            annotated: true,
            output_ports: false,
            compile_time: false,
            body,
            locals: vec![],
        })
    }
}
fn builder<'a>(
    program: &'a Program,
    source_revision: Option<&'a blake3::Hash>,
) -> Result<Builder<'a>> {
    let mut b = Builder {
        references: References::default(),
        program,
        source_revision,
        next: program.next_id()?,
        modules: BTreeMap::new(),
        entries: BTreeMap::new(),
        definitions: vec![],
        origins: BTreeMap::new(),
        external: BTreeMap::new(),
        wrappers: BTreeSet::new(),
        graphs: BTreeMap::new(),
        binding_depth: 0,
        allocated: 0,
        reflection: BTreeMap::new(),
        reflection_rows: 0,
        derivatives: BTreeMap::new(),
    };
    fn cells(s: &Space, out: &mut Vec<Id>) {
        for c in &s.cells {
            if matches!(c.definition, Definition::Code(_) | Definition::Adaptive(_)) {
                out.push(c.id);
            }
            if let Some(inner) = &c.space {
                cells(inner, out);
            }
        }
    }
    let mut ids = Vec::new();
    cells(&program.root, &mut ids);
    // Bind every code body to its actual cell and owned graph context.
    for id in ids {
        let c = program.root.find_cell(id).unwrap();
        if let Definition::Code(module) = c.definition {
            let entry = b.module(Some(id), module)?;
            b.entries.insert(id, entry);
        }
        if let Definition::Adaptive(module) = c.definition {
            b.module(Some(id), module)?;
        }
    }
    // A global helper may itself delegate to a context-dependent definition.
    // Such templates are checked in their actual owner, not in a fake root.
    let all_modules = mira_core::code::symbols(&program.code_modules);
    let mut contextual: BTreeSet<_> = all_modules
        .iter()
        .filter(|(_, d)| uses_children(d))
        .map(|(_, d)| d.id)
        .collect();
    let mut reverse: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    fn dependencies(d: &CodeModule, program: &Program, calls: &mut BTreeSet<Id>) {
        let mut body = d.body.clone();
        let _ = each_expr(
            &mut body,
            &mut |e, _| {
                if let Expr::Call(name, args) = e {
                    let reference = if crate::intrinsics::symbol_reference(name)
                        || matches!(
                            name.as_str(),
                            "sys.neural_try" | "sys.scratch" | "sys.read_only"
                        ) {
                        match args.first() {
                            Some(Expr::Name(target)) => target,
                            _ => name,
                        }
                    } else {
                        name
                    };
                    if let Some(d) = mira_core::code::resolve(&program.code_modules, d.id, reference) {
                        calls.insert(d.id);
                    }
                }
                Ok(())
            },
            &mut BTreeSet::new(),
        );
    }
    for (_, d) in &all_modules {
        let mut calls = BTreeSet::new();
        dependencies(d, program, &mut calls);
        for callee in calls {
            reverse.entry(callee).or_default().push(d.id);
        }
    }
    let mut pending = contextual.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        for caller in reverse.get(&id).into_iter().flatten() {
            if contextual.insert(*caller) {
                pending.push(*caller);
            }
        }
    }
    for (_, d) in &all_modules {
        if !d.compile_time && !contextual.contains(&d.id) {
            b.module(None, d.id)?;
        }
    }
    crate::behavior::check_with(&mut b.definitions, &b.external, &program.data_types())
        .map_err(|error| source_error(error, &b.origins, program))?;
    Ok(b)
}
/// Infer contextual code signatures without requiring unused graph bodies to run.
pub(crate) fn infer(program: &mut Program) -> Result<()> {
    let b = builder(program, None)?;
    let mut outputs = BTreeMap::new();
    fn collect(
        defs: &[CodeModule],
        origins: &BTreeMap<Id, Id>,
        outputs: &mut BTreeMap<Id, Type>,
    ) -> Result<()> {
        for d in defs {
            if let Some(original) = origins.get(&d.id) {
                if outputs
                    .insert(*original, d.output)
                    .is_some_and(|old| old != d.output)
                {
                    return Err(
                        "shared code has inconsistent output types between cell contexts".into(),
                    );
                }
            }
            collect(&d.locals, origins, outputs)?;
        }
        Ok(())
    }
    collect(&b.definitions, &b.origins, &mut outputs)?;
    fn update(defs: &mut [CodeModule], outputs: &BTreeMap<Id, Type>) {
        for d in defs {
            if let Some(ty) = outputs.get(&d.id) {
                d.output = *ty;
            }
            update(&mut d.locals, outputs);
        }
    }
    update(&mut program.code_modules, &outputs);
    Ok(())
}
pub(crate) fn compile(program: &Program) -> Result<Executable> {
    compile_mode(program, false, &[], None)
}
/// Preserve the validated source revision across native Adaptive lowering.
pub(crate) fn compile_with_source_revision(
    program: &Program,
    source_revision: &blake3::Hash,
) -> Result<Executable> {
    compile_mode(program, false, &[], Some(source_revision))
}
/// Scheduler-owned entries are declarations, never substitute implementations.
pub(crate) fn compile_host(program: &Program, updates: &[(Id, Id)]) -> Result<Executable> {
    compile_mode(program, true, updates, None)
}
fn compile_mode(
    program: &Program,
    hosted: bool,
    updates: &[(Id, Id)],
    source_revision: Option<&blake3::Hash>,
) -> Result<Executable> {
    let mut b = builder(program, source_revision)?;
    for (owner, module) in updates {
        b.module(Some(*owner), *module)?;
    }
    let mut host_entries = BTreeSet::new();
    let mut done = BTreeSet::new();
    let mut graphs_done = BTreeSet::new();
    loop {
        if let Some(cell) = b.wrappers.iter().find(|id| !done.contains(*id)).copied() {
            done.insert(cell);
            let d = if hosted {
                let c = program.root.find_cell(cell).ok_or("host cell missing")?;
                let ports = program.ports(c);
                let id = b.entries[&cell];
                host_entries.insert(id);
                CodeModule {
                    owner: None,
                    id,
                    name: label(id),
                    parameters: ports
                        .iter()
                        .filter(|p| p.direction == Direction::In)
                        .map(|p| Parameter {
                            name: p.name.into(),
                            ty: p.ty,
                        })
                        .collect(),
                    output: ports
                        .iter()
                        .find(|p| p.direction == Direction::Out && p.name == "out")
                        .map_or(Type::Unit, |p| p.ty),
                    annotated: true,
                    output_ports: false,
                    compile_time: false,
                    body: vec![],
                    locals: vec![],
                    data_types: vec![],
                }
            } else {
                b.wrapper(cell, None)?
            };
            b.definitions.push(d);
        } else if let Some(((cell, output), id)) = b
            .graphs
            .iter()
            .find(|(key, _)| !graphs_done.contains(*key))
            .map(|(key, id)| (key.clone(), *id))
        {
            graphs_done.insert((cell, output.clone()));
            let d = if hosted {
                let space = program
                    .root
                    .find_cell(cell)
                    .and_then(|c| c.space.as_ref())
                    .ok_or("host graph missing")?;
                host_entries.insert(id);
                CodeModule {
                    owner: None,
                    id,
                    name: label(id),
                    parameters: space.inputs.clone(),
                    output: space
                        .outputs
                        .iter()
                        .find(|p| p.name == output)
                        .ok_or("host graph output missing")?
                        .ty,
                    annotated: true,
                    output_ports: false,
                    compile_time: false,
                    body: vec![],
                    locals: vec![],
                    data_types: vec![],
                }
            } else {
                b.wrapper(cell, Some((&output, id)))?
            };
            b.definitions.push(d);
        } else {
            break;
        }
    }
    // Wrappers may discover additional context-bound code. Recheck that code
    // against declared cell interfaces; internal graph temporaries are already typed by Core.
    let mut code = b
        .definitions
        .iter()
        .filter(|d| b.origins.contains_key(&d.id))
        .cloned()
        .collect::<Vec<_>>();
    crate::behavior::check_with(&mut code, &b.external, &program.data_types())
        .map_err(|error| source_error(error, &b.origins, program))?;
    let definitions = crate::behavior::procedures(&b.definitions, &program.data_types())?;
    let graph_slots = b
        .graphs
        .iter()
        .map(|((cell, _), entry)| {
            let space = program
                .root
                .find_cell(*cell)
                .unwrap()
                .space
                .as_ref()
                .unwrap();
            let slots = space
                .cells
                .iter()
                .flat_map(|c| {
                    program
                        .ports(c)
                        .into_iter()
                        .filter(|p| p.direction == Direction::Out)
                        .map(|p| (c.id, p.name.to_owned()))
                })
                .collect();
            (*entry, slots)
        })
        .collect();
    Ok(Executable {
        bound_modules: b.modules.clone(),
        host_entries,
        graph_slots,
        definitions,
        data_types: program.data_types(),
        graph_entries: b
            .graphs
            .into_iter()
            .map(|((cell, output), entry)| {
                (
                    entry,
                    (
                        program
                            .root
                            .find_cell(cell)
                            .unwrap()
                            .space
                            .as_ref()
                            .unwrap()
                            .id,
                        output,
                    ),
                )
            })
            .collect(),
        entries: b.entries,
    })
}

/// Use precisely the same owner/name binding as compilation; never infer calls from text search.
pub(crate) fn references(program: &Program) -> Result<References> {
    Ok(builder(program, None)?.references)
}

/// Bind additional learning behaviors in their actual owners and expose the
/// compiler's dependency information to the portable execution readiness check.
pub(crate) fn references_with_updates(
    program: &Program,
    updates: &[(Id, Id)],
) -> Result<References> {
    let mut b = builder(program, None)?;
    for (owner, module) in updates {
        b.module(Some(*owner), *module)?;
    }
    crate::behavior::check_with(&mut b.definitions, &b.external, &program.data_types())
        .map_err(|error| source_error(error, &b.origins, program))?;
    Ok(b.references)
}
