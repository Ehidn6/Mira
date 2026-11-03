//! Bounded, revision-aware context for a human or assistant, over the real graph.
use mira_core::{system::System, *};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub revision: Option<u64>,
    pub tick: Option<u64>,
    pub space: Option<Id>,
    pub cell: Option<Id>,
    pub after: Option<Id>,
    pub limit: Option<usize>,
}
#[derive(Serialize)]
pub struct PortInfo {
    pub name: String,
    pub ty: String,
    pub direction: String,
}
#[derive(Serialize)]
pub struct CellInfo {
    pub id: Id,
    pub name: String,
    pub definition: String,
    pub behavior: Option<Id>,
    pub parameter: i64,
    pub ports: Vec<PortInfo>,
    pub incoming: Vec<Link>,
    pub outgoing: Vec<Link>,
    pub inner: Option<Id>,
    pub parameters: Option<mira_core::system::Parameters>,
    pub delay: Option<i64>,
}
#[derive(Serialize)]
pub struct Page {
    pub children: Vec<(Id, String)>,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    pub revision: u64,
    pub tick: u64,
    pub space: Id,
    pub cells: Vec<CellInfo>,
    pub next_after: Option<Id>,
    pub protected: bool,
}
pub fn page(system: &System, request: &Request) -> Result<Page> {
    if request.revision.is_some_and(|r| r != system.revision)
        || request.tick.is_some_and(|t| t != system.tick)
    {
        return Err("context revision/tick changed; restart the snapshot".into());
    }
    let e = system;
    let space = e
        .program
        .root
        .find(request.space.unwrap_or(e.program.root.id))
        .ok_or("context space missing")?;
    let limit = request.limit.unwrap_or(32).clamp(1, 128);
    if request
        .cell
        .is_some_and(|id| !space.cells.iter().any(|c| c.id == id))
    {
        return Err("context cell is outside this graph".into());
    }
    let mut selected: Vec<_> = space
        .cells
        .iter()
        .filter(|c| {
            request.cell.is_none_or(|id| c.id == id)
                && request.after.is_none_or(|after| c.id > after)
        })
        .collect();
    selected.sort_by_key(|c| c.id);
    let more = selected.len() > limit;
    selected.truncate(limit);
    let cells: Vec<_> = selected
        .into_iter()
        .map(|c| CellInfo {
            id: c.id,
            name: c.name.clone(),
            definition: crate::syntax::behavior_in(&e.program, c),
            behavior: match c.definition {
                Definition::Code(id) | Definition::Adaptive(id) => Some(id),
                _ => None,
            },
            parameter: c.parameter,
            ports: e
                .program
                .ports(c)
                .iter()
                .map(|p| PortInfo {
                    name: p.name.into(),
                    ty: crate::behavior::type_name(p.ty, &e.program.data_types()),
                    direction: if p.direction == Direction::In {
                        "in"
                    } else {
                        "out"
                    }
                    .into(),
                })
                .collect(),
            incoming: space
                .links
                .iter()
                .filter(|l| l.to.cell == c.id)
                .cloned()
                .collect(),
            outgoing: space
                .links
                .iter()
                .filter(|l| l.from.cell == c.id)
                .cloned()
                .collect(),
            inner: c.space.as_ref().map(|s| s.id),
            parameters: if matches!(c.definition, Definition::Neural | Definition::Adaptive(_)) {
                Some(e.parameters.get(&c.id).cloned().unwrap_or_default())
            } else {
                None
            },
            delay: if c.definition == Definition::Delay {
                Some(e.delays.get(&c.id).copied().unwrap_or(c.parameter))
            } else {
                None
            },
        })
        .collect();
    // Wide fan-out is explicit: refuse oversized responses rather than silently hiding dependencies.
    let interfaces = |ports: &[code::Parameter], direction: &str| {
        ports
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                ty: crate::behavior::type_name(p.ty, &e.program.data_types()),
                direction: direction.into(),
            })
            .collect()
    };
    let page = Page {
        children: space
            .cells
            .iter()
            .filter_map(|c| c.space.as_ref().map(|g| (g.id, g.name.clone())))
            .collect(),
        inputs: interfaces(&space.inputs, "in"),
        outputs: interfaces(&space.outputs, "out"),
        revision: system.revision,
        tick: system.tick,
        space: space.id,
        next_after: if more {
            cells.last().map(|c| c.id)
        } else {
            None
        },
        cells,
        protected: e.protected.contains(&space.id),
    };
    if serde_json::to_vec(&page).map_err(|e| e.to_string())?.len() > 256 * 1024 {
        return Err("context page exceeds 256 KiB; request a smaller scope".into());
    }
    Ok(page)
}
#[derive(Serialize)]
pub struct BehaviorInfo {
    pub revision: u64,
    pub id: Id,
    pub hash: String,
    pub unchanged: bool,
    pub definition: Option<code::CodeModule>,
}
pub fn behavior(
    system: &System,
    id: Id,
    revision: u64,
    known: Option<&str>,
) -> Result<BehaviorInfo> {
    if revision != system.revision {
        return Err("context revision changed".into());
    }
    let e = system;
    fn find(ms: &[code::CodeModule], id: Id) -> Option<&code::CodeModule> {
        ms.iter()
            .find(|m| m.id == id)
            .or_else(|| ms.iter().find_map(|m| find(&m.locals, id)))
    }
    let module = find(&e.program.code_modules, id).ok_or("context behavior missing")?;
    let bytes = serde_json::to_vec(module).map_err(|e| e.to_string())?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let unchanged = known == Some(hash.as_str());
    if !unchanged && bytes.len() > 256 * 1024 {
        return Err(
            "behavior context exceeds 256 KiB; use the existing paginated inspect commands".into(),
        );
    }
    Ok(BehaviorInfo {
        revision,
        id,
        hash,
        unchanged,
        definition: if unchanged {
            None
        } else {
            Some(module.clone())
        },
    })
}
