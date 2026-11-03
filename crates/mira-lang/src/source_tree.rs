//! Recursive source storage. The assembled source is the only compiler input.
use mira_core::{Id, Program, Result, Space, code::CodeModule};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const FORMAT: &str = "mira-source-index-v2";
const SOURCE_LIMIT: usize = 4 * 1024 * 1024;
const JSON_LIMIT: usize = 32 * 1024 * 1024;
const NODE_LIMIT: usize = 65536;
const DEPTH_LIMIT: usize = 64;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTree {
    pub format: String,
    pub root: SourceElement,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceElement {
    pub id: Id,
    pub name: String,
    pub content: String,
    pub children: Vec<EmbeddedSource>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Byte size of this file with all its children assembled.
    pub size: Option<usize>,
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
        add(out, module.id, &module.name, Some(parent))?;
        for data in &module.data_types {
            add(out, data.id, &data.name, Some(module.id))?;
        }
        for local in &module.locals {
            code(out, local, module.id, depth + 1)?;
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
            format: FORMAT.into(),
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
        if self.format != FORMAT {
            return Err(format!("unsupported source tree format {}", self.format));
        }
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
            format: FORMAT.into(),
            root: file.clone(),
        }
        .assembled()
    }
    pub fn list(&self) -> Result<Vec<SourceEntry>> {
        self.validate()?;
        fn visit(file: &SourceElement, parent: Option<Id>, result: &mut Vec<SourceEntry>) -> usize {
            let index = result.len();
            result.push(SourceEntry {
                parent,
                id: file.id,
                name: file.name.clone(),
                size: None,
            });
            let size = file.content.len()
                + file
                    .children
                    .iter()
                    .map(|child| visit(&child.element, Some(file.id), result))
                    .sum::<usize>();
            result[index].size = Some(size);
            size
        }
        let mut result = Vec::new();
        visit(&self.root, None, &mut result);
        Ok(result)
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
        let old_ids = topology(&old_program)?;
        let new_program = staged.validate()?;
        for id in topology(&new_program)?.keys() {
            if !old_ids.contains_key(id) && *id < old_program.next_identity {
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
// Bound nesting before deserialization, allowing valid recursive Z trees
// beyond serde_json's default 128 JSON containers without unbounded recursion.
fn decode_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let mut depth = 0usize;
    let mut string = false;
    let mut escaped = false;
    for byte in bytes {
        if string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                string = false;
            }
        } else {
            match *byte {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > DEPTH_LIMIT * 4 + 16 {
                        return Err("source JSON nesting budget exceeded".into());
                    }
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1).ok_or("invalid source JSON nesting")?;
                }
                _ => {}
            }
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    decoder.disable_recursion_limit();
    let tree = T::deserialize(&mut decoder).map_err(|e| e.to_string())?;
    decoder.end().map_err(|e| e.to_string())?;
    Ok(tree)
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

// Physical packing is independent of Program/Z identities. No file-kind tag.
const LAYOUT: &str = "mira-source-layout-v3";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    format: String,
    tree: SourceTree,
    files: Vec<SourceFile>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    content: String,
    name: String,
    ranges: Vec<Range>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    id: Id,
    offset: usize,
    length: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGroup {
    pub name: String,
    pub ids: Vec<Id>,
}
type Locations = BTreeMap<Id, (usize, usize, usize)>;

fn locations(layout: &Layout) -> Result<Locations> {
    if layout.format != LAYOUT || layout.tree.format != FORMAT {
        return Err("unsupported source layout".into());
    }
    let mut names = BTreeSet::new();
    let mut result = BTreeMap::new();
    let mut total = 0usize;
    for (index, part) in layout.files.iter().enumerate() {
        if part.name.is_empty()
            || part.name.len() > 64
            || part.name == "index"
            || !part
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || !names.insert(&part.name)
        {
            return Err("invalid or duplicate source part name".into());
        }
        let mut end = 0;
        for range in &part.ranges {
            if range.offset != end
                || result
                    .insert(range.id, (index, range.offset, range.length))
                    .is_some()
            {
                return Err("overlapping or duplicate source range".into());
            }
            end = end
                .checked_add(range.length)
                .ok_or("source size overflow")?;
        }
        total = total.checked_add(end).ok_or("source size overflow")?;
        if total > SOURCE_LIMIT {
            return Err("source byte budget exceeded".into());
        }
    }
    fn shape(
        file: &SourceElement,
        locations: &Locations,
        seen: &mut BTreeSet<Id>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEPTH_LIMIT
            || seen.len() >= NODE_LIMIT
            || file.id == 0
            || !seen.insert(file.id)
            || !file.content.is_empty()
        {
            return Err("invalid source index identity/depth/content".into());
        }
        let length = locations.get(&file.id).ok_or("source range missing")?.2;
        let mut previous = 0;
        for child in &file.children {
            if child.offset < previous || child.offset > length {
                return Err("invalid source child offset".into());
            }
            previous = child.offset;
            shape(&child.element, locations, seen, depth + 1)?;
        }
        Ok(())
    }
    let mut seen = BTreeSet::new();
    shape(&layout.tree.root, &result, &mut seen, 0)?;
    if seen.len() != result.len() {
        return Err("source layout contains unknown identities".into());
    }
    Ok(result)
}
fn source_layout(path: &Path) -> Result<(Layout, Locations)> {
    let expanded = path.is_dir();
    let layout: Layout = decode_json(&read_bytes(
        &if expanded {
            path.join("index")
        } else {
            path.to_path_buf()
        },
        JSON_LIMIT,
    )?)?;
    let locations = locations(&layout)?;
    for file in &layout.files {
        let expected = file.ranges.last().map_or(0, |r| r.offset + r.length);
        if expanded {
            if !file.content.is_empty() {
                return Err("directory index duplicates file content".into());
            }
        } else if file.content.len() != expected {
            return Err("contained source file length mismatch".into());
        }
    }
    Ok((layout, locations))
}
fn hydrate_part(
    file: &mut SourceElement,
    parts: &[SourceFile],
    locations: &Locations,
    cache: &mut BTreeMap<usize, Vec<u8>>,
    read: &impl Fn(&SourceFile, usize) -> Result<Vec<u8>>,
) -> Result<()> {
    let &(part, start, length) = locations.get(&file.id).ok_or("source range missing")?;
    if !cache.contains_key(&part) {
        let last = parts[part].ranges.last().ok_or("empty source part")?;
        let expected = last.offset + last.length;
        let bytes = read(&parts[part], expected)?;
        if bytes.len() != expected {
            return Err("source part length changed outside a validated edit".into());
        }
        cache.insert(part, bytes);
    }
    let bytes = &cache[&part][start..start + length];
    file.content = std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())?
        .into();
    for child in &mut file.children {
        hydrate_part(&mut child.element, parts, locations, cache, read)?;
    }
    Ok(())
}
fn read_source_part(path: &Path, part: &SourceFile, expected: usize) -> Result<Vec<u8>> {
    if path.is_dir() {
        read_bytes(&path.join(&part.name), expected)
    } else {
        Ok(part.content.as_bytes().to_vec())
    }
}
/// Stage zero embeds selected canonical parts, using the same index and assembler
/// as source navigation. The executable does not require a checkout at runtime.
pub(crate) fn embedded_selected(index: &str, parts: &[(&str, &str)], id: Id) -> Result<String> {
    let layout: Layout = decode_json(index.as_bytes())?;
    let locations = locations(&layout)?;
    let mut element = layout.tree.get(id)
        .ok_or("unknown embedded source identity")?.clone();
    hydrate_part(
        &mut element,
        &layout.files,
        &locations,
        &mut BTreeMap::new(),
        &|part, _| {
            parts.iter().find(|(name, _)| *name == part.name)
                .map(|(_, source)| source.as_bytes().to_vec())
                .ok_or_else(|| format!("missing embedded source part {}", part.name))
        },
    )?;
    SourceTree { format: FORMAT.into(), root: element }.assembled()
}
/// Metadata only. Source bodies are not opened for catalog queries.
pub fn catalog(path: &Path) -> Result<Vec<SourceEntry>> {
    let path = checked_path(path)?;
    let (layout, locations) = source_layout(&path)?;
    let tree = layout.tree;
    let lengths = Some(locations);
    fn walk(
        file: &SourceElement,
        parent: Option<Id>,
        lengths: &Option<Locations>,
        out: &mut Vec<SourceEntry>,
    ) -> usize {
        let index = out.len();
        out.push(SourceEntry {
            parent,
            id: file.id,
            name: file.name.clone(),
            size: None,
        });
        let own = lengths
            .as_ref()
            .map_or(file.content.len(), |map| map[&file.id].2);
        let length = own
            + file
                .children
                .iter()
                .map(|child| walk(&child.element, Some(file.id), lengths, out))
                .sum::<usize>();
        out[index].size = Some(length);
        length
    }
    let mut out = Vec::new();
    walk(&tree.root, None, &lengths, &mut out);
    Ok(out)
}
pub fn source_files(path: &Path) -> Result<Vec<SourceGroup>> {
    let (layout, _) = source_layout(&checked_path(path)?)?;
    Ok(layout
        .files
        .into_iter()
        .map(|file| SourceGroup {
            name: file.name,
            ids: file.ranges.into_iter().map(|r| r.id).collect(),
        })
        .collect())
}
pub fn read_file(path: &Path, name: &str) -> Result<String> {
    let path = checked_path(path)?;
    let (layout, _) = source_layout(&path)?;
    let file = layout
        .files
        .iter()
        .find(|file| file.name == name)
        .ok_or("unknown source file")?;
    let expected = file.ranges.last().map_or(0, |r| r.offset + r.length);
    let bytes = if path.is_dir() {
        read_bytes(&path.join(name), expected)?
    } else {
        file.content.as_bytes().to_vec()
    };
    if bytes.len() != expected {
        return Err("source file length mismatch".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
pub fn read_selected(path: &Path, id: Id) -> Result<String> {
    let path = checked_path(path)?;
    let (layout, locations) = source_layout(&path)?;
    let mut element = layout
        .tree
        .get(id)
        .ok_or("unknown source identity")?
        .clone();
    hydrate_part(
        &mut element,
        &layout.files,
        &locations,
        &mut BTreeMap::new(),
        &|part, expected| read_source_part(&path, part, expected),
    )?;
    SourceTree {
        format: FORMAT.into(),
        root: element,
    }
    .assembled()
}
fn read_storage(path: &Path) -> Result<SourceTree> {
    let path = checked_path(path)?;
    let (mut layout, locations) = source_layout(&path)?;
    if path.is_dir() {
        let allowed: BTreeSet<_> = layout
            .files
            .iter()
            .map(|file| file.name.as_str())
            .chain(std::iter::once("index"))
            .collect();
        for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| allowed.contains(name))
            {
                return Err("unexpected source storage entry".into());
            }
            checked_path(&entry.path())?;
        }
    }
    hydrate_part(
        &mut layout.tree.root,
        &layout.files,
        &locations,
        &mut BTreeMap::new(),
        &|part, expected| read_source_part(&path, part, expected),
    )?;
    Ok(layout.tree)
}
/// Read the same authoritative storage for incomplete-code analysis. Storage
/// limits/hashes still apply; semantic completeness is deliberately checked later.
pub fn draft_source(path: &Path) -> Result<String> {
    read_storage(path)?.assembled()
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

/// Source boundaries follow existing composition, never line counts or leaf IDs.
/// A container with several children starts a contained file. Leaves and
/// single-child headers stay with their nearest containing file.
pub fn composition_groups(tree: &SourceTree) -> Vec<SourceGroup> {
    fn visit(element: &SourceElement, parent: usize, root: bool, out: &mut Vec<SourceGroup>) {
        let group = if root || element.children.len() > 1 {
            let name = if root { "program".into() } else { format!("{}-{}", element.name, element.id) };
            out.push(SourceGroup { name, ids: Vec::new() });
            out.len() - 1
        } else { parent };
        out[group].ids.push(element.id);
        for child in &element.children {
            visit(&child.element, group, false, out);
        }
    }
    let mut out = Vec::new();
    visit(&tree.root, 0, true, &mut out);
    out
}
fn default_groups(tree: &SourceTree) -> Vec<SourceGroup> {
    composition_groups(tree)
}
fn inherited_groups(tree: &SourceTree, previous: &Layout) -> Vec<SourceGroup> {
    let mut out: Vec<_> = previous
        .files
        .iter()
        .map(|part| SourceGroup {
            name: part.name.clone(),
            ids: Vec::new(),
        })
        .collect();
    let assignments: BTreeMap<_, _> = previous
        .files
        .iter()
        .enumerate()
        .flat_map(|(index, part)| part.ranges.iter().map(move |r| (r.id, index)))
        .collect();
    fn visit(
        file: &SourceElement,
        parent: usize,
        map: &BTreeMap<Id, usize>,
        out: &mut [SourceGroup],
    ) {
        let group = map.get(&file.id).copied().unwrap_or(parent);
        out[group].ids.push(file.id);
        for child in &file.children {
            visit(&child.element, group, map, out);
        }
    }
    if out.is_empty() {
        return default_groups(tree);
    }
    visit(&tree.root, 0, &assignments, &mut out);
    out.retain(|group| !group.ids.is_empty());
    out
}
fn packing(tree: &SourceTree, groups: &[SourceGroup]) -> Result<(Layout, Vec<Vec<u8>>)> {
    let mut layout = Layout {
        format: LAYOUT.into(),
        tree: tree.clone(),
        files: Vec::new(),
    };
    let mut blobs = Vec::new();
    for group in groups {
        let mut blob = Vec::new();
        let mut ranges = Vec::new();
        for id in &group.ids {
            let file = tree
                .get(*id)
                .ok_or("unknown source identity in physical group")?;
            ranges.push(Range {
                id: *id,
                offset: blob.len(),
                length: file.content.len(),
            });
            blob.extend_from_slice(file.content.as_bytes());
        }
        layout.files.push(SourceFile {
            content: String::new(),
            name: group.name.clone(),
            ranges,
        });
        blobs.push(blob);
    }
    fn clear(file: &mut SourceElement) {
        file.content.clear();
        for child in &mut file.children {
            clear(&mut child.element);
        }
    }
    clear(&mut layout.tree.root);
    locations(&layout)?;
    Ok((layout, blobs))
}

pub fn write(path: &Path, tree: &SourceTree) -> Result<()> {
    if path.is_dir() {
        return write_expanded(path, tree);
    }
    let groups = if path.exists() {
        inherited_groups(tree, &source_layout(&checked_path(path)?)?.0)
    } else {
        default_groups(tree)
    };
    write_packed(path, tree, &groups)
}
pub fn write_packed(path: &Path, tree: &SourceTree, groups: &[SourceGroup]) -> Result<()> {
    tree.validate()?;
    let path = checked_path(path)?;
    let (mut layout, blobs) = packing(tree, groups)?;
    for (file, blob) in layout.files.iter_mut().zip(blobs) {
        file.content = String::from_utf8(blob).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&layout).map_err(|e| e.to_string())?;
    if bytes.len() > JSON_LIMIT {
        return Err("source storage byte budget exceeded".into());
    }
    let temporary = sibling(&path, "saving")?;
    let result = (|| {
        create_file(&temporary, &bytes)?;
        if read_storage(&temporary)? != *tree {
            return Err("packed source changed contents".into());
        }
        checked_path(&path)?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn write_expanded(path: &Path, tree: &SourceTree) -> Result<()> {
    let groups = if path.is_dir() {
        inherited_groups(tree, &source_layout(&checked_path(path)?)?.0)
    } else {
        default_groups(tree)
    };
    write_grouped(path, tree, &groups)
}

/// Publish grouped byte storage. Logical files/identities/Z topology do not change.
pub fn write_grouped(path: &Path, tree: &SourceTree, groups: &[SourceGroup]) -> Result<()> {
    tree.validate()?;
    let path = checked_path(path)?;
    if path.exists() {
        read(&path)?;
        if !path.is_dir() {
            return Err("expanded destination must be a directory".into());
        }
    }
    let (layout, blobs) = packing(tree, groups)?;
    let temporary = sibling(&path, "saving")?;
    fs::create_dir(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        for (part, blob) in layout.files.iter().zip(&blobs) {
            create_file(&temporary.join(&part.name), blob)?;
        }
        create_file(
            &temporary.join("index"),
            &serde_json::to_vec_pretty(&layout).map_err(|e| e.to_string())?,
        )?;
        if read_storage(&temporary)? != *tree {
            return Err("physical source packing changed contents".into());
        }
        checked_path(&path)?;
        if !path.exists() {
            return fs::rename(&temporary, &path).map_err(|e| e.to_string());
        }
        #[cfg(target_os = "linux")]
        {
            use std::{ffi::CString, os::unix::ffi::OsStrExt};
            unsafe extern "C" {
                fn renameat2(
                    oldfd: i32,
                    old: *const std::ffi::c_char,
                    newfd: i32,
                    new: *const std::ffi::c_char,
                    flags: u32,
                ) -> i32;
            }
            let old = CString::new(temporary.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            let new = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            // RENAME_EXCHANGE publishes a complete tree in one filesystem operation.
            if unsafe { renameat2(-100, old.as_ptr(), -100, new.as_ptr(), 2) } != 0 {
                return Err(format!(
                    "atomic source publication failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if let Some(parent) = path.parent() {
                fs::File::open(parent)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            let _ = fs::remove_dir_all(&temporary);
        }
        #[cfg(not(target_os = "linux"))]
        return Err("atomic replacement of expanded source is currently supported on Linux".into());
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}
