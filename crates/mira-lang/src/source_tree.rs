//! Recursive source storage. The assembled source is the only compiler input.
use mira_core::{Id, Program, Result, Space, code::CodeModule};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const SOURCE_LIMIT: usize = 4 * 1024 * 1024;
const NODE_LIMIT: usize = 65536;
const DEPTH_LIMIT: usize = 64;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTree {
    pub root: SourceElement,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceElement {
    pub id: Id,
    pub name: String,
    pub content: String,
    pub children: Vec<EmbeddedSource>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedSource {
    /// Byte offset in the parent's own content, before any children are inserted.
    pub offset: usize,
    pub element: SourceElement,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceEntry {
    pub parent: Option<Id>,
    pub id: Id,
    pub name: String,
    /// Human-readable path through actual declaration/Z containment.
    pub path: String,
    /// Byte size of this file with all its children assembled.
    pub size: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceView {
    pub entry: SourceEntry,
    pub source: String,
    pub children: Vec<SourceEntry>,
}

type Topology = BTreeMap<Id, (String, Option<Id>)>;
fn topology(program: &Program) -> Result<Topology> {
    fn add(out: &mut Topology, id: Id, name: &str, parent: Option<Id>) -> Result<()> {
        if out.insert(id, (name.into(), parent)).is_some() {
            return Err(format!("duplicate source identity {id}"));
        }
        if out.len() > NODE_LIMIT {
            return Err("source node budget exceeded".into());
        }
        Ok(())
    }
    fn code(out: &mut Topology, module: &CodeModule, parent: Id, depth: usize) -> Result<()> {
        if depth > DEPTH_LIMIT {
            return Err("source nesting exceeds 64".into());
        }
        let container = if let Some(owner) = module.owner {
            // The inline implementation is part of the existing component,
            // not another source object between it and its code.
            owner
        } else {
            add(out, module.id, &module.name, Some(parent))?;
            module.id
        };
        for data in &module.data_types {
            add(out, data.id, &data.name, Some(container))?;
        }
        for local in &module.locals {
            code(out, local, container, depth + 1)?;
        }
        Ok(())
    }
    fn space(out: &mut Topology, s: &Space, parent: Option<Id>, depth: usize) -> Result<()> {
        if depth > DEPTH_LIMIT {
            return Err("source nesting exceeds 64".into());
        }
        add(out, s.id, &s.name, parent)?;
        for cell in &s.cells {
            add(out, cell.id, &cell.name, Some(s.id))?;
            if let Some(inner) = &cell.space {
                space(out, inner, Some(cell.id), depth + 2)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    space(&mut out, &program.root, None, 0)?;
    for data in &program.global_types {
        add(&mut out, data.id, &data.name, Some(program.root.id))?;
    }
    for module in &program.code_modules {
        code(&mut out, module, program.root.id, 1)?;
    }
    Ok(out)
}

fn identities(program: &Program) -> Result<BTreeSet<Id>> {
    let mut ids: BTreeSet<_> = topology(program)?.into_keys().collect();
    for (_, definition) in mira_core::code::symbols(&program.code_modules) { ids.insert(definition.id); }
    for entry in mira_core::inspect::spaces(program) {
        for link in &entry.space.links { ids.insert(link.id); }
    }
    Ok(ids)
}

/// Pin identities without reformatting source, then project actual declaration scopes.
pub fn from_source(source: &str) -> Result<SourceTree> {
    rebuild(source).map(|(tree, _)| tree)
}
fn rebuild(source: &str) -> Result<(SourceTree, Program)> {
    let source = crate::frontend::pin_source(source)?;
    if source.len() > SOURCE_LIMIT {
        return Err("source exceeds 4 MiB".into());
    }
    let (program, mut spans) = crate::frontend::source_layout(&source)?;
    let topology = topology(&program)?;
    spans.retain(|id, _| topology.contains_key(id));
    spans.insert(program.root.id, (0, source.len()));
    fn build(
        id: Id,
        source: &str,
        topology: &Topology,
        spans: &BTreeMap<Id, (usize, usize)>,
        depth: usize,
    ) -> Result<SourceElement> {
        if depth > DEPTH_LIMIT {
            return Err("source nesting exceeds 64".into());
        }
        let &(start, end) = spans
            .get(&id)
            .ok_or_else(|| format!("source span missing for identity {id}"))?;
        if start > end
            || end > source.len()
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(end)
        {
            return Err(format!("invalid source span for identity {id}"));
        }
        let mut child_ids = topology
            .iter()
            .filter_map(|(&child, (_, parent))| (*parent == Some(id)).then_some(child))
            .collect::<Vec<_>>();
        child_ids.sort_by_key(|child| {
            spans
                .get(child)
                .copied()
                .unwrap_or((usize::MAX, usize::MAX))
        });
        let mut content = String::new();
        let mut children = Vec::new();
        let mut previous = start;
        for child in child_ids {
            let &(a, b) = spans
                .get(&child)
                .ok_or_else(|| format!("source span missing for identity {child}"))?;
            if a < previous || b < a || b > end {
                return Err(format!(
                    "overlapping or invalid source span for identity {child}"
                ));
            }
            content.push_str(&source[previous..a]);
            children.push(EmbeddedSource {
                offset: content.len(),
                element: build(child, source, topology, spans, depth + 1)?,
            });
            previous = b;
        }
        content.push_str(&source[previous..end]);
        Ok(SourceElement {
            id,
            name: topology[&id].0.clone(),
            content,
            children,
        })
    }
    Ok((
        SourceTree {
            root: build(program.root.id, &source, &topology, &spans, 0)?,
        },
        program,
    ))
}

impl SourceTree {
    pub fn from_source(source: &str) -> Result<Self> {
        from_source(source)
    }
    /// Assemble and verify identities, names, declaration parents and exact spans.
    pub fn validate(&self) -> Result<Program> {
        let source = self.assembled()?;
        let (canonical, program) = rebuild(&source)?;
        if canonical != *self {
            return Err(
                "source tree identity, name, parent or declaration boundaries disagree with source"
                    .into(),
            );
        }
        Ok(program)
    }
    pub fn assembled(&self) -> Result<String> {
        let mut seen = BTreeSet::new();
        let mut bytes = 0;
        fn assemble(
            file: &SourceElement,
            seen: &mut BTreeSet<Id>,
            bytes: &mut usize,
            depth: usize,
        ) -> Result<String> {
            if depth > DEPTH_LIMIT {
                return Err("source nesting exceeds 64".into());
            }
            if !seen.insert(file.id) {
                return Err(format!("duplicate source identity {}", file.id));
            }
            if seen.len() > NODE_LIMIT {
                return Err("source node budget exceeded".into());
            }
            *bytes = bytes
                .checked_add(file.content.len())
                .ok_or("source size overflow")?;
            if *bytes > SOURCE_LIMIT {
                return Err("source exceeds 4 MiB".into());
            }
            let mut result = String::new();
            let mut previous = 0;
            for child in &file.children {
                if child.offset < previous
                    || child.offset > file.content.len()
                    || !file.content.is_char_boundary(child.offset)
                {
                    return Err(format!("invalid child offset in identity {}", file.id));
                }
                result.push_str(&file.content[previous..child.offset]);
                result.push_str(&assemble(&child.element, seen, bytes, depth + 1)?);
                previous = child.offset;
            }
            result.push_str(&file.content[previous..]);
            Ok(result)
        }
        assemble(&self.root, &mut seen, &mut bytes, 0)
    }
    pub fn get(&self, id: Id) -> Option<&SourceElement> {
        fn find(file: &SourceElement, id: Id, depth: usize) -> Option<&SourceElement> {
            if depth > DEPTH_LIMIT {
                return None;
            }
            if file.id == id {
                return Some(file);
            }
            file.children
                .iter()
                .find_map(|child| find(&child.element, id, depth + 1))
        }
        find(&self.root, id, 0)
    }
    /// Return an assembled subtree, including its exact indentation and comments.
    pub fn read(&self, id: Id) -> Result<String> {
        self.validate()?;
        let file = self
            .get(id)
            .ok_or_else(|| format!("unknown source identity {id}"))?;
        SourceTree {
            root: file.clone(),
        }
        .assembled()
    }
    pub fn list(&self) -> Result<Vec<SourceEntry>> {
        self.validate()?;
        Ok(self.entries())
    }
    pub(crate) fn entries(&self) -> Vec<SourceEntry> {
        fn visit(file: &SourceElement, parent: Option<Id>, prefix: &str, result: &mut Vec<SourceEntry>) -> usize {
            let path = format!("{prefix}/{}", file.name.replace('~', "~0").replace('/', "~1"));
            let index = result.len();
            result.push(SourceEntry {
                parent,
                id: file.id,
                name: file.name.clone(),
                path: path.clone(),
                size: None,
            });
            let size = file.content.len()
                + file
                    .children
                    .iter()
                    .map(|child| visit(&child.element, Some(file.id), &path, result))
                    .sum::<usize>();
            result[index].size = Some(size);
            size
        }
        let mut result = Vec::new();
        visit(&self.root, None, "", &mut result);
        result
    }
    /// Paths traverse the same children as execution; bare names are accepted
    /// only when unambiguous. Numeric IDs remain an internal/tooling address.
    pub fn resolve(&self, selector: &str) -> Result<Id> {
        let rows = self.entries();
        if let Ok(id) = selector.parse::<Id>() {
            return self.get(id).map(|_| id).ok_or("unknown source object".into());
        }
        let root_path = &rows[0].path;
        let absolute = if selector.starts_with('/') {
            selector.to_owned()
        } else if selector == self.root.name || selector.starts_with(&format!("{}/", self.root.name)) {
            format!("/{selector}")
        } else {
            format!("{root_path}/{selector}")
        };
        if let Some(row) = rows.iter().find(|row| row.path == absolute) {
            return Ok(row.id);
        }
        let matches: Vec<_> = rows.iter().filter(|row| row.name == selector).collect();
        match matches.as_slice() {
            [row] => Ok(row.id),
            [] => Err(format!("unknown source object {selector}")),
            _ => Err(format!("ambiguous source name {selector}; enter its full path")),
        }
    }
    /// Open just this object's own contents and immediate children. Text is an
    /// inspection projection, not a second editable or executable source model.
    pub fn enter(&self, selector: &str) -> Result<SourceView> {
        self.assembled()?;
        let id = self.resolve(selector)?;
        let rows = self.entries();
        let entry = rows.iter().find(|row| row.id == id).ok_or("source object missing")?.clone();
        let element = self.get(id).ok_or("source object missing")?;
        let mut source = String::new();
        let mut previous = 0;
        for child in &element.children {
            source.push_str(&element.content[previous..child.offset]);
            let indent = child.element.content.lines().find(|line| !line.trim().is_empty())
                .map(|line| line.len() - line.trim_start().len()).unwrap_or(0);
            source.push_str(&format!("{}// ↳ enter {}\n", " ".repeat(indent), child.element.name));
            previous = child.offset;
        }
        source.push_str(&element.content[previous..]);
        Ok(SourceView {
            entry,
            source: crate::frontend::display_source(&source)?,
            children: rows.into_iter().filter(|row| row.parent == Some(id)).collect(),
        })
    }
    /// Follow the existing cell -> code-definition relation. This is an edge
    /// traversal, not new containment or a copied implementation per instance.
    pub fn behavior(&self, selector: &str) -> Result<SourceView> {
        let id = self.resolve(selector)?;
        let program = self.validate()?;
        let definition = if mira_core::code::find(&program.code_modules, id).is_some() {
            id
        } else if let Some(cell) = program.root.find_cell(id) {
            match cell.definition {
                mira_core::Definition::Code(code) | mira_core::Definition::Adaptive(code) => code,
                mira_core::Definition::Graph => return Err("this component's behavior is its Z graph; use enter".into()),
                _ => return Err("this component is a primitive; its value/composition is shown by enter".into()),
            }
        } else {
            return Err("this object has no separate code behavior; use enter for its composition".into());
        };
        let body = mira_core::code::find(&program.code_modules, definition).ok_or("behavior missing")?;
        self.enter(&body.owner.unwrap_or(definition).to_string())
    }
    /// Stage a full source splice and reparse; failure leaves this tree unchanged.
    pub fn replace(&mut self, id: Id, replacement: &str) -> Result<()> {
        let old_program = self.validate()?;
        let mut source = self.assembled()?;
        let range = if id == self.root.id {
            (0, source.len())
        } else {
            if self.get(id).is_none() {
                return Err(format!("unknown source identity {id}"));
            }
            *crate::frontend::source_spans(&source)?
                .get(&id)
                .ok_or("source span missing")?
        };
        let replacement = if id == self.root.id {
            replacement.to_string()
        } else {
            retain_header_id(replacement, id)?
        };
        source.replace_range(range.0..range.1, &replacement);
        source = retain_high_water(&source, old_program.next_identity)?;
        let staged = from_source(&source)?;
        let old_ids = identities(&old_program)?;
        let new_program = staged.validate()?;
        for id in &identities(&new_program)? {
            if !old_ids.contains(id) && *id < old_program.next_identity {
                return Err("replacement reused an identity below the high-water mark".into());
            }
        }
        if staged.root.id != self.root.id {
            return Err("replacement changed root identity".into());
        }
        if staged.get(id).is_none() {
            return Err("replacement removed its target identity".into());
        }
        *self = staged;
        Ok(())
    }
}

// This only locates the declaration name; the frontend remains the syntax authority.
fn retain_header_id(source: &str, id: Id) -> Result<String> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset += line.len();
            continue;
        }
        let mut at = line.len() - trimmed.len();
        for prefix in ["struct", "type", "space", "macro"] {
            if line[at..]
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            {
                at += prefix.len();
                while line.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) {
                    at += 1;
                }
                break;
            }
        }
        if line.as_bytes().get(at) == Some(&b'`') {
            at += 1;
            loop {
                match line.as_bytes().get(at) {
                    Some(b'\\') => at += 2,
                    Some(b'`') => {
                        at += 1;
                        break;
                    }
                    Some(_) => at += 1,
                    None => return Err("unterminated replacement name".into()),
                }
            }
        } else {
            let begin = at;
            while line
                .as_bytes()
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                at += 1;
            }
            if at == begin {
                return Err("replacement must start with a declaration".into());
            }
        }
        let name_end = at;
        while line.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if line.as_bytes().get(at) == Some(&b'@') {
            at += 1;
            let begin = at;
            while line.as_bytes().get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            if line[begin..at].parse::<Id>().ok() != Some(id) {
                return Err("replacement changed target identity".into());
            }
            return Ok(source.into());
        }
        let mut result = source.to_string();
        result.insert_str(offset + name_end, &format!(" @{id}"));
        return Ok(result);
    }
    Err("replacement must contain its target declaration".into())
}
fn retain_high_water(source: &str, floor: Id) -> Result<String> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//") {
            offset += line.len();
            continue;
        }
        if let Some(rest) = t.strip_prefix("identities ") {
            let count = rest.bytes().take_while(u8::is_ascii_digit).count();
            let value = rest[..count]
                .parse::<Id>()
                .map_err(|_| "invalid identity high-water mark")?;
            if value >= floor {
                return Ok(source.into());
            }
            let start = offset + line.len() - rest.len();
            let mut result = source.to_string();
            result.replace_range(start..start + count, &floor.to_string());
            return Ok(result);
        }
        break;
    }
    Ok(format!("identities {floor}\n{source}"))
}

fn checked_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            return Err("parent path traversal is not allowed".into());
        }
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(format!("symlink is not allowed: {}", prefix.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(path)
}
fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let path = checked_path(path)?;
    if !fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("source storage byte budget exceeded".into());
    }
    Ok(bytes)
}
fn sibling(path: &Path, role: &str) -> Result<PathBuf> {
    let parent = path.parent().ok_or("destination has no parent")?;
    let name = path
        .file_name()
        .ok_or("destination has no filename")?
        .to_string_lossy();
    Ok(parent.join(format!(
        ".{name}.{role}-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )))
}
fn create_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

// Source code itself is the recursive container: declarations and Z blocks
// determine containment. SourceTree is only an in-memory view of that syntax.
pub(crate) fn read_storage(path: &Path) -> Result<SourceTree> {
    from_source(&draft_source(path)?)
}
/// Stage zero extracts the real compiler declaration using the same lexical
/// declaration boundaries. No JSON, offset index or second compiler source.
pub(crate) fn embedded_selected(container: &str, id: Id) -> Result<String> {
    let spans = crate::frontend::source_spans_seed(container)?;
    let &(start, end) = spans.get(&id).ok_or("unknown embedded source object")?;
    Ok(container[start..end].into())
}
fn selected(tree: &SourceTree, id: Id) -> Result<String> {
    let element = tree.get(id).ok_or("unknown source object")?.clone();
    SourceTree { root: element }.assembled()
}
/// Structural navigation needs no compiler run and never creates another tree.
pub fn catalog(path: &Path) -> Result<Vec<SourceEntry>> {
    let tree = read_storage(path)?;
    Ok(tree.entries())
}
pub fn read_selected(path: &Path, id: Id) -> Result<String> {
    selected(&read_storage(path)?, id)
}
/// Incomplete-code analysis uses the same contained contents; semantic checks
/// remain the responsibility of the draft checker.
pub fn draft_source(path: &Path) -> Result<String> {
    String::from_utf8(read_bytes(path, SOURCE_LIMIT)?).map_err(|e| e.to_string())
}
pub fn open(path: &Path) -> Result<(SourceTree, Program)> {
    let tree = read_storage(path)?;
    let program = tree.validate()?;
    Ok((tree, program))
}
pub fn read(path: &Path) -> Result<SourceTree> {
    open(path).map(|(tree, _)| tree)
}
pub fn source(path: &Path) -> Result<String> {
    read(path)?.assembled()
}
/// Publish the complete recursive object atomically. Editing containment is
/// editing Program/Z containment; a physical regroup operation does not exist.
pub fn write(path: &Path, tree: &SourceTree) -> Result<()> {
    tree.validate()?;
    let path = checked_path(path)?;
    if path.exists() {
        read_storage(&path)?; // Never overwrite an unrelated file or directory.
    }
    let bytes = tree.assembled()?.into_bytes();
    if bytes.len() > SOURCE_LIMIT {
        return Err("source storage byte budget exceeded".into());
    }
    let temporary = sibling(&path, "saving")?;
    let result = (|| {
        create_file(&temporary, &bytes)?;
        if read_storage(&temporary)? != *tree {
            return Err("recursive source publication changed contents".into());
        }
        checked_path(&path)?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        fs::File::open(path.parent().ok_or("destination has no parent")?)
            .and_then(|f| f.sync_all()).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
