//! Canonical layout syntax over Mira Core.
use mira_core::*;

pub fn parse(source: &str) -> Result<Program> {
    if source.len() > 4 * 1024 * 1024 {
        return Err("source exceeds 4 MiB budget".into());
    }
    crate::frontend::parse(source)
}

/// Parse the same Program model while retaining explicit incomplete expressions.
/// Native compilation and normal execution still require the strict checker.
pub fn parse_draft(source: &str) -> Result<Program> {
    if source.len() > 4 * 1024 * 1024 { return Err("source exceeds 4 MiB".into()); }
    crate::frontend::parse_template(source)
}

pub fn format(program: &Program) -> String {
    use std::fmt::Write;
    fn space(program: &Program, s: &Space, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        let pos = |p: [f32; 3]| format!("at ({}, {}, {})", p[0], p[1], p[2]);
        writeln!(
            out,
            "{pad}space {} @{} {}:",
            crate::behavior::quote_identifier(&s.name),
            s.id,
            pos(s.position)
        )
        .unwrap();
        for (direction, ports) in [("in", &s.inputs), ("out", &s.outputs)] {
            for port in ports {
                writeln!(
                    out,
                    "{pad}  {direction} {}: {}",
                    crate::behavior::quote_identifier(&port.name),
                    crate::behavior::type_name(port.ty, &program.data_types())
                )
                .unwrap();
            }
        }
        for c in &s.cells {
            let owned = if let Definition::Code(id) = c.definition {
                code::find(&program.code_modules, id).filter(|d| d.owner == Some(c.id))
            } else { None };
            if let Some(definition) = owned {
                let mut body = definition.clone();
                body.name = "code".into();
                let text = crate::behavior::format_with_types(&body, &program.data_types());
                let mut lines = text.lines();
                writeln!(out, "{pad}  {} @{} {} = {}", crate::behavior::quote_identifier(&c.name), c.id, pos(c.position), lines.next().unwrap_or("")).unwrap();
                for line in lines { writeln!(out, "{pad}  {line}").unwrap(); }
                if let Some(inner) = &c.space { space(program, inner, out, depth + 2); }
                continue;
            }
            let behavior = behavior_in(program, c);
            writeln!(
                out,
                "{pad}  {} @{} = {} {}{}",
                crate::behavior::quote_identifier(&c.name),
                c.id,
                behavior,
                pos(c.position),
                if c.space.is_some() { ":" } else { "" }
            )
            .unwrap();
            if let Some(inner) = &c.space {
                space(program, inner, out, depth + 2);
            }
        }

        let endpoint_ports = |e: &Endpoint, direction: Direction| -> Vec<(String, Type)> {
            if e.cell == s.id {
                let boundary = if direction == Direction::Out {
                    &s.inputs
                } else {
                    &s.outputs
                };
                boundary
                    .iter()
                    .filter(|p| p.name == e.port)
                    .map(|p| (p.name.clone(), p.ty))
                    .collect()
            } else {
                s.cell(e.cell)
                    .map(|c| {
                        program
                            .ports(c)
                            .into_iter()
                            .filter(|p| p.direction == direction)
                            .map(|p| (p.name.into(), p.ty))
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        let base = |e: &Endpoint| -> String {
            if e.cell == s.id {
                crate::behavior::quote_identifier(&e.port)
            } else {
                s.cells
                    .iter()
                    .find(|c| c.id == e.cell)
                    .map_or("INVALID".into(), |c| {
                        crate::behavior::quote_identifier(&c.name)
                    })
            }
        };
        let arm = |l: &Link| -> (String, String) {
            let outputs = endpoint_ports(&l.from, Direction::Out);
            let inputs = endpoint_ports(&l.to, Direction::In);
            let pairs = outputs
                .iter()
                .flat_map(|a| {
                    inputs
                        .iter()
                        .filter(move |b| a.1 == b.1)
                        .map(move |b| (a.0.as_str(), b.0.as_str()))
                })
                .collect::<Vec<_>>();
            if pairs.len() == 1 {
                return (base(&l.from), base(&l.to));
            }
            let selected = outputs.iter().find(|p| p.0 == l.from.port).map(|p| p.1);
            let input_count = inputs.iter().filter(|p| Some(p.1) == selected).count();
            let target = if input_count == 1 || l.to.cell == s.id {
                base(&l.to)
            } else {
                format!(
                    "{}.{}",
                    base(&l.to),
                    crate::behavior::quote_identifier(&l.to.port)
                )
            };
            if outputs.len() > 1 {
                (
                    base(&l.from),
                    format!(
                        "{}:{target}",
                        crate::behavior::quote_identifier(&l.from.port)
                    ),
                )
            } else {
                (base(&l.from), target)
            }
        };
        let mut i = 0;
        while i < s.links.len() {
            let l = &s.links[i];
            let (source, target) = arm(l);
            let mut arms = vec![format!("{target} @{}", l.id)];
            let mut j = i + 1;
            if target.contains(':') {
                while j < s.links.len() {
                    let (next_source, next_target) = arm(&s.links[j]);
                    if next_source != source || !next_target.contains(':') {
                        break;
                    }
                    arms.push(format!("{next_target} @{}", s.links[j].id));
                    j += 1;
                }
            }
            writeln!(out, "{pad}  {source} -> {}", arms.join(", ")).unwrap();
            i = j;
        }
    }
    let mut out = "// Mira textual graph projection; @ identities survive renaming and movement.\n"
        .to_string();
    writeln!(out, "identities {}", program.next_identity).unwrap();
    for ty in &program.global_types {
        crate::behavior::format_data(ty, 0, true, &program.data_types(), &mut out);
    }
    for definition in program.code_modules.iter().filter(|d| d.owner.is_none()) {
        out.push_str(&crate::behavior::format_with_types(
            definition,
            &program.data_types(),
        ));
        out.push('\n');
    }
    space(program, &program.root, &mut out, 0);
    out
}

pub fn behavior(c: &Instance) -> String {
    match c.definition {
        Definition::Graph => "graph()".into(),
        Definition::Code(id) => format!("code_{id}()"),
        Definition::Adaptive(id) => format!("adaptive_{id}()"),
        Definition::Const => c.parameter.to_string(),
        Definition::Delay => format!("delay({})", c.parameter),
        Definition::Neural => format!("neural({})", c.parameter),
        Definition::ConstBool => {
            if c.parameter == 0 {
                "false".into()
            } else {
                "true".into()
            }
        }
        Definition::Branch(cmp, ty) => format!(
            "{}{}",
            cmp.symbol(),
            if ty == Type::Bool {
                if c.parameter == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            } else {
                c.parameter.to_string()
            }
        ),
        Definition::AsEvent(Type::Data(id)) => format!("as_event_data_{id}()"),
        Definition::AsEvent(ty) => format!(
            "as_event_{}()",
            match ty {
                Type::Infer => "_",
                Type::F32 => "f32",
                Type::I64 => "i64",
                Type::Bool => "bool",
                Type::Event => "event",
                Type::Unit => "unit",
                Type::Addr => "addr",
                Type::Data(_) => "data",
            }
        ),
        Definition::Add if c.parameter >= 0 => format!("+{}", c.parameter),
        Definition::Add => format!("add({})", c.parameter),
        Definition::Mul => format!("*{}", c.parameter),
    }
}

pub fn behavior_in(program: &Program, c: &Instance) -> String {
    if let Definition::Code(id) | Definition::Adaptive(id) = c.definition {
        mira_core::code::find(&program.code_modules, id)
            .map_or_else(
                || format!("missing #{id}"),
                |d| {
                    if matches!(c.definition, Definition::Adaptive(_)) {
                        format!("adaptive({})", crate::behavior::quote_identifier(&mira_core::code::path(&program.code_modules, d.id).expect("stored definition")))
                    } else {
                        format!("{}()", crate::behavior::quote_identifier(&mira_core::code::path(&program.code_modules, d.id).expect("stored definition")))
                    }
                },
            )
    } else {
        behavior(c)
    }
}
