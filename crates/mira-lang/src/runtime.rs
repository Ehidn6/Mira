//! Reference execution of typed connections and composed code/graph calls.
use mira_core::*;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub struct Instruction {
    pub cell: Id,
    pub path: String,
    pub definition: Definition,
    pub parameter: i64,
    pub inputs: Vec<Endpoint>,
    pub guards: Vec<Endpoint>,
}
#[derive(Clone, Debug)]
pub struct Output {
    pub path: String,
    pub endpoint: Endpoint,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub instructions: Vec<Instruction>,
    pub outputs: Vec<Output>,
    pub observations: Vec<(Endpoint, Endpoint)>,
}
#[derive(Clone, Debug)]
pub struct Activation {
    pub id: u64,
    pub trained: usize,
    pub values: BTreeMap<Id, i64>,
    pub deliveries: BTreeMap<(Id, String), i64>,
    pub terminals: Vec<(String, i64)>,
    pub data: crate::data_runtime::Arena,
    pub terminal_types: BTreeMap<String, Type>,
}
fn resolved(e: &Endpoint, aliases: &BTreeMap<(Id, String), Endpoint>) -> Result<Endpoint> {
    let mut e = e.clone();
    for _ in 0..MAX_DEPTH * 4 + 16 {
        if let Some(next) = aliases.get(&(e.cell, e.port.clone())) {
            if *next == e {
                return Err("cyclic boundary alias".into());
            }
            e = next.clone();
        } else {
            return Ok(e);
        }
    }
    Err("boundary nesting budget exceeded".into())
}
pub fn lower(program: &Program) -> Result<Plan> {
    program.require_complete()?;
    crate::access::validate_source(program)?;
    program.validate()?;
    let mut checked = program.clone();
    crate::composition::infer(&mut checked)?;
    if checked.code_modules != program.code_modules {
        return Err("stored behavior signatures differ from inferred types".into());
    }
    struct Builder<'a> {
        program: &'a Program,
        plan: Plan,
        aliases: BTreeMap<(Id, String), Endpoint>,
    }
    impl Builder<'_> {
        fn scope(
            &mut self,
            s: &Space,
            path: &str,
            guards: &[Endpoint],
            expose: bool,
        ) -> Result<()> {
            // An owned graph body computes its declared outputs. Stored lessons
            // or other disconnected children are not activation roots.
            let mut needed = std::collections::BTreeSet::new();
            if !expose && !s.outputs.is_empty() {
                let mut pending: Vec<_> = s
                    .links
                    .iter()
                    .filter(|l| l.to.cell == s.id)
                    .map(|l| l.from.cell)
                    .collect();
                while let Some(id) = pending.pop() {
                    if id == s.id || !needed.insert(id) {
                        continue;
                    }
                    pending.extend(
                        s.links
                            .iter()
                            .filter(|l| l.to.cell == id)
                            .map(|l| l.from.cell),
                    );
                }
            }
            for input in &s.inputs {
                if !self.aliases.contains_key(&(s.id, input.name.clone())) {
                    return Err(format!("{path}/{}: no parent activation input", input.name));
                }
            }
            for id in s.order()? {
                if !expose && !s.outputs.is_empty() && !needed.contains(&id) {
                    continue;
                }
                let c = s.cell(id)?;
                let ports = self.program.ports(c);
                let mut inputs = Vec::new();
                for port in ports.iter().filter(|p| p.direction == Direction::In) {
                    let link = s
                        .links
                        .iter()
                        .find(|l| l.to.cell == id && l.to.port == port.name)
                        .ok_or_else(|| format!("{path}/{}.{}: missing input", c.name, port.name))?;
                    inputs.push(resolved(&link.from, &self.aliases)?);
                }
                let cell_path = format!("{path}/{}", c.name);
                if c.definition == Definition::Graph {
                    let inner = c.space.as_ref().ok_or("graph cell has no Z body")?;
                    for (port, input) in ports
                        .iter()
                        .filter(|p| p.direction == Direction::In)
                        .zip(&inputs)
                    {
                        self.aliases
                            .insert((inner.id, port.name.into()), input.clone());
                    }
                    let mut child_guards = guards.to_vec();
                    child_guards.extend(inputs.iter().cloned());
                    self.scope(inner, &cell_path, &child_guards, false)?;
                    for port in &inner.outputs {
                        let leaf = resolved(
                            &Endpoint {
                                cell: inner.id,
                                port: port.name.clone(),
                            },
                            &self.aliases,
                        )?;
                        self.aliases.insert((id, port.name.clone()), leaf.clone());
                        self.plan.observations.push((
                            Endpoint {
                                cell: id,
                                port: port.name.clone(),
                            },
                            leaf,
                        ));
                    }
                } else {
                    self.plan.instructions.push(Instruction {
                        cell: id,
                        path: cell_path.clone(),
                        definition: c.definition,
                        parameter: c.parameter,
                        inputs,
                        guards: guards.to_vec(),
                    });
                }
                if expose && s.outputs.is_empty() {
                    for port in ports.iter().filter(|p| p.direction == Direction::Out) {
                        if !s
                            .links
                            .iter()
                            .any(|l| l.from.cell == id && l.from.port == port.name)
                        {
                            let endpoint = resolved(
                                &Endpoint {
                                    cell: id,
                                    port: port.name.into(),
                                },
                                &self.aliases,
                            )?;
                            self.plan.outputs.push(Output {
                                path: if port.name == "out" {
                                    cell_path.clone()
                                } else {
                                    format!("{cell_path}.{}", port.name)
                                },
                                endpoint,
                            });
                        }
                    }
                }
            }
            for port in &s.outputs {
                let link = s
                    .links
                    .iter()
                    .find(|l| l.to.cell == s.id && l.to.port == port.name)
                    .ok_or_else(|| {
                        format!("{path}/{}: output interface is not connected", port.name)
                    })?;
                let leaf = resolved(&link.from, &self.aliases)?;
                self.aliases.insert((s.id, port.name.clone()), leaf.clone());
                self.plan.observations.push((
                    Endpoint {
                        cell: s.id,
                        port: port.name.clone(),
                    },
                    leaf.clone(),
                ));
                if expose {
                    self.plan.outputs.push(Output {
                        path: format!("{path}.{}", port.name),
                        endpoint: leaf,
                    });
                }
            }
            Ok(())
        }
    }
    let mut b = Builder {
        program,
        plan: Plan {
            instructions: Vec::new(),
            outputs: Vec::new(),
            observations: Vec::new(),
        },
        aliases: BTreeMap::new(),
    };
    b.scope(&program.root, &program.root.name, &[], true)?;
    b.plan.outputs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(b.plan)
}
/// All hosted execution uses Core's bounded graph scheduler. Plan lowering
/// above remains a compiler input, not an alternative interpreter.
pub fn execute(program: &Program, id: u64) -> Result<Activation> {
    let mut system = mira_core::system::System::from_program(program.clone())?;
    execute_system(&mut system, id)
}
pub fn execute_cell(program: &Program, cell: Id, id: u64) -> Result<Activation> {
    let mut system = mira_core::system::System::from_program(program.clone())?;
    execute_cell_system(&mut system, cell, id)
}
fn scoped_system(
    system: &mira_core::system::System,
    program: Program,
) -> mira_core::system::System {
    let mut scoped = system.clone();
    scoped.program = program;
    let exists = |id: &Id| {
        scoped.program.root.find(*id).is_some() || scoped.program.root.find_cell(*id).is_some()
    };
    scoped.parameters.retain(|id, _| {
        scoped
            .program
            .root
            .find_cell(*id)
            .is_some_and(|c| matches!(c.definition, Definition::Neural | Definition::Adaptive(_)))
    });
    scoped
        .buffers
        .retain(|id, _| scoped.program.root.find_cell(*id).is_some());
    scoped.delays.retain(|id, _| {
        scoped
            .program
            .root
            .find_cell(*id)
            .is_some_and(|c| c.definition == Definition::Delay)
    });
    scoped.data.retain(|id, _| exists(id));
    scoped.memories.retain(|id, _| exists(id));
    scoped.protected.retain(exists);
    scoped.pending.clear();
    scoped
}
fn publish_scope(system: &mut mira_core::system::System, scoped: mira_core::system::System) {
    system.parameters.extend(scoped.parameters);
    system.buffers.extend(scoped.buffers);
    system.delays.extend(scoped.delays);
    system.tick = scoped.tick;
}
pub fn execute_cell_system(
    system: &mut mira_core::system::System,
    cell: Id,
    id: u64,
) -> Result<Activation> {
    fn containing(s: &Space, cell: Id) -> Option<&Space> {
        if s.cells.iter().any(|c| c.id == cell) {
            return Some(s);
        }
        s.cells
            .iter()
            .filter_map(|c| c.space.as_ref())
            .find_map(|s| containing(s, cell))
    }
    let source = containing(&system.program.root, cell).ok_or("selected cell not found")?;
    let mut needed = std::collections::BTreeSet::new();
    let mut pending = vec![cell];
    while let Some(id) = pending.pop() {
        if !needed.insert(id) {
            continue;
        }
        let c = source.cell(id)?;
        for port in system
            .program
            .ports(c)
            .iter()
            .filter(|p| p.direction == Direction::In)
        {
            let link = source
                .links
                .iter()
                .find(|l| l.to.cell == id && l.to.port == port.name)
                .ok_or_else(|| {
                    format!(
                        "{}.{}: missing input; connect a source before running this cell",
                        c.name, port.name
                    )
                })?;
            if link.from.cell == source.id {
                return Err(format!(
                    "{}.{}: input comes from the parent boundary; run its owning cell to supply arguments",
                    c.name, port.name
                ));
            }
            pending.push(link.from.cell);
        }
    }
    let path = cell_path(&system.program, cell).ok_or("cell path missing")?;
    let short = format!("{}/{}", source.name, source.cell(cell)?.name);
    let mut program = system.program.clone();
    program.root = source.clone();
    program.root.cells.retain(|c| needed.contains(&c.id));
    program
        .root
        .links
        .retain(|l| needed.contains(&l.to.cell) && needed.contains(&l.from.cell));
    program.root.inputs.clear();
    program.root.outputs.clear();
    let mut scoped = scoped_system(system, program);
    let mut activation = scheduled(&mut scoped, &BTreeMap::new(), &BTreeMap::new(), Some(id))?;
    activation
        .terminals
        .retain(|(name, _)| name == &short || name.starts_with(&format!("{short}.")));
    for (name, _) in &mut activation.terminals {
        *name = name.replacen(&short, &path, 1);
    }
    activation.terminal_types = activation
        .terminal_types
        .into_iter()
        .filter(|(name, _)| name == &short || name.starts_with(&format!("{short}.")))
        .map(|(name, ty)| (name.replacen(&short, &path, 1), ty))
        .collect();
    publish_scope(system, scoped);
    Ok(activation)
}
pub fn cell_path(program: &Program, cell: Id) -> Option<String> {
    fn walk(s: &Space, cell: Id, path: &str) -> Option<String> {
        for c in &s.cells {
            let child = format!("{path}/{}", c.name);
            if c.id == cell {
                return Some(child);
            }
            if let Some(inner) = &c.space {
                if let Some(found) = walk(inner, cell, &child) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&program.root, cell, &program.root.name)
}
pub fn execute_system(system: &mut mira_core::system::System, id: u64) -> Result<Activation> {
    scheduled(system, &BTreeMap::new(), &BTreeMap::new(), Some(id))
}
pub(crate) fn scheduled(
    system: &mut mira_core::system::System,
    inputs: &BTreeMap<(Id, String), i64>,
    targets: &BTreeMap<Id, i64>,
    id: Option<u64>,
) -> Result<Activation> {
    scheduled_with_scratch(system, inputs, targets, id, None)
}
fn scheduled_with_scratch(
    system: &mut mira_core::system::System,
    inputs: &BTreeMap<(Id, String), i64>,
    targets: &BTreeMap<Id, i64>,
    id: Option<u64>,
    scratch: Option<usize>,
) -> Result<Activation> {
    let mut host = crate::system::Host::default();
    host.scratch_budget = scratch;
    let report = engine::tick(system, inputs, targets, &mut host)?;
    let mut terminals = Vec::new();
    let mut terminal_types = BTreeMap::new();
    let root = &system.program.root;
    if root.outputs.is_empty() {
        for c in &root.cells {
            for port in system
                .program
                .ports(c)
                .iter()
                .filter(|p| p.direction == Direction::Out)
            {
                if root
                    .links
                    .iter()
                    .any(|l| l.from.cell == c.id && l.from.port == port.name)
                {
                    continue;
                }
                if let Some(value) = report.deliveries.get(&(c.id, port.name.into())) {
                    let path = if port.name == "out" {
                        format!("{}/{}", root.name, c.name)
                    } else {
                        format!("{}/{}.{}", root.name, c.name, port.name)
                    };
                    terminals.push((path.clone(), *value));
                    terminal_types.insert(path, port.ty);
                }
            }
        }
    } else {
        for port in &root.outputs {
            if let Some(value) = report.outputs.get(&(root.id, port.name.clone())) {
                let path = format!("{}.{}", root.name, port.name);
                terminals.push((path.clone(), *value));
                terminal_types.insert(path, port.ty);
            }
        }
    }
    terminals.sort_by(|a, b| a.0.cmp(&b.0));
    let values = report
        .deliveries
        .iter()
        .map(|((cell, _), value)| (*cell, *value))
        .collect();
    Ok(Activation {
        id: id.unwrap_or(report.tick),
        trained: report.trained,
        values,
        deliveries: report.deliveries,
        terminals,
        data: host.arena,
        terminal_types,
    })
}
/// Invoke an existing Mira behavior in the context of an owning graph cell.
/// This is the shared typed execution boundary used by managed tensor learning.
/// Buffers are imported before the call and exported only after successful return.
/// For read-only evaluation the compiler checks the same sys.read_only contract
/// used by ordinary source programs; scratch storage never escapes the call.
pub fn execute_behavior_system(
    system: &mut mira_core::system::System,
    owner: Id,
    behavior: Id,
    id: u64,
    read_only: bool,
    scratch_bytes: usize,
) -> Result<Activation> {
    crate::access::validate_source(&system.program)?;
    system.program.validate()?;
    let module = mira_core::code::find(&system.program.code_modules, behavior)
        .ok_or("typed execution behavior missing")?
        .clone();
    if !module.parameters.is_empty() {
        return Err(
            "typed execution entry must have no explicit inputs; use declared input resources"
                .into(),
        );
    }
    let cell = system
        .program
        .root
        .find_cell(owner)
        .ok_or("typed execution owner missing")?
        .clone();
    let mut program = system.program.clone();
    let mut scoped = Space::new(program.root.id, program.root.name.clone());
    let mut entry = cell;
    entry.definition = Definition::Code(behavior);
    scoped.cells.push(entry);
    program.root = scoped;
    if read_only {
        if matches!(module.output, Type::Data(_))
            || scratch_bytes > crate::data_runtime::ARENA_BYTES
        {
            return Err(
                "read-only typed evaluation requires a scalar result and bounded scratch".into(),
            );
        }
        let wrapper = program.next_id()?;
        let mut name = "typed_evaluation_entry".to_string();
        while program.code_modules.iter().any(|m| m.name == name) {
            name.push('_');
        }
        program.code_modules.push(code::CodeModule {
            owner: None,
            id: wrapper,
            name,
            parameters: vec![],
            output: module.output,
            output_ports: false,
            compile_time: false,
            annotated: true,
            body: vec![code::Statement::Expr(code::Expr::Call(
                "sys.read_only".into(),
                vec![
                    code::Expr::Name(code::path(&program.code_modules, module.id).ok_or("typed entry path missing")?),
                    code::Expr::Int(scratch_bytes as i64),
                ],
            ))],
            locals: vec![],
            data_types: vec![],
        });
        program.root.cells[0].definition = Definition::Code(wrapper);
        program.next_identity = wrapper
            .checked_add(1)
            .ok_or("typed execution identity exhausted")?;
    }
    crate::composition::infer(&mut program)?;
    let path = cell_path(&system.program, owner).ok_or("typed execution owner path missing")?;
    let short = format!("{}/{}", program.root.name, program.root.cells[0].name);
    let mut scoped = scoped_system(system, program);
    let mut activation = scheduled_with_scratch(
        &mut scoped,
        &BTreeMap::new(),
        &BTreeMap::new(),
        Some(id),
        Some(scratch_bytes),
    )?;
    for (name, _) in &mut activation.terminals {
        *name = name.replacen(&short, &path, 1);
    }
    activation.terminal_types = activation
        .terminal_types
        .into_iter()
        .map(|(name, ty)| (name.replacen(&short, &path, 1), ty))
        .collect();
    publish_scope(system, scoped);
    Ok(activation)
}
pub(crate) fn initialize_persistent(
    program: &Program,
    executable: &crate::composition::Executable,
    data: &mut crate::data_runtime::Arena,
) -> Result<()> {
    for (cell, ty) in crate::tensor::persistent_references(&executable.definitions)? {
        let owner = program
            .root
            .find_cell(cell)
            .ok_or("persistent owner missing")?;
        let ports = program.ports(owner);
        if ports.iter().any(|p| p.direction == Direction::In)
            || !ports
                .iter()
                .any(|p| p.direction == Direction::Out && p.ty == Type::Data(ty))
        {
            return Err("persistent buffers require a dedicated typed cell without inputs".into());
        }
        data.persistent_buffer(cell, ty, &program.data_types())?;
    }
    Ok(())
}
