//! Bounded, read-only program queries and content-addressed context exports.
use crate::{behavior, composition, syntax};
use mira_core::{
    code::CodeModule,
    inspect::{CellInfo, SpaceInfo},
    *,
};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Write,
    path::Path,
};

pub const SCHEMA: &str = "mira.inspect/1";
#[derive(Clone)]
pub struct Options {
    pub limit: usize,
    pub budget: usize,
    pub after: Id,
    pub line: usize,
    pub lines: usize,
    pub known: Option<String>,
    pub text: bool,
    pub revision: Option<String>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            limit: 20,
            budget: 16_384,
            after: 0,
            line: 1,
            lines: 60,
            known: None,
            text: false,
            revision: None,
        }
    }
}
impl Options {
    fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.limit)
            || !(512..=65_536).contains(&self.budget)
            || !(1..=200).contains(&self.lines)
            || self.line == 0
        {
            return Err(
                "limit: 1..100; budget: 512..65536 UTF-8 bytes; line >= 1; lines: 1..200".into(),
            );
        }
        Ok(())
    }
}
fn hash(bytes: impl AsRef<[u8]>) -> String {
    blake3::hash(bytes.as_ref()).to_hex().to_string()
}
/// The same canonical revision is exposed to native Mira reflection and Inspect.
pub(crate) fn revision_digest(program: &Program) -> blake3::Hash {
    blake3::hash(syntax::format(program).as_bytes())
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON values serialize")
}
fn module_list<'a>(defs: &'a [CodeModule], out: &mut BTreeMap<Id, &'a CodeModule>) {
    for d in defs {
        out.insert(d.id, d);
        module_list(&d.locals, out);
    }
}
pub struct Snapshot<'a> {
    program: &'a Program,
    cells: Vec<CellInfo<'a>>,
    spaces: Vec<SpaceInfo<'a>>,
    references: composition::References,
    modules: BTreeMap<Id, &'a CodeModule>,
    module_parents: BTreeMap<Id, Id>,
    types: Vec<data::Definition>,
    sources: BTreeMap<Id, (String, String)>,
    pub revision: String,
}
impl<'a> Snapshot<'a> {
    pub fn new(program: &'a Program) -> Result<Self> {
        program.validate()?;
        let mut modules = BTreeMap::new();
        module_list(&program.code_modules, &mut modules);
        let types = program.data_types();
        let module_parents = modules
            .values()
            .flat_map(|d| d.locals.iter().map(|c| (c.id, d.id)))
            .collect();
        let sources = modules
            .iter()
            .map(|(id, d)| {
                let text = behavior::format_with_types(d, &types);
                let digest = hash(&text);
                (*id, (text, digest))
            })
            .collect();
        Ok(Self {
            sources,
            module_parents,
            program,
            cells: mira_core::inspect::cells(program),
            spaces: mira_core::inspect::spaces(program),
            references: composition::references(program)?,
            modules,
            types,
            revision: revision_digest(program).to_hex().to_string(),
        })
    }
    fn ty(&self, ty: Type) -> Value {
        match ty {
            Type::Data(id) => json!({"id":id,"name":behavior::type_name(ty,&self.types)}),
            _ => json!(behavior::type_name(ty, &self.types)),
        }
    }
    fn ports(&self, c: &CellInfo<'_>) -> Value {
        json!(
            c.ports
                .iter()
                .map(|p| json!([
                    if p.direction == Direction::In {
                        "in"
                    } else {
                        "out"
                    },
                    p.name,
                    self.ty(p.ty)
                ]))
                .collect::<Vec<_>>()
        )
    }
    fn cell(&self, selector: &str) -> Result<&CellInfo<'a>> {
        let numeric = selector.trim_start_matches('#').parse::<Id>().ok();
        let found: Vec<_> = self
            .cells
            .iter()
            .filter(|i| numeric == Some(i.cell.id) || i.path == selector || i.cell.name == selector)
            .collect();
        match found.as_slice() {
            [c] => Ok(c),
            [] => Err(format!("cell {selector} not found")),
            _ => Err("ambiguous cell name; use its ID or full path".into()),
        }
    }
    fn source(&self, c: &CellInfo<'_>) -> Cow<'_, str> {
        if let Definition::Code(id) | Definition::Adaptive(id) = c.cell.definition {
            Cow::Borrowed(&self.sources[&id].0)
        } else {
            Cow::Owned(format!(
                "{} = {}\n",
                c.cell.name,
                syntax::behavior_in(self.program, c.cell)
            ))
        }
    }
    fn source_hash(&self, c: &CellInfo<'_>) -> String {
        if let Definition::Code(id) | Definition::Adaptive(id) = c.cell.definition {
            self.sources[&id].1.clone()
        } else {
            hash(self.source(c).as_bytes())
        }
    }
    fn module_info(&self, id: Id) -> Value {
        let d = self.modules[&id];
        json!({"id":id,"name":d.name,"in":d.parameters.iter().map(|p| json!([p.name,self.ty(p.ty)])).collect::<Vec<_>>(),"out":self.ty(d.output),
            "hash":self.sources[&id].1,"helpers":d.locals.iter().map(|m|json!({"id":m.id,"name":m.name})).collect::<Vec<_>>(),"types":d.data_types.iter().map(|t|t.id).collect::<Vec<_>>()})
    }
    fn summary(&self, c: &CellInfo<'_>) -> Value {
        json!({"id":c.cell.id,"name":c.cell.name,"path":c.path,"space":c.space.id,"owner":c.owner,
            "ports":self.ports(c),"z":c.cell.space.as_ref().map(|s|s.id),
            "code":match c.cell.definition {Definition::Code(id)|Definition::Adaptive(id)=>Some(id),_=>None},"source_hash":self.source_hash(c)})
    }
    fn graphs(&self, id: Id) -> Vec<Value> {
        self.references.graphs.get(&id).into_iter().flatten().map(|name| {
            let s = self.program.root.find_cell(id).unwrap().space.as_ref().unwrap();
            json!({"space":s.id,"output":name,"cells":mira_core::inspect::upstream(self.program,s.id,name).expect("bound graph interface")})
        }).collect()
    }
    fn record(&self, c: &CellInfo<'_>) -> Value {
        let mut v = self.summary(c);
        v["z_hash"] = c
            .cell
            .space
            .as_ref()
            .map(|s| {
                let info = self.spaces.iter().find(|i| i.space.id == s.id).unwrap();
                json!(hash(bytes(&self.space_info(info, true))))
            })
            .unwrap_or(Value::Null);
        v["calls"] = json!(
            self.references
                .calls
                .get(&c.cell.id)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        );
        v["graphs"] = json!(self.graphs(c.cell.id));
        v["modules"] = json!(
            self.references
                .modules
                .get(&c.cell.id)
                .into_iter()
                .flatten()
                .map(|id| self.module_info(*id))
                .collect::<Vec<_>>()
        );
        v
    }
    fn wire(l: &Link) -> Value {
        json!({"id":l.id,"from":[l.from.cell,l.from.port],"to":[l.to.cell,l.to.port]})
    }
    fn space_info(&self, s: &SpaceInfo<'_>, full: bool) -> Value {
        let mut v = json!({"id":s.space.id,"name":s.space.name,"path":s.path,"owner":s.owner,
            "in":s.space.inputs.iter().map(|p|json!([p.name,self.ty(p.ty)])).collect::<Vec<_>>(),
            "out":s.space.outputs.iter().map(|p|json!([p.name,self.ty(p.ty)])).collect::<Vec<_>>(),
            "cells":s.space.cells.len(),"links":s.space.links.len(),"spaces":s.space.cells.iter().filter_map(|c|c.space.as_ref().map(|s|s.id)).collect::<Vec<_>>()});
        if full {
            v["cells"] = json!(s.space.cells.iter().map(|c| c.id).collect::<Vec<_>>());
            v["links"] = json!(s.space.links.iter().map(Self::wire).collect::<Vec<_>>());
        }
        v
    }
    fn envelope(&self, mut value: Value) -> Value {
        value["schema"] = json!(SCHEMA);
        value["revision"] = json!(self.revision);
        value
    }
    fn bounded(&self, value: Value, o: &Options) -> Result<Value> {
        if o.revision
            .as_ref()
            .is_some_and(|expected| expected != &self.revision)
        {
            return Err("project revision changed; refresh overview and restart pagination".into());
        }
        let value = self.envelope(value);
        if bytes(&value).len() + 1 > o.budget {
            Err(
                "response exceeds byte budget; use list/links/context pages, or raise --budget"
                    .into(),
            )
        } else {
            Ok(value)
        }
    }
    fn page(&self, mut rows: Vec<Value>, o: &Options) -> Result<Value> {
        rows.sort_by_key(|v| v["id"].as_u64());
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|v| v["id"].as_u64().unwrap() > o.after)
            .collect();
        let total = rows.len();
        let mut items = Vec::new();
        for row in rows.iter().take(o.limit) {
            items.push(row.clone());
            let next = if items.len() < total {
                items.last().map(|v| v["id"].clone())
            } else {
                None
            };
            if self
                .bounded(
                    json!({"items":items,"next_after":next,"remaining":total-items.len()}),
                    o,
                )
                .is_err()
            {
                items.pop();
                break;
            }
        }
        if total > 0 && items.is_empty() {
            return Err(
                "one record exceeds budget; raise --budget or request code separately".into(),
            );
        }
        let next = if items.len() < total {
            items.last().map(|v| v["id"].clone())
        } else {
            None
        };
        self.bounded(
            json!({"items":items,"next_after":next,"remaining":total-items.len()}),
            o,
        )
    }
    fn source_page(&self, source: &str, id: Id, o: &Options) -> Result<Value> {
        let digest = hash(&source);
        if o.known.as_ref() == Some(&digest) {
            return self.bounded(json!({"id":id,"hash":digest,"unchanged":true,"representation":"canonical_mira","line_scope":"definition","authority":"project_content"}), o);
        }
        let all: Vec<_> = source.lines().collect();
        if o.line > all.len() + 1 {
            return Err("line beyond end of source".into());
        }
        let start = o.line - 1;
        let mut end = (start + o.lines).min(all.len());
        loop {
            let value = json!({"id":id,"hash":digest,"line":o.line,"total_lines":all.len(),"next_line":if end<all.len(){Some(end+1)}else{None},"source":all[start..end].join("\n"),"representation":"canonical_mira","line_scope":"definition","authority":"project_content"});
            if let Ok(v) = self.bounded(value, o) {
                return Ok(v);
            }
            if end == start || end == start + 1 {
                return Err("source line exceeds budget; raise --budget".into());
            }
            end -= 1;
        }
    }
    fn context_cells(&self, root: Id) -> BTreeSet<Id> {
        let mut visited = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut pending = VecDeque::from([(root, true)]);
        while let Some((id, wired)) = pending.pop_front() {
            if !visited.insert((id, wired)) {
                continue;
            }
            ids.insert(id);
            let c = self.cells.iter().find(|c| c.cell.id == id).unwrap();
            if wired {
                pending.extend(
                    c.space
                        .links
                        .iter()
                        .filter(|l| l.to.cell == id && l.from.cell != c.space.id)
                        .map(|l| (l.from.cell, true)),
                );
            }
            pending.extend(
                self.references
                    .calls
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .map(|id| (*id, false)),
            );
            for graph in self.graphs(id) {
                pending.extend(
                    graph["cells"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| (id.as_u64().unwrap(), false)),
                );
            }
        }
        ids
    }
    fn card(&self, id: Id, o: &Options) -> Result<Value> {
        let mut value = if let Some(cell) = self.cells.iter().find(|c| c.cell.id == id) {
            let mut v = self.summary(cell);
            v["kind"] = json!("cell");
            v["next"] = json!([{"query":"code","selector":id,"lines":40},{"query":"links","selector":id},{"query":"context","selector":id}]);
            v
        } else if let Some(d) = self.modules.get(&id) {
            json!({"id":id,"kind":"definition","name":d.name,
                "in":d.parameters.iter().map(|p| json!([p.name,self.ty(p.ty)])).collect::<Vec<_>>(),
                "out":self.ty(d.output),"compile_time":d.compile_time,"named_outputs":d.output_ports,
                "source_hash":self.sources[&id].1,"total_lines":self.sources[&id].0.lines().count(),
                "helpers":d.locals.len(),"local_types":d.data_types.len(),
                "parent":self.module_parents.get(&id),
                "next":[{"query":"module","selector":id,"lines":40},{"query":"symbols","selector":id}]})
        } else if let Some(space) = self.spaces.iter().find(|s| s.space.id == id) {
            let mut v = self.space_info(space, false);
            v["kind"] = json!("space");
            v["next"] = json!([{"query":"list","selector":id},{"query":"links","selector":id}]);
            v
        } else if let Some(t) = self.types.iter().find(|t| t.id == id) {
            let mut v = self.type_info(t);
            v["layout_kind"] = v["kind"].clone();
            v["kind"] = json!("type");
            v
        } else {
            return Err(format!("object #{id} not found"));
        };
        value["facts_origin"] = json!("program_structure");
        value["effect_analysis"] = json!("not_provided");
        value["author_notes"] = Value::Null;
        self.bounded(value, o)
    }
    pub fn query(&self, command: &str, selector: Option<&str>, o: &Options) -> Result<Value> {
        o.validate()?;
        if o.revision.as_ref().is_some_and(|r| r != &self.revision) {
            return Err("project revision changed; refresh overview and restart pagination".into());
        }
        let required = || selector.ok_or("query needs a selector");
        match command {
            "enter" => self.bounded(json!({"root":self.program.root.id,"name":self.program.root.name,
                "counts":{"cells":self.cells.len(),"definitions":self.modules.len(),"types":self.types.len()},
                "protocol":{"version":mira_core::inspect::PROTOCOL_VERSION,"origin":"builtin_protocol","rules":mira_core::inspect::ENTRY_RULES},
                "implementation":"bootstrap_snapshot","mutation":false,
                "next":[{"query":"list","selector":self.program.root.id},{"query":"definitions"},{"query":"card","selector":self.program.root.id}]}),o),
            "card" => self.card(required()?.trim_start_matches('#').parse::<Id>().map_err(|_|"card requires an object ID")?,o),
            "overview" => self.bounded(json!({"root":self.program.root.id,"name":self.program.root.name,"counts":{"cells":self.cells.len(),"spaces":self.spaces.len(),"modules":self.modules.len(),"types":self.types.len()},"queries":["enter","card","definitions","list","spaces","cell","links","context","code","symbols","module","types","type","search","export","changes"]}),o),
            "definitions" => self.page(self.modules.values().map(|d|json!({"id":d.id,"name":d.name,"compile_time":d.compile_time,"source_hash":self.sources[&d.id].1,"parent":self.module_parents.get(&d.id)})).collect(),o),
            "list" => {
                let scope=selector.map(|s|s.parse::<Id>().map_err(|_|"list scope must be a space ID")).transpose()?;
                if scope.is_some_and(|id|self.program.root.find(id).is_none()) {return Err("space not found".into());}
                self.page(self.cells.iter().filter(|c|scope.is_none_or(|id|id==c.space.id)).map(|c|self.summary(c)).collect(),o)
            }
            "spaces" => self.page(self.spaces.iter().map(|s|self.space_info(s,false)).collect(),o),
            "cell" => {let c=self.cell(required()?)?;self.bounded(self.record(c),o)}
            "links" => {
                let text=required()?;
                let (s,filter)=if let Ok(id)=text.parse::<Id>() {if let Some(s)=self.program.root.find(id) {(s,None)} else {let c=self.cell(text)?;(c.space,Some(c.cell.id))}} else {let c=self.cell(text)?;(c.space,Some(c.cell.id))};
                let mut rows:Vec<_>=s.links.iter().filter(|l|filter.is_none_or(|id|id==l.from.cell||id==l.to.cell)).map(Self::wire).collect();rows.sort_by_key(|v|v["id"].as_u64());self.page(rows,o)
            }
            "code" => {let c=self.cell(required()?)?;self.source_page(self.source(c).as_ref(),c.cell.id,o)}
            "symbols" => {
                let selector = required()?;
                let module = selector.parse::<Id>().ok().and_then(|id| self.modules.get(&id).copied());
                let module = if let Some(d) = module { d } else {
                    let c = self.cell(selector)?;
                    let (Definition::Code(id) | Definition::Adaptive(id)) = c.cell.definition else { return self.page(vec![], o); };
                    self.modules[&id]
                };
                let mut rows: Vec<_> = module.locals.iter().map(|d| {
                    let mut v = self.module_info(d.id); v["kind"] = json!("code"); v
                }).collect();
                rows.extend(module.data_types.iter().map(|t| json!({"id":t.id,"name":t.name,"kind":"type"})));
                self.page(rows,o)
            }
            "module" => {let id=required()?.parse::<Id>().map_err(|_|"module selector must be an ID")?;let d=self.modules.get(&id).ok_or("module not found")?;self.source_page(&self.sources[&d.id].0,id,o)}
            "types" => self.page(self.types.iter().map(|t|json!({"id":t.id,"name":t.name})).collect(),o),
            "type" => {
                let s=required()?;let id=s.parse::<Id>().ok();let t=self.types.iter().find(|t|id==Some(t.id)||t.name==s).ok_or("type not found")?;
                self.bounded(self.type_info(t),o)
            }
            "context" => {
                let c=self.cell(required()?)?;let ids=self.context_cells(c.cell.id);
                self.page(self.cells.iter().filter(|i|ids.contains(&i.cell.id)).map(|c|self.record(c)).collect(),o)
            }
            "search" => {
                let q=required()?.to_lowercase();if q.is_empty(){return Err("empty search".into());}
                let mut rows=Vec::new();
                for c in &self.cells {
                    let name=c.path.to_lowercase().contains(&q);
                    let source=if o.text {self.source(c)} else {Cow::Borrowed("")};
                    let hit=source.lines().enumerate().find(|(_,s)|s.to_lowercase().contains(&q));
                    if name||hit.is_some() {let mut row=self.summary(c);if let Some((line,text))=hit {row["match"]=json!({"line":line+1,"text":text.chars().take(120).collect::<String>()});}rows.push(row);}
                }
                self.page(rows,o)
            }
            _=>Err("unknown inspection query".into()),
        }
    }
    fn type_info(&self, t: &data::Definition) -> Value {
        match &t.kind {
            data::Kind::Tensor { element, shape } => {
                json!({"id":t.id,"name":t.name,"kind":"tensor","element":self.ty(*element),"shape":shape})
            }
            data::Kind::Record(fields) => {
                json!({"id":t.id,"name":t.name,"kind":"record","fields":fields.iter().map(|p|json!([p.name,self.ty(p.ty)])).collect::<Vec<_>>()})
            }
            data::Kind::Array { element, length } => {
                json!({"id":t.id,"name":t.name,"kind":"array","element":self.ty(*element),"length":length})
            }
        }
    }
    fn manifest(&self) -> (Value, BTreeMap<String, Vec<u8>>) {
        let mut blobs = BTreeMap::new();
        let mut blob = |contents: &[u8], extension: &str| {
            let digest = hash(contents);
            let path = format!("blobs/{digest}.{extension}");
            blobs
                .entry(path.clone())
                .or_insert_with(|| contents.to_vec());
            json!({"hash":digest,"path":path})
        };
        let mut cells = serde_json::Map::new();
        for c in &self.cells {
            let mut record = self.record(c);
            record["source"] = blob(self.source(c).as_bytes(), "mira");
            cells.insert(c.cell.id.to_string(), blob(&bytes(&record), "json"));
        }
        let mut spaces = serde_json::Map::new();
        for s in &self.spaces {
            spaces.insert(
                s.space.id.to_string(),
                blob(&bytes(&self.space_info(s, true)), "json"),
            );
        }
        let mut modules = serde_json::Map::new();
        for (id, (text, _)) in &self.sources {
            modules.insert(id.to_string(), blob(text.as_bytes(), "mira"));
        }
        let mut types = serde_json::Map::new();
        for t in &self.types {
            types.insert(t.id.to_string(), blob(&bytes(&self.type_info(t)), "json"));
        }
        (
            json!({"schema":SCHEMA,"revision":self.revision,"root":self.program.root.id,"cells":cells,"spaces":spaces,"modules":modules,"types":types}),
            blobs,
        )
    }
    /// Commit a new manifest only after all immutable blocks exist. Never modify source.
    pub fn export(&self, directory: &Path) -> Result<Value> {
        let old = read_manifest(directory)?;
        let (manifest, blobs) = self.manifest();
        fs::create_dir_all(directory.join("blobs")).map_err(|e| e.to_string())?;
        let mut written = 0usize;
        let mut reused = 0usize;
        for (relative, contents) in blobs {
            let path = directory.join(relative);
            if path.exists() {
                if fs::read(&path).map_err(|e| e.to_string())? != contents {
                    return Err("index blob content/hash mismatch".into());
                }
                reused += 1;
            } else {
                atomic_write(&path, &contents)?;
                written += 1;
            }
        }
        if old.as_ref() != Some(&manifest) {
            atomic_write(&directory.join("manifest.json"), &bytes(&manifest))?;
        }
        Ok(self.envelope(json!({"written":written,"reused":reused,"manifest":directory.join("manifest.json").display().to_string()})))
    }
    pub fn changes(&self, directory: &Path, o: &Options) -> Result<Value> {
        o.validate()?;
        if o.revision.as_ref().is_some_and(|r| r != &self.revision) {
            return Err("project revision changed; refresh overview and restart pagination".into());
        }
        let old = read_manifest(directory)?.ok_or("index manifest missing")?;
        let (current, _) = self.manifest();
        let mut rows = BTreeMap::new();
        for category in ["cells", "spaces", "modules", "types"] {
            let before = old[category]
                .as_object()
                .ok_or("invalid index manifest section")?;
            let after = current[category].as_object().unwrap();
            for id in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
                if before.get(id) == after.get(id) {
                    continue;
                }
                let number = id.parse::<Id>().map_err(|_| "invalid manifest ID")?;
                let change = if !before.contains_key(id) {
                    "added"
                } else if !after.contains_key(id) {
                    "removed"
                } else {
                    "changed"
                };
                rows.insert(number, json!({"id":number,"kind":category,"change":change}));
            }
        }
        self.page(rows.into_values().collect(), o)
    }
}
fn read_manifest(directory: &Path) -> Result<Option<Value>> {
    let path = directory.join("manifest.json");
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read(&path).map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if value["schema"] != SCHEMA || !value["cells"].is_object() {
        return Err(
            "directory has an incompatible manifest; choose a Mira inspection index directory"
                .into(),
        );
    }
    Ok(Some(value))
}
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let temporary = path.with_extension(format!(
        "writing-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|e| e.to_string())
}
/// The CLI adapter; no model provider, editor, or execution authority is required.
pub fn command(program: &Program, args: &[String]) -> Result<Value> {
    let mut o = Options::default();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        if key == "--text" {
            o.text = true;
            i += 1;
            continue;
        }
        if key.starts_with("--") {
            let value = args.get(i + 1).ok_or("missing option value")?;
            match key {
                "--known" => o.known = Some(value.clone()),
                "--revision" => o.revision = Some(value.clone()),
                "--limit" => o.limit = value.parse().map_err(|_| "invalid limit")?,
                "--budget" => o.budget = value.parse().map_err(|_| "invalid byte budget")?,
                "--after" => o.after = value.parse().map_err(|_| "invalid cursor")?,
                "--line" => o.line = value.parse().map_err(|_| "invalid line")?,
                "--lines" => o.lines = value.parse().map_err(|_| "invalid line count")?,
                _ => return Err("unknown inspection option".into()),
            }
            i += 2;
        } else {
            positional.push(key);
            i += 1;
        }
    }
    if positional.len() > 2 {
        return Err("expected a query and at most one selector".into());
    }
    o.validate()?;
    let s = Snapshot::new(program)?;
    if o.revision.as_ref().is_some_and(|r| r != &s.revision) {
        return Err("project revision changed; refresh overview and restart pagination".into());
    }
    let query = positional.first().copied().unwrap_or("enter");
    let selector = positional.get(1).copied();
    match query {
        "export" => {
            let result = s.export(Path::new(selector.ok_or("export needs a directory")?))?;
            if bytes(&result).len() > o.budget {
                return Err("export report exceeds byte budget".into());
            }
            Ok(result)
        }
        "changes" => s.changes(
            Path::new(selector.ok_or("changes needs an index directory")?),
            &o,
        ),
        _ => s.query(query, selector, &o),
    }
}
