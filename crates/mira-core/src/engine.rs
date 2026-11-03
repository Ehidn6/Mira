//! Bounded deterministic tick scheduler, usable in the kernel without an OS service.
use crate::{
    neural::{Activation, LearningRule},
    system::*,
    *,
};

pub trait BehaviorHost {
    /// The implementation must bound instructions and expose no ambient I/O authority.
    fn call(
        &mut self,
        program: &Program,
        behavior: Id,
        cell: Option<Id>,
        inputs: &[i64],
    ) -> Result<i64>;
    /// A host may validate activation-owned typed handles; the portable default
    /// accepts only scalar values and never treats a word as a native pointer.
    fn validate_value(&self, _program: &Program, ty: Type, value: i64) -> Result<()> {
        scalar(ty, value)
    }
    fn extract_outputs(
        &self,
        program: &Program,
        module: &code::CodeModule,
        value: i64,
    ) -> Result<BTreeMap<String, i64>> {
        if module.output_ports {
            return Err("named output records require typed activation storage".into());
        }
        let mut outputs = BTreeMap::new();
        if module.output != Type::Unit {
            self.validate_value(program, module.output, value)?;
            outputs.insert("out".into(), value);
        }
        Ok(outputs)
    }
    fn charge(&mut self) -> Result<()> {
        Ok(())
    }
    fn begin_tick(&mut self, _system: &System) -> Result<()> {
        Ok(())
    }
    fn commit_tick(&mut self, _system: &mut System) -> Result<()> {
        Ok(())
    }
    /// Bind contextual cells/graph calls without giving behavior ambient authority.
    fn call_bound(
        &mut self,
        program: &Program,
        behavior: Id,
        cell: Option<Id>,
        inputs: &[i64],
        _context: &mut dyn Calls,
    ) -> Result<i64> {
        self.call(program, behavior, cell, inputs)
    }
}
pub trait Calls {
    fn begin_scope(&mut self) -> Result<()> {
        Ok(())
    }
    fn end_scope(&mut self, _commit: bool, _read_only: bool) -> Result<()> {
        Ok(())
    }
    fn invoke(&mut self, host: &mut dyn BehaviorHost, name: &str, inputs: &[i64]) -> Result<i64>;
}
pub struct Detached;
impl Calls for Detached {
    fn invoke(
        &mut self,
        _host: &mut dyn BehaviorHost,
        _name: &str,
        _inputs: &[i64],
    ) -> Result<i64> {
        Err("contextual call requires a cell activation".into())
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickReport {
    pub tick: u64,
    pub deliveries: BTreeMap<(Id, String), i64>,
    pub outputs: BTreeMap<(Id, String), i64>,
    pub trained: usize,
}
fn quantize(v: f64, scale: i64) -> Result<i64> {
    let v = libm::round(v * scale as f64);
    if !v.is_finite() || v < i64::MIN as f64 || v >= 9223372036854775808.0 {
        return Err("neural value exceeds fixed-point range".into());
    }
    Ok(v as i64)
}
fn activate(a: Activation, x: f64) -> (f64, f64) {
    match a {
        Activation::Identity => (x, 1.),
        Activation::Tanh => {
            let y = libm::tanh(x);
            (y, 1. - y * y)
        }
        Activation::Sigmoid => {
            let y = if x >= 0. {
                1. / (1. + libm::exp(-x))
            } else {
                let e = libm::exp(x);
                e / (1. + e)
            };
            (y, y * (1. - y))
        }
        Activation::Relu => (x.max(0.), if x > 0. { 1. } else { 0. }),
        Activation::Gelu => {
            let t = libm::tanh(0.7978845608028654 * (x + 0.044715 * x * x * x));
            (
                0.5 * x * (1. + t),
                0.5 * (1. + t)
                    + 0.5 * x * (1. - t * t) * 0.7978845608028654 * (1. + 0.134145 * x * x),
            )
        }
    }
}
fn neural(p: &mut Parameters, cell: &Instance, x: i64, target: Option<i64>) -> Result<i64> {
    let scale = cell.parameter;
    let real = x as f64 / scale as f64;
    let (y, derivative) = activate(p.activation, p.weight * real + p.bias);
    let output = quantize(y, scale)?;
    p.state = output;
    if let Some(target) = target.filter(|_| p.trainable) {
        let delta = match p.learning {
            LearningRule::None => 0.,
            LearningRule::Gradient => (target as f64 / scale as f64 - y) * derivative * real,
            LearningRule::Hebbian => real * y,
            LearningRule::Oja => y * (real - y * p.weight),
            LearningRule::RewardModulated => target as f64 / scale as f64 * real * y,
        };
        p.weight += p.rate * delta;
        if p.learning == LearningRule::Gradient {
            p.bias += p.rate * (target as f64 / scale as f64 - y) * derivative;
        }
    }
    if !p.weight.is_finite() || !p.bias.is_finite() {
        return Err("nonfinite learned parameter".into());
    }
    Ok(output)
}
fn scalar(ty: Type, value: i64) -> Result<()> {
    if ty == Type::F32 {
        crate::vm::word_f32(value)?;
    }
    if ty == Type::Bool && !matches!(value, 0 | 1)
        || ty == Type::Event && value != 1
        || matches!(ty, Type::Data(_) | Type::Addr)
    {
        return Err("invalid scalar or activation-local value in system runtime".into());
    }
    Ok(())
}
type Signals = BTreeMap<(Id, String), i64>;
type Outputs = BTreeMap<String, i64>;

/// Activation state is temporary. Delays observe the same old snapshot throughout
/// the tick; weights/state of a neural instance are updated at most once.
struct Execution<'a> {
    program: &'a Program,
    parameters: &'a mut BTreeMap<Id, Parameters>,
    delays: &'a BTreeMap<Id, i64>,
    writes: BTreeMap<Id, i64>,
    targets: &'a BTreeMap<Id, i64>,
    report: &'a mut TickReport,
    neural_cache: BTreeMap<Id, (Vec<i64>, Outputs)>,
    last_record: Option<(Id, i64)>,
    active: BTreeSet<Id>,
    remaining: usize,
}
impl Execution<'_> {
    fn spend(&mut self) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("graph activation budget exhausted")?;
        Ok(())
    }
    fn behavior(
        &mut self,
        owner: Id,
        module: Id,
        args: &[i64],
        host: &mut dyn BehaviorHost,
    ) -> Result<i64> {
        let program = self.program;
        host.call_bound(
            program,
            module,
            Some(owner),
            args,
            &mut Bound {
                execution: self,
                owner,
                scopes: Vec::new(),
            },
        )
    }
    fn delay_write(&mut self, id: Id, value: i64) -> Result<()> {
        if self.writes.get(&id).is_some_and(|old| *old != value) {
            return Err(format!("delay #{id}: conflicting writes in one tick"));
        }
        self.writes.insert(id, value);
        Ok(())
    }
    fn cell(&mut self, c: &Instance, args: &[i64], host: &mut dyn BehaviorHost) -> Result<Outputs> {
        self.spend()?;
        host.charge()?;
        if self.active.len() >= MAX_DEPTH || !self.active.insert(c.id) {
            return Err("recursive cell activation/depth budget".into());
        }
        let result = self.cell_inner(c, args, host);
        self.active.remove(&c.id);
        result
    }
    fn cell_inner(
        &mut self,
        c: &Instance,
        args: &[i64],
        host: &mut dyn BehaviorHost,
    ) -> Result<Outputs> {
        let ports = self.program.ports(c);
        let inputs: Vec<_> = ports
            .iter()
            .filter(|p| p.direction == Direction::In)
            .collect();
        if inputs.len() != args.len() {
            return Err(format!("{}: activation arity mismatch", c.name));
        }
        for (port, value) in inputs.iter().zip(args) {
            host.validate_value(self.program, port.ty, *value)?;
        }
        if c.space.is_some() && matches!(c.definition, Definition::Delay | Definition::Neural) {
            return Err("intrinsic delay/neural cells cannot execute an owned body; use a Code or Adaptive owner".into());
        }
        if let Some((previous, result)) = self.neural_cache.get(&c.id) {
            if previous != args {
                return Err(format!(
                    "stateful cell #{}: conflicting inputs in one tick",
                    c.id
                ));
            }
            for (name, value) in result {
                let port = ports
                    .iter()
                    .find(|p| p.direction == Direction::Out && p.name == name)
                    .ok_or("cached output interface changed")?;
                host.validate_value(self.program, port.ty, *value)?;
            }
            return Ok(result.clone());
        }
        if c.definition == Definition::Graph {
            let graph = c.space.as_ref().ok_or("graph cell has no Z body")?;
            let values = self.graph(graph, args, None, host)?;
            let mut result = Outputs::new();
            for output in &graph.outputs {
                let link = graph
                    .links
                    .iter()
                    .find(|l| l.to.cell == graph.id && l.to.port == output.name)
                    .ok_or("graph output producer missing")?;
                if let Some(value) = values.get(&(link.from.cell, link.from.port.clone())) {
                    result.insert(output.name.clone(), *value);
                    self.report
                        .deliveries
                        .insert((c.id, output.name.clone()), *value);
                }
            }
            return Ok(result);
        }
        let x = args.first().copied().unwrap_or(0);
        let (port, value) = match c.definition {
            Definition::Graph => return Err("graph dispatch invariant".into()),
            Definition::Const | Definition::ConstBool => ("out", c.parameter),
            Definition::Add => (
                "out",
                x.checked_add(c.parameter)
                    .ok_or("exact addition overflow")?,
            ),
            Definition::Mul => (
                "out",
                x.checked_mul(c.parameter)
                    .ok_or("exact multiplication overflow")?,
            ),
            Definition::Branch(cmp, _) => (
                if cmp.test(x, c.parameter) {
                    "yes"
                } else {
                    "no"
                },
                x,
            ),
            Definition::AsEvent(_) => ("out", 1),
            Definition::Delay => {
                self.delay_write(c.id, x)?;
                ("out", *self.delays.get(&c.id).unwrap_or(&c.parameter))
            }
            Definition::Neural => {
                let p = self.parameters.entry(c.id).or_default();
                let target = self.targets.get(&c.id).copied();
                let y = neural(p, c, x, target)?;
                if target.is_some() && p.trainable {
                    self.report.trained += 1;
                }
                ("out", y)
            }
            Definition::Adaptive(behavior) => {
                let module = crate::code::find(&self.program.code_modules, behavior)
                    .ok_or("adaptive type missing")?;
                let old = self.parameters.get(&c.id).cloned().unwrap_or_default();
                let mut p = old.clone();
                let mut named = BTreeMap::new();
                let mut external = args.iter();
                for input in &module.parameters {
                    let value =
                        if input.name.starts_with("state_") || input.name.starts_with("param_") {
                            old.slots.get(&input.name).copied().unwrap_or(0)
                        } else {
                            *external.next().ok_or("adaptive input missing")?
                        };
                    host.validate_value(self.program, input.ty, value)?;
                    named.insert(input.name.clone(), value);
                }
                let args: Vec<_> = module.parameters.iter().map(|p| named[&p.name]).collect();
                let y = self.behavior(c.id, behavior, &args, host)?;
                named.insert("result".into(), y);
                let target = self
                    .targets
                    .get(&c.id)
                    .copied()
                    .or_else(|| named.get("target").copied());
                if let Some(target) = target {
                    host.validate_value(self.program, module.output, target)?;
                    named.insert("target".into(), target);
                }
                for (slot, update) in &old.updates {
                    if slot.starts_with("param_") && (target.is_none() || !old.trainable) {
                        continue;
                    }
                    let update = crate::code::find(&self.program.code_modules, *update)
                        .ok_or("adaptive update missing")?;
                    let args: Vec<_> = update
                        .parameters
                        .iter()
                        .map(|p| {
                            named
                                .get(&p.name)
                                .copied()
                                .ok_or_else(|| String::from("update requires a training target"))
                        })
                        .collect::<Result<_>>()?;
                    let value = self.behavior(c.id, update.id, &args, host)?;
                    host.validate_value(self.program, update.output, value)?;
                    p.slots.insert(slot.clone(), value);
                }
                if target.is_some() && p.trainable {
                    self.report.trained += 1;
                }
                self.parameters.insert(c.id, p);
                ("out", y)
            }
            Definition::Code(module) => {
                let value = self.behavior(c.id, module, args, host)?;
                let module = crate::code::find(&self.program.code_modules, module)
                    .ok_or("code result definition missing")?;
                let result = host.extract_outputs(self.program, module, value)?;
                for (name, value) in &result {
                    let port = ports
                        .iter()
                        .find(|p| p.direction == Direction::Out && p.name == name)
                        .ok_or("code produced an undeclared output port")?;
                    host.validate_value(self.program, port.ty, *value)?;
                    self.report.deliveries.insert((c.id, name.clone()), *value);
                }
                if module.output_ports {
                    self.last_record = Some((c.id, value));
                }
                return Ok(result);
            }
        };
        let mut result = Outputs::new();
        if let Some(p) = ports
            .iter()
            .find(|p| p.direction == Direction::Out && p.name == port)
        {
            host.validate_value(self.program, p.ty, value)?;
            result.insert(port.into(), value);
        }
        for (port, value) in &result {
            self.report.deliveries.insert((c.id, port.clone()), *value);
        }
        if matches!(c.definition, Definition::Neural | Definition::Adaptive(_)) {
            self.neural_cache
                .insert(c.id, (args.to_vec(), result.clone()));
        }
        Ok(result)
    }
    /// Root activation evaluates all root cells. An owned graph evaluates only
    /// dependencies of the explicitly requested output, including next-state work.
    fn graph(
        &mut self,
        s: &Space,
        args: &[i64],
        requested: Option<&str>,
        host: &mut dyn BehaviorHost,
    ) -> Result<Signals> {
        self.spend()?;
        host.charge()?;
        if args.len() != s.inputs.len() {
            return Err("graph activation arity mismatch".into());
        }
        let mut values = Signals::new();
        for (p, value) in s.inputs.iter().zip(args) {
            host.validate_value(self.program, p.ty, *value)?;
            values.insert((s.id, p.name.clone()), *value);
        }
        self.graph_values(s, values, requested, host)
    }
    fn graph_values(
        &mut self,
        s: &Space,
        mut values: Signals,
        requested: Option<&str>,
        host: &mut dyn BehaviorHost,
    ) -> Result<Signals> {
        let mut needed = BTreeSet::new();
        if let Some(output) = requested {
            if !s.outputs.iter().any(|p| p.name == output) {
                return Err("graph output missing".into());
            }
            let mut pending: Vec<_> = s
                .links
                .iter()
                .filter(|l| l.to.cell == s.id && l.to.port == output)
                .map(|l| l.from.cell)
                .collect();
            while let Some(id) = pending.pop() {
                if id == s.id || !needed.insert(id) {
                    continue;
                }
                s.cell(id)?;
                pending.extend(
                    s.links
                        .iter()
                        .filter(|l| l.to.cell == id)
                        .map(|l| l.from.cell),
                );
            }
        } else {
            needed.extend(s.cells.iter().map(|c| c.id));
        }
        let links: BTreeMap<_, _> = s
            .links
            .iter()
            .filter(|l| l.to.cell != s.id)
            .map(|l| ((l.to.cell, l.to.port.as_str()), l))
            .collect();
        for c in s
            .cells
            .iter()
            .filter(|c| needed.contains(&c.id) && c.definition == Definition::Delay)
        {
            host.charge()?;
            if c.space.is_some() {
                return Err("intrinsic delay/neural cells cannot execute an owned body; use a Code or Adaptive owner".into());
            }
            values.insert(
                (c.id, "out".into()),
                *self.delays.get(&c.id).unwrap_or(&c.parameter),
            );
        }
        for id in s.order()? {
            if !needed.contains(&id) {
                continue;
            }
            let c = s.cell(id)?;
            if c.definition == Definition::Delay {
                continue;
            }
            let mut args = Vec::new();
            let mut ready = true;
            for port in self
                .program
                .ports(c)
                .iter()
                .filter(|p| p.direction == Direction::In)
            {
                let link = links
                    .get(&(id, port.name))
                    .ok_or_else(|| format!("{}.{}: missing input", c.name, port.name))?;
                if let Some(value) = values.get(&(link.from.cell, link.from.port.clone())) {
                    args.push(*value);
                } else {
                    ready = false;
                    break;
                }
            }
            if ready {
                for (port, value) in self.cell(c, &args, host)? {
                    values.insert((id, port), value);
                }
            }
        }
        for c in s
            .cells
            .iter()
            .filter(|c| needed.contains(&c.id) && c.definition == Definition::Delay)
        {
            let link = links.get(&(c.id, "in")).ok_or("delay input missing")?;
            if let Some(value) = values.get(&(link.from.cell, link.from.port.clone())) {
                self.delay_write(c.id, *value)?;
            }
        }
        self.report.deliveries.extend(values.clone());
        Ok(values)
    }
}
struct ScopeSnapshot {
    parameters: BTreeMap<Id, Parameters>,
    writes: BTreeMap<Id, i64>,
    cache: BTreeMap<Id, (Vec<i64>, Outputs)>,
    report: TickReport,
    last_record: Option<(Id, i64)>,
}
struct Bound<'a, 'b> {
    execution: &'a mut Execution<'b>,
    owner: Id,
    scopes: Vec<ScopeSnapshot>,
}
impl Calls for Bound<'_, '_> {
    fn begin_scope(&mut self) -> Result<()> {
        let e = &self.execution;
        self.scopes.push(ScopeSnapshot {
            parameters: e.parameters.clone(),
            writes: e.writes.clone(),
            cache: e.neural_cache.clone(),
            report: e.report.clone(),
            last_record: e.last_record,
        });
        Ok(())
    }
    fn end_scope(&mut self, commit: bool, read_only: bool) -> Result<()> {
        let old = self.scopes.pop().ok_or("unmatched execution scope")?;
        let e = &mut self.execution;
        let forbidden = read_only && (old.parameters != *e.parameters || old.writes != e.writes);
        // Both cached inputs and outputs may carry activation-local handles.
        // A successful scratch/neural scope will reclaim their storage. Reject
        // and roll back newly cached aggregate activations rather than dropping
        // the cache and accidentally applying their state updates a second time.
        // Entries already present at scope entry belong to the enclosing arena.
        let escaping_cache = commit
            && e.neural_cache.iter().any(|(id, cached)| {
                old.cache.get(id) != Some(cached)
                    && e.program.root.find_cell(*id).is_some_and(|cell| {
                        e.program
                            .ports(cell)
                            .iter()
                            .any(|port| matches!(port.ty, Type::Data(_) | Type::Addr))
                    })
            });
        if !commit || forbidden || escaping_cache {
            *e.parameters = old.parameters;
            e.writes = old.writes;
            e.neural_cache = old.cache;
        }
        // Scoped aggregate handles are reclaimed by the behavior machine. Its
        // internal deliveries must therefore never survive in the public report.
        e.last_record = old.last_record;
        let trained = e.report.trained;
        *e.report = old.report;
        if commit && !forbidden && !escaping_cache {
            e.report.trained = trained;
        }
        if forbidden {
            return Err("read_only scope attempted a stateful graph update".into());
        }
        if escaping_cache {
            return Err(
                "stateful aggregate activation cannot outlive its scratch or neural scope".into(),
            );
        }
        Ok(())
    }
    fn invoke(&mut self, host: &mut dyn BehaviorHost, name: &str, args: &[i64]) -> Result<i64> {
        let e = &mut self.execution;
        let owner = e
            .program
            .root
            .find_cell(self.owner)
            .ok_or("context owner missing")?;
        if name.starts_with("cells.") || name.starts_with("#cell.") {
            let c = if let Some(id) = name.strip_prefix("#cell.") {
                let id: Id = id.parse().map_err(|_| "invalid contextual cell identity")?;
                e.program.visible_cell(self.owner, id)
            } else {
                let name = name.strip_prefix("cells.").unwrap();
                let (cell, method) = e.program.component_target(self.owner, name).ok_or("component operation missing")?;
                if let Some(method) = method { return e.behavior(cell.id, method.id, args, host); }
                Some(cell)
            }
            .ok_or("child cell missing in owned Z graph")?;
            let ports = e.program.ports(c);
            let outputs: Vec<_> = ports
                .iter()
                .filter(|p| p.direction == Direction::Out)
                .collect();
            if let Definition::Code(id) = c.definition {
                if crate::code::find(&e.program.code_modules, id)
                    .is_some_and(|m| m.output_ports)
                {
                    e.cell(c, args, host)?;
                    return e
                        .last_record
                        .filter(|(id, _)| *id == c.id)
                        .map(|(_, value)| value)
                        .ok_or_else(|| {
                            String::from("named result aggregate was not retained for its call")
                        });
                }
            }
            if outputs.is_empty()
                && matches!(c.definition, Definition::Code(id) if crate::code::find(&e.program.code_modules, id).is_some_and(|m| m.output == Type::Unit))
            {
                e.cell(c, args, host)?;
                return Ok(0); // Unit has no delivered signal.
            }
            if outputs.len() != 1 || outputs[0].name != "out" {
                return Err("a code child call requires one result port named out".into());
            }
            return e
                .cell(c, args, host)?
                .get("out")
                .copied()
                .ok_or_else(|| String::from("child result not delivered"));
        }
        let requested = if let Some(rest) = name.strip_prefix("#graph.") {
            let (id, output) = rest
                .split_once('.')
                .ok_or("invalid contextual graph identity")?;
            let id: Id = id
                .parse()
                .map_err(|_| "invalid contextual graph identity")?;
            let mut context = Some(owner);
            let mut graph = None;
            while let Some(c) = context {
                graph = c.space.as_ref().and_then(|s| s.find(id));
                if graph.is_some() { break; }
                context = e.program.parent_cell(c.id);
            }
            let graph = graph.ok_or("called graph is outside the owning interface")?;
            Some((graph, output))
        } else {
            name.strip_prefix("graph.").and_then(|output| e.program.relative_scope(self.owner, output))
        };
        if let Some((s, output)) = requested {
            let values = e.graph(s, args, Some(output), host)?;
            let active: Vec<_> = s
                .links
                .iter()
                .filter(|l| l.to.cell == s.id && l.to.port == output)
                .filter_map(|l| values.get(&(l.from.cell, l.from.port.clone())).copied())
                .collect();
            if active.len() != 1 {
                return Err(format!(
                    "graph.{output}: expected exactly one active producer, got {}",
                    active.len()
                ));
            }
            let value = active[0];
            e.report.deliveries.insert((s.id, output.into()), value);
            return Ok(value);
        }
        Err("unknown contextual operation".into())
    }
}
fn activate_program(
    e: &mut System,
    incoming: &BTreeMap<(Id, String), i64>,
    targets: &BTreeMap<Id, i64>,
    report: &mut TickReport,
    host: &mut dyn BehaviorHost,
) -> Result<()> {
    let mut execution = Execution {
        program: &e.program,
        parameters: &mut e.parameters,
        delays: &e.delays,
        writes: BTreeMap::new(),
        targets,
        report,
        neural_cache: BTreeMap::new(),
        last_record: None,
        active: BTreeSet::new(),
        remaining: MAX_ELEMENTS * 4,
    };
    let s = &e.program.root;
    let mut initial = Signals::new();
    for p in &s.inputs {
        if let Some(value) = incoming.get(&(e.program.root.id, p.name.clone())) {
            host.validate_value(&e.program, p.ty, *value)?;
            initial.insert((s.id, p.name.clone()), *value);
        }
    }
    let values = execution.graph_values(s, initial, None, host)?;
    for p in &s.outputs {
        if let Some(link) = s
            .links
            .iter()
            .find(|l| l.to.cell == s.id && l.to.port == p.name)
        {
            if let Some(value) = values.get(&(link.from.cell, link.from.port.clone())) {
                execution
                    .report
                    .outputs
                    .insert((e.program.root.id, p.name.clone()), *value);
            }
        } else {
            return Err("graph output missing".into());
        }
    }
    if s.outputs.is_empty() {
        for c in &s.cells {
            for p in e
                .program
                .ports(c)
                .iter()
                .filter(|p| p.direction == Direction::Out)
            {
                if !s
                    .links
                    .iter()
                    .any(|l| l.from.cell == c.id && l.from.port == p.name)
                {
                    if let Some(value) = values.get(&(c.id, p.name.into())) {
                        execution.report.outputs.insert(
                            (e.program.root.id, format!("{}.{}", c.name, p.name)),
                            *value,
                        );
                    }
                }
            }
        }
    }
    e.delays.extend(execution.writes);
    Ok(())
}
/// One bounded tick. Failure rolls back every state, mailbox, and parameter write.
pub fn tick(
    system: &mut System,
    inputs: &BTreeMap<(Id, String), i64>,
    targets: &BTreeMap<Id, i64>,
    host: &mut impl BehaviorHost,
) -> Result<TickReport> {
    system.validate()?;
    system.program.require_complete()?;
    host.begin_tick(system)?;
    let mut draft = system.clone();
    let mut incoming = core::mem::take(&mut draft.pending);
    for (k, v) in inputs {
        if incoming.insert(k.clone(), *v).is_some() {
            return Err("external input conflicts with routed signal".into());
        }
    }
    draft.pending = incoming.clone();
    draft.validate()?;
    draft.pending.clear();
    for id in targets.keys() {
        if !draft
            .program
            .root
            .find_cell(*id)
            .is_some_and(|c| matches!(c.definition, Definition::Neural | Definition::Adaptive(_)))
        {
            return Err("training target is not an executable neural cell".into());
        }
    }
    let mut report = TickReport {
        tick: system.tick.checked_add(1).ok_or("tick exhausted")?,
        ..Default::default()
    };
    activate_program(&mut draft, &incoming, targets, &mut report, host)?;
    draft.tick = report.tick;
    host.commit_tick(&mut draft)?;
    draft.validate()?;
    *system = draft;
    Ok(report)
}
