//! Bounded source staging over the actual Core graph-edit transaction model.
//! Expansion edits a draft, then replays one journal before publishing anything.
use mira_core::{
    code::*,
    system::System,
    transform::{Edit, Origin, Proposal},
    *,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Invocation {
    pub name: String,
    pub args: Vec<i64>,
}
pub(crate) fn signature(name: &str) -> Option<(Vec<Type>, Type)> {
    let (arity, output) = match name {
        "meta.clone_cell" => (2, Type::I64),
        "meta.quote" | "meta.parameter" => (2, Type::I64),
        "meta.integer" | "meta.boolean" | "meta.float_bits" => (1, Type::I64),
        "meta.binary" => (3, Type::I64),
        "meta.replace" => (2, Type::Unit),
        "meta.connect" => (5, Type::I64),
        "meta.disconnect" => (2, Type::Unit),
        "meta.remove_cell" => (1, Type::Unit),
        "meta.set_parameter" => (2, Type::Unit),
        _ => return None,
    };
    Some((vec![Type::I64; arity], output))
}
const FUEL: u64 = 100_000;
#[derive(Clone)]
struct CodeValue {
    expression: Expr,
    anchor: Option<(Id, Vec<usize>)>,
    // A quoted subtree may only stay within its original lexical location.
    origins: Vec<(Id, Vec<usize>)>,
    parameters: Vec<(Id, String)>,
}
struct Stage {
    draft: System,
    edits: Vec<Edit>,
    cursor: Id,
    allocated: usize,
    structural_remaining: usize,
    syntax: Vec<CodeValue>,
}
impl Stage {
    fn syntax_value(&self, handle: i64) -> Result<&CodeValue> {
        usize::try_from(handle)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.syntax.get(i))
            .ok_or_else(|| "macro syntax handle is invalid or belongs to another expansion".into())
    }
    fn store_syntax(&mut self, value: CodeValue) -> Result<i64> {
        if self.syntax.len() >= 2048 {
            return Err("macro syntax handle budget exceeded".into());
        }
        let mut stack = vec![(&value.expression, 0)];
        let mut count = 0usize;
        while let Some((e, depth)) = stack.pop() {
            count += 1;
            if count > 4096 || depth > MAX_DEPTH {
                return Err("macro expression budget exceeded".into());
            }
            stack.extend(children(e).into_iter().map(|e| (e, depth + 1)));
        }
        self.structural_remaining = self
            .structural_remaining
            .checked_sub(count)
            .ok_or("macro structural work budget exceeded")?;
        if value.origins.len() + value.parameters.len() > 4096 {
            return Err("macro scope budget exceeded".into());
        }
        self.syntax.push(value);
        Ok(self.syntax.len() as i64)
    }
    fn replace_expression(&mut self, target: i64, value: i64) -> Result<()> {
        let target = self.syntax_value(target)?.clone();
        let value = self.syntax_value(value)?.clone();
        let (definition, path) = target
            .anchor
            .ok_or("macro replacement needs a quoted target")?;
        if value
            .origins
            .iter()
            .any(|(id, p)| *id != definition || !p.starts_with(&path))
        {
            return Err("macro cannot move a quoted expression outside its lexical scope".into());
        }
        let module = mira_core::code::find(&self.draft.program.code_modules, definition)
            .ok_or("macro definition missing")?;
        // Parameters are immutable, but for-bindings can shadow them. Reject
        // that capture conservatively; this stage does not introduce binders.
        fn shadows(body: &[Statement], name: &str) -> bool {
            body.iter().any(|s| match s {
                Statement::Bind { name: n, .. } => n == name,
                Statement::For { name: n, body, .. } => n == name || shadows(body, name),
                Statement::If { yes, no, .. } => shadows(yes, name) || shadows(no, name),
                Statement::While { body, .. } => shadows(body, name),
                _ => false,
            })
        }
        for (id, name) in &value.parameters {
            if *id != definition || shadows(&module.body, name) {
                return Err("macro parameter reference would capture a different binding".into());
            }
        }
        self.edit(Edit::ReplaceExpression {
            definition,
            path,
            expected: target.expression,
            value: value.expression,
        })
    }
    fn charge(&mut self) -> Result<()> {
        fn spend(remaining: &mut usize) -> Result<()> {
            *remaining = remaining
                .checked_sub(1)
                .ok_or("macro structural work budget exceeded")?;
            Ok(())
        }
        fn graph(graph: &Space, remaining: &mut usize) -> Result<()> {
            spend(remaining)?;
            for _ in graph.inputs.iter().chain(&graph.outputs) {
                spend(remaining)?;
            }
            for cell in &graph.cells {
                spend(remaining)?;
                if let Some(inner) = &cell.space {
                    graph_cost(inner, remaining)?;
                }
            }
            for _ in &graph.links {
                spend(remaining)?;
            }
            Ok(())
        }
        fn graph_cost(space: &Space, remaining: &mut usize) -> Result<()> {
            graph(space, remaining)
        }
        fn modules(modules: &[CodeModule], remaining: &mut usize) -> Result<()> {
            for module in modules {
                spend(remaining)?;
                for _ in &module.parameters {
                    spend(remaining)?;
                }
                for ty in &module.data_types {
                    spend(remaining)?;
                    for _ in 0..ty.len().min(MAX_ELEMENTS) {
                        spend(remaining)?;
                    }
                }
                modules_cost(&module.locals, remaining)?;
            }
            Ok(())
        }
        fn modules_cost(definitions: &[CodeModule], remaining: &mut usize) -> Result<()> {
            modules(definitions, remaining)
        }
        graph(&self.draft.program.root, &mut self.structural_remaining)?;
        modules(
            &self.draft.program.code_modules,
            &mut self.structural_remaining,
        )?;
        for ty in &self.draft.program.global_types {
            spend(&mut self.structural_remaining)?;
            for _ in 0..ty.len().min(MAX_ELEMENTS) {
                spend(&mut self.structural_remaining)?;
            }
        }
        Ok(())
    }
    fn allocate(&mut self) -> Result<Id> {
        self.allocated = self
            .allocated
            .checked_add(1)
            .ok_or("macro allocation budget exhausted")?;
        if self.allocated > MAX_ELEMENTS {
            return Err("macro allocation budget exhausted".into());
        }
        self.structural_remaining = self
            .structural_remaining
            .checked_sub(1)
            .ok_or("macro structural work budget exceeded")?;
        let id = self.cursor;
        i64::try_from(id).map_err(|_| "macro identity exceeds the current i64 structural ABI")?;
        self.cursor = id.checked_add(1).ok_or("macro identity exhausted")?;
        Ok(id)
    }
    fn edit(&mut self, edit: Edit) -> Result<()> {
        if self.edits.len() >= 1024 {
            return Err("macro structural edit budget exceeded".into());
        }
        let proposal = Proposal {
            origin: Origin::System,
            explanation: "Source macro expansion".into(),
            revision: self.draft.revision,
            label: "Source macro expansion".into(),
            edits: vec![edit.clone()],
        };
        self.draft = proposal.prepare(&self.draft)?;
        self.edits.push(edit);
        Ok(())
    }
    fn clone_cell(&mut self, template: Id, target: Id) -> Result<Id> {
        let mut cell = self
            .draft
            .program
            .root
            .find_cell(template)
            .ok_or("macro template cell missing")?
            .clone();
        self.draft
            .program
            .root
            .find(target)
            .ok_or("macro target graph missing")?;
        let mut ids = BTreeMap::new();
        fn assign(
            stage: &mut Stage,
            cell: &mut Instance,
            ids: &mut BTreeMap<Id, Id>,
            depth: usize,
        ) -> Result<()> {
            if depth > MAX_DEPTH {
                return Err("macro clone exceeds graph depth budget".into());
            }
            let id = stage.allocate()?;
            ids.insert(cell.id, id);
            cell.id = id;
            if let Some(graph) = &mut cell.space {
                let id = stage.allocate()?;
                ids.insert(graph.id, id);
                graph.id = id;
                for child in &mut graph.cells {
                    assign(stage, child, ids, depth + 1)?;
                }
                for link in &mut graph.links {
                    link.id = stage.allocate()?;
                }
            }
            Ok(())
        }
        fn rewire(cell: &mut Instance, ids: &BTreeMap<Id, Id>) -> Result<()> {
            if let Some(graph) = &mut cell.space {
                for link in &mut graph.links {
                    link.from.cell = *ids
                        .get(&link.from.cell)
                        .ok_or("macro cloned graph contains a nonlocal source")?;
                    link.to.cell = *ids
                        .get(&link.to.cell)
                        .ok_or("macro cloned graph contains a nonlocal target")?;
                }
                for child in &mut graph.cells {
                    rewire(child, ids)?;
                }
            }
            Ok(())
        }
        assign(self, &mut cell, &mut ids, 0)?;
        rewire(&mut cell, &ids)?;
        let id = cell.id;
        // Names are local to the destination graph. The fresh global ID gives a
        // stable suffix; reserved port names are checked as well as sibling cells.
        let base = format!("{}_copy{id}", cell.name);
        cell.name = base;
        let graph = self
            .draft
            .program
            .root
            .find(target)
            .ok_or("macro target graph missing")?;
        while graph.cells.iter().any(|c| c.name == cell.name)
            || graph
                .inputs
                .iter()
                .chain(&graph.outputs)
                .any(|p| p.name == cell.name)
        {
            cell.name.push('_');
        }
        self.edit(Edit::AddCell {
            space: target,
            cell,
        })?;
        Ok(id)
    }
    fn port(&self, space: Id, cell: Id, index: i64, source: bool) -> Result<(String, Type)> {
        let index = usize::try_from(index).map_err(|_| "macro port index must be nonnegative")?;
        let graph = self
            .draft
            .program
            .root
            .find(space)
            .ok_or("macro graph missing")?;
        if cell == space {
            let ports = if source {
                &graph.inputs
            } else {
                &graph.outputs
            };
            let port = ports
                .get(index)
                .ok_or("macro boundary port index out of range")?;
            return Ok((port.name.clone(), port.ty));
        }
        let cell = graph
            .cells
            .iter()
            .find(|c| c.id == cell)
            .ok_or("macro connection must use direct cells or the graph boundary")?;
        let direction = if source {
            Direction::Out
        } else {
            Direction::In
        };
        let ports = self.draft.program.ports(cell);
        let port = ports
            .iter()
            .filter(|p| p.direction == direction)
            .nth(index)
            .ok_or("macro port index out of range")?;
        Ok((port.name.into(), port.ty))
    }
}
fn identity(value: i64) -> Result<Id> {
    let id = Id::try_from(value).map_err(|_| "macro identity must be positive")?;
    if id == 0 {
        return Err("macro identity must be positive".into());
    }
    Ok(id)
}
impl crate::behavior::StructuralHost for Stage {
    fn call(&mut self, name: &str, args: &[i64]) -> Result<i64> {
        self.charge()?;
        if name.starts_with("reflect.") {
            return crate::reflection::evaluate(name, &self.draft.program, None, args);
        }
        match (name, args) {
            ("meta.quote", [definition, index]) => {
                let id = identity(*definition)?;
                let d = mira_core::code::find(&self.draft.program.code_modules, id)
                    .ok_or("macro definition missing")?;
                if d.compile_time {
                    return Err("a running expansion cannot rewrite compile-time code".into());
                }
                let nodes = mira_core::code::expressions(d)?;
                let n = usize::try_from(*index)
                    .ok()
                    .and_then(|i| nodes.get(i))
                    .ok_or("macro expression missing")?;
                self.store_syntax(CodeValue {
                    expression: n.expression.clone(),
                    anchor: Some((id, n.path.clone())),
                    origins: vec![(id, n.path.clone())],
                    parameters: vec![],
                })
            }
            ("meta.parameter", [definition, index]) => {
                let id = identity(*definition)?;
                let d = mira_core::code::find(&self.draft.program.code_modules, id)
                    .ok_or("macro definition missing")?;
                let p = usize::try_from(*index)
                    .ok()
                    .and_then(|i| d.parameters.get(i))
                    .ok_or("macro parameter missing")?;
                self.store_syntax(CodeValue {
                    expression: Expr::Name(p.name.clone()),
                    anchor: None,
                    origins: vec![],
                    parameters: vec![(id, p.name.clone())],
                })
            }
            ("meta.integer" | "meta.boolean" | "meta.float_bits", [value]) => {
                let expression = match name {
                    "meta.boolean" if matches!(*value, 0 | 1) => Expr::Bool(*value != 0),
                    "meta.boolean" => return Err("boolean syntax needs 0 or 1".into()),
                    "meta.float_bits" => Expr::Float(
                        u32::try_from(*value).map_err(|_| "float syntax needs u32 bits")?,
                    ),
                    _ => Expr::Int(*value),
                };
                self.store_syntax(CodeValue {
                    expression,
                    anchor: None,
                    origins: vec![],
                    parameters: vec![],
                })
            }
            ("meta.binary", [op, a, b]) => {
                let op = match op {
                    0 => Binary::Add,
                    1 => Binary::Sub,
                    2 => Binary::Mul,
                    3 => Binary::Div,
                    4 => Binary::Eq,
                    5 => Binary::Lt,
                    6 => Binary::And,
                    7 => Binary::Or,
                    _ => return Err("unknown macro binary operator".into()),
                };
                let a = self.syntax_value(*a)?.clone();
                let b = self.syntax_value(*b)?.clone();
                self.store_syntax(CodeValue {
                    expression: Expr::Binary(op, Box::new(a.expression), Box::new(b.expression)),
                    anchor: None,
                    origins: a.origins.into_iter().chain(b.origins).collect(),
                    parameters: a.parameters.into_iter().chain(b.parameters).collect(),
                })
            }
            ("meta.replace", [target, value]) => {
                self.replace_expression(*target, *value)?;
                Ok(0)
            }
            ("meta.clone_cell", [template, target]) => {
                i64::try_from(self.clone_cell(identity(*template)?, identity(*target)?)?)
                    .map_err(|_| "macro identity exceeds i64".into())
            }
            ("meta.connect", [space, source, source_index, target, target_index]) => {
                let space = identity(*space)?;
                let source = identity(*source)?;
                let target = identity(*target)?;
                let (from, a) = self.port(space, source, *source_index, true)?;
                let (to, b) = self.port(space, target, *target_index, false)?;
                if a != b {
                    return Err("macro connection has incompatible port types".into());
                }
                let id = self.allocate()?;
                self.edit(Edit::Connect {
                    space,
                    link: Link {
                        id,
                        from: Endpoint {
                            cell: source,
                            port: from,
                        },
                        to: Endpoint {
                            cell: target,
                            port: to,
                        },
                    },
                })?;
                Ok(id as i64)
            }
            ("meta.disconnect", [space, link]) => {
                self.edit(Edit::Disconnect {
                    space: identity(*space)?,
                    link: identity(*link)?,
                })?;
                Ok(0)
            }
            ("meta.remove_cell", [cell]) => {
                self.edit(Edit::RemoveCell {
                    cell: identity(*cell)?,
                })?;
                Ok(0)
            }
            ("meta.set_parameter", [cell, value]) => {
                self.edit(Edit::SetParameter {
                    cell: identity(*cell)?,
                    value: *value,
                })?;
                Ok(0)
            }
            _ => Err(format!(
                "{name}: unknown structural operation or wrong argument count"
            )),
        }
    }
}
fn resolve<'a>(program: &'a Program, owner: Id, name: &str) -> Option<&'a CodeModule> {
    mira_core::code::resolve(&program.code_modules, owner, name)
}
/// Reject ambient effects throughout the reachable source call graph, including
/// unused branches. The interpreter additionally bounds all loops and recursion.
fn pure_staging(
    program: &Program,
    module: &CodeModule,
    active: &mut BTreeSet<Id>,
    done: &mut BTreeSet<Id>,
) -> Result<()> {
    if done.contains(&module.id) {
        return Ok(());
    }
    if active.len() >= MAX_DEPTH || !active.insert(module.id) {
        return Err("macro helper recursion/depth budget exceeded".into());
    }
    fn expression(
        program: &Program,
        owner: Id,
        expr: &Expr,
        active: &mut BTreeSet<Id>,
        done: &mut BTreeSet<Id>,
    ) -> Result<()> {
        match expr {
            Expr::Call(name, args) => {
                for arg in args {
                    expression(program, owner, arg, active, done)?;
                }
                if name.starts_with("meta.")
                    || name.starts_with("reflect.")
                    || crate::tensor::is_layout_query(name)
                {
                    return Ok(());
                }
                if crate::intrinsics::pure_address(name)
                    || crate::intrinsics::pure_integer(name)
                    || matches!(
                        name.as_str(),
                        "math.sqrt" | "math.to_f32" | "math.bits" | "math.from_bits"
                    )
                {
                    return Ok(());
                }
                if name.starts_with("sys.")
                    || name.starts_with("cells.")
                    || name.starts_with("graph.")
                    || name.starts_with("diff.")
                    || name.starts_with('#')
                    || name.ends_with(".persist")
                {
                    return Err(format!(
                        "{name}: macro expansion cannot perform runtime, hardware, persistent or nested differentiation effects"
                    ));
                }
                let helper = resolve(program, owner, name)
                    .ok_or_else(|| format!("{name}: unknown pure macro helper"))?;
                pure_staging(program, helper, active, done)
            }
            Expr::Unary(_, a) => expression(program, owner, a, active, done),
            Expr::Binary(_, a, b) => {
                expression(program, owner, a, active, done)?;
                expression(program, owner, b, active, done)
            }
            Expr::Choose(c, a, b) => {
                expression(program, owner, c, active, done)?;
                expression(program, owner, a, active, done)?;
                expression(program, owner, b, active, done)
            }
            Expr::Construct(..) | Expr::View(..) | Expr::Field(..) | Expr::Index(..) => {
                Err("macro expansion currently accepts scalar computations only".into())
            }
            _ => Ok(()),
        }
    }
    fn body(
        program: &Program,
        owner: Id,
        statements: &[Statement],
        active: &mut BTreeSet<Id>,
        done: &mut BTreeSet<Id>,
    ) -> Result<()> {
        for statement in statements {
            match statement {
                Statement::Bind { value, .. } | Statement::Expr(value) => {
                    expression(program, owner, value, active, done)?
                }
                Statement::If { condition, yes, no } => {
                    expression(program, owner, condition, active, done)?;
                    body(program, owner, yes, active, done)?;
                    body(program, owner, no, active, done)?;
                }
                Statement::While {
                    condition,
                    body: inner,
                } => {
                    expression(program, owner, condition, active, done)?;
                    body(program, owner, inner, active, done)?;
                }
                Statement::For {
                    start,
                    end,
                    body: inner,
                    ..
                } => {
                    expression(program, owner, start, active, done)?;
                    expression(program, owner, end, active, done)?;
                    body(program, owner, inner, active, done)?;
                }
                Statement::Set { .. } => {
                    return Err("macro expansion cannot mutate runtime aggregate storage".into());
                }
            }
        }
        Ok(())
    }
    body(program, module.id, &module.body, active, done)?;
    active.remove(&module.id);
    done.insert(module.id);
    Ok(())
}
pub fn expand(program: &mut Program, invocations: &[Invocation]) -> Result<()> {
    if invocations.is_empty() {
        return Ok(());
    }
    if invocations.len() > 128 {
        return Err("macro invocation budget exceeded".into());
    }
    let original = System::from_program(program.clone())?;
    let mut stage = Stage {
        cursor: program.next_id()?,
        draft: original.clone(),
        edits: vec![],
        allocated: 0,
        structural_remaining: MAX_ELEMENTS * 16,
        syntax: vec![],
    };
    let mut fuel = FUEL;
    let mut checked = BTreeSet::new();
    for invocation in invocations {
        let module = program
            .code_modules
            .iter()
            .find(|m| m.name == invocation.name)
            .ok_or_else(|| format!("{}: macro definition missing", invocation.name))?;
        if !module.compile_time
            || module.output != Type::Unit
            || module.output_ports
            || module.parameters.len() != invocation.args.len()
            || module.parameters.iter().any(|p| p.ty != Type::I64)
        {
            return Err(format!(
                "{}: expansion requires a macro with i64 literal arguments and unit result",
                invocation.name
            ));
        }
        stage.syntax.clear();
        stage.charge()?;
        pure_staging(program, module, &mut BTreeSet::new(), &mut checked)?;
        let mut stage_program = program.clone();
        // Runtime bodies may use contextual cells/hardware. They are not part of
        // the compile-time call graph and must not enter its unbound checker.
        // Keep lexical ancestors of reachable nested helpers. Their runtime
        // bodies are excluded, but their names still qualify the same tree.
        fn staging_modules(modules: &mut Vec<CodeModule>, checked: &BTreeSet<Id>) {
            modules.retain_mut(|module| {
                staging_modules(&mut module.locals, checked);
                if checked.contains(&module.id) {
                    return true;
                }
                if module.locals.is_empty() {
                    return false;
                }
                module.parameters.clear();
                module.output = Type::Unit;
                module.output_ports = false;
                module.annotated = true;
                module.compile_time = false;
                module.body = vec![Statement::Expr(Expr::Unit)];
                true
            });
        }
        staging_modules(&mut stage_program.code_modules, &checked);
        crate::behavior::execute_macro(
            &stage_program,
            module.id,
            &invocation.args,
            &mut stage,
            &mut fuel,
        )?;
    }
    let journal = Proposal {
        origin: Origin::System,
        explanation: "Source macro expansion".into(),
        revision: original.revision,
        label: "Source macro expansion".into(),
        edits: stage.edits,
    };
    let mut candidate = journal.prepare(&original)?;
    candidate.program.next_identity = candidate.program.next_identity.max(stage.cursor);
    if crate::inference::needed(&candidate.program) {
        crate::inference::analyze(&mut candidate.program, &[], true)?;
    }
    crate::composition::infer(&mut candidate.program)?;
    candidate.program.require_complete()?;
    candidate.validate()?;
    *program = candidate.program;
    Ok(())
}
