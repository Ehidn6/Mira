//! Bounded navigation/editing of recursive source files by existing Program IDs.
use crate::source_tree::{self, SourceTree};
use mira_core::{Id, Result};
use std::path::Path;

pub fn command(args: &[String]) -> Result<()> {
    let input = Path::new(args.first().ok_or("source needs INPUT and an action")?);
    let action = args.get(1).map(String::as_str).unwrap_or("list");
    if matches!(action,"analyze"|"suggest"|"learn"|"resolve") {
        return draft_action(input,action,&args[2..]);
    }
    let identity = |index: usize| -> Result<Id> {
        args.get(index)
            .ok_or("source action needs an identity")?
            .parse()
            .map_err(|_| "invalid source identity".into())
    };
    let stored = input.is_dir() || {
        use std::io::Read;
        let mut header = [0u8; 256];
        let count = std::fs::File::open(input)
            .map_err(|e| e.to_string())?
            .read(&mut header)
            .map_err(|e| e.to_string())?;
        header[..count].iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
    };
    let replacement = if action == "write" {
        use std::io::Read;
        let path = Path::new(args.get(3).ok_or("source write needs ID REPLACEMENT")?);
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 4 * 1024 * 1024 {
            return Err("replacement exceeds source budget".into());
        }
        if stored && source_tree::read_selected(input, identity(2)?)? == text {
            println!("Source identity {} unchanged; no write", identity(2)?);
            return Ok(());
        }
        Some(text)
    } else {
        None
    };
    if stored && action == "files" {
        let groups = source_tree::source_files(input)?;
        let entries = source_tree::catalog(input)?;
        let parents: std::collections::BTreeMap<_,_> = entries.iter().map(|e| (e.id,e.parent)).collect();
        let anchors: std::collections::BTreeMap<_,_> = groups.iter().filter_map(|g| g.ids.first().map(|id| (*id,g.name.clone()))).collect();
        let files: Vec<_> = groups
            .into_iter()
            .map(|file| {
                let anchor = file.ids.first().copied();
                let mut parent = anchor.and_then(|id| parents.get(&id).copied().flatten());
                let mut parent_file = None;
                while let Some(id) = parent {
                    if let Some(name) = anchors.get(&id) { parent_file = Some(name.clone()); break; }
                    parent = parents.get(&id).copied().flatten();
                }
                serde_json::json!({"name":file.name,"elements":file.ids.len(),"anchor":anchor,"parent_file":parent_file})
            })
            .collect();
        println!("{}", serde_json::json!({"files":files}));
        return Ok(());
    }
    if stored && action == "file" {
        print!(
            "{}",
            source_tree::read_file(input, args.get(2).ok_or("source file needs NAME")?)?
        );
        return Ok(());
    }
    if stored && matches!(action, "list" | "find" | "read") {
        if action == "read" {
            print!("{}", source_tree::read_selected(input, identity(2)?)?);
            return Ok(());
        }
        let rows = source_tree::catalog(input)?;
        if action == "find" {
            let query = args.get(2).ok_or("source find needs NAME")?.to_lowercase();
            let found: Vec<_> = rows
                .into_iter()
                .filter(|e| e.name.to_lowercase().contains(&query))
                .take(40)
                .collect();
            println!(
                "{}",
                serde_json::json!({"storage_metadata":true,"entries":found})
            );
        } else {
            let parent = if args.len() > 2 {
                identity(2)?
            } else {
                rows.iter()
                    .find(|e| e.parent.is_none())
                    .ok_or("missing root")?
                    .id
            };
            if !rows.iter().any(|e| e.id == parent) {
                return Err("source parent missing".into());
            }
            let after = args
                .get(3)
                .map(|s| s.parse::<Id>())
                .transpose()
                .map_err(|_| "invalid page cursor")?
                .unwrap_or(0);
            let mut found: Vec<_> = rows
                .into_iter()
                .filter(|e| e.parent == Some(parent) && e.id > after)
                .collect();
            found.sort_by_key(|e| e.id);
            let more = found.len() > 24;
            found.truncate(24);
            println!(
                "{}",
                serde_json::json!({"storage_metadata":true,"parent":parent,"entries":found,"more_after":if more {found.last().map(|e|e.id)} else {None}})
            );
        }
        return Ok(());
    }
    let tree = if stored {
        source_tree::read(input)?
    } else {
        SourceTree::from_source(&crate::source_text(input)?)?
    };
    match action {
        "list" => {
            let parent = if args.len() > 2 { identity(2)? } else { tree.root.id };
            if tree.get(parent).is_none() { return Err("source parent missing".into()); }
            let after = args.get(3).map(|s| s.parse::<Id>()).transpose().map_err(|_| "invalid page cursor")?.unwrap_or(0);
            let mut entries: Vec<_> = tree.list()?.into_iter()
                .filter(|e| e.parent == Some(parent) && e.id > after).collect();
            entries.sort_by_key(|e| e.id);
            let more = entries.len() > 24;
            entries.truncate(24);
            println!("{}", serde_json::json!({"parent":parent,"entries":entries,"more_after":if more { entries.last().map(|e| e.id) } else { None }}));
        }
        "find" => {
            let query = args.get(2).ok_or("source find needs NAME")?.to_lowercase();
            let rows: Vec<_> = tree.list()?.into_iter().filter(|e|e.name.to_lowercase().contains(&query)).take(40).collect();
            println!("{}", serde_json::to_string(&rows).map_err(|e|e.to_string())?);
        }
        "read" => print!("{}", tree.read(identity(2)?)?),
        "text" => print!("{}", tree.assembled()?),
        "compare" => {
            let other=Path::new(args.get(2).ok_or("source compare needs OTHER")?);
            if tree.validate()? != crate::load(other)? {return Err("programs or identities differ".into());}
            println!("Identical Program: identities, graphs, definitions and types preserved");
        }
        "write" => {
            if !input.is_dir() && !std::fs::read_to_string(input).map_err(|e|e.to_string())?.trim_start().starts_with('{') {
                return Err("split or pack a text program before editing contained files".into());
            }
            let id = identity(2)?;
            let mut staged = tree;
            staged.replace(id, replacement.as_deref().ok_or("missing replacement")?).map_err(|e|format!("source identity {id}: {e}"))?;
            source_tree::write(input, &staged)?;
            println!("Updated source identity {id}; program validated");
        }
        "regroup" => {
            let plan=Path::new(args.get(2).ok_or("source regroup needs PLAN [OUTPUT]")?);
            let bytes=std::fs::read(plan).map_err(|e|e.to_string())?;
            if bytes.len()>1024*1024 {return Err("group plan exceeds budget".into());}
            let groups:Vec<source_tree::SourceGroup>=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
            let output=args.get(3).map(Path::new).unwrap_or(input);
            source_tree::write_grouped(output,&tree,&groups)?;
            println!("Grouped physical source; logical identities and Z tree retained");
        }
        "organize" => {
            let output = args.get(2).map(Path::new).unwrap_or(input);
            let groups = source_tree::composition_groups(&tree);
            source_tree::write_grouped(output, &tree, &groups)?;
            println!("Stored {} source files following existing composition", groups.len());
        }
        "pack" | "split" | "unpack" => {
            let output = Path::new(args.get(2).ok_or("source storage action needs OUTPUT")?);
            if stored {
                let groups=source_tree::source_files(input)?;
                if action=="pack"{source_tree::write_packed(output,&tree,&groups)?;}else{source_tree::write_grouped(output,&tree,&groups)?;}
            }else if action=="pack"{source_tree::write(output,&tree)?;}else{source_tree::write_expanded(output,&tree)?;}
            println!("Stored recursive source: {}",output.display());
        }
        _ => return Err("source actions: files, file NAME, list [PARENT [AFTER]], find NAME, read ID, text, write ID REPLACEMENT, pack OUTPUT, split OUTPUT, organize [OUTPUT], regroup PLAN [OUTPUT]".into()),
    }
    Ok(())
}

fn bounded_text(path:&Path,limit:usize)->Result<String> {
    use std::io::Read;
    let mut text=String::new();
    std::fs::File::open(path).map_err(|e|e.to_string())?.take(limit as u64+1)
        .read_to_string(&mut text).map_err(|e|e.to_string())?;
    if text.len()>limit{return Err("analysis input budget exceeded".into());}
    Ok(text)
}
fn draft_action(input:&Path,action:&str,args:&[String])->Result<()> {
    let source=if input.is_dir(){source_tree::draft_source(input)?}else{
        let text=bounded_text(input,4*1024*1024)?;
        if text.trim_start().starts_with('{'){source_tree::draft_source(input)?}else{text}
    };
    let mut draft=crate::syntax::parse_draft(&source)?;
    let report=crate::inference::analyze(&mut draft,&[],false)?;
    let argument=|index:usize|->Result<&str>{args.get(index).map(String::as_str).ok_or_else(||format!("source {action}: missing argument"))};
    match action {
        "analyze"=>{
            let signatures:Vec<_>=mira_core::code::symbols(&draft.code_modules).into_iter().map(|(name,d)|
                serde_json::json!({"name":name,"definition":d.id,"parameters":d.parameters,"result":d.output})).collect();
            println!("{}",serde_json::json!({"status":"draft","signatures":signatures,"requirements":report.requirements}));
        },
        "resolve"=>{
            let choices:Vec<crate::inference::Choice>=serde_json::from_str(&bounded_text(Path::new(argument(0)?),64*1024)?).map_err(|e|e.to_string())?;
            let checked=crate::inference::checked(&draft,&choices)?;
            let output=Path::new(argument(1)?);
            if output==input{return Err("resolve needs a distinct output; review the resolved source before replacing the original".into());}
            crate::save(output,&checked)?;
            println!("{}",serde_json::json!({"status":"checked","output":output}));
        },
        "suggest"=>{
            let model=crate::system::load(Path::new(argument(0)?))?;
            let mut suggestions=Vec::new();
            for requirement in &report.requirements {
                if requirement.candidates.len()>1 {
                    suggestions.push(serde_json::json!({"target":requirement.target,
                        "ranked":crate::assist::rank_types(&model,&draft,&requirement.target)?}));
                }
            }
            println!("{}",serde_json::json!({"status":"proposals","suggestions":suggestions}));
        },
        "learn"=>{
            let path=Path::new(argument(0)?);
            let choice:crate::inference::Choice=serde_json::from_str(&bounded_text(Path::new(argument(1)?),64*1024)?).map_err(|e|e.to_string())?;
            let accepted=match argument(2)?{"accepted"=>true,"rejected"=>false,_=>return Err("learn expects accepted or rejected".into())};
            let mut model=if path.exists(){crate::system::load(path)?}else{crate::assist::new()?};
            crate::assist::observe_type(&mut model,&draft,&choice.target,choice.ty,accepted)?;
            crate::system::save(path,&model)?;
            println!("{}",serde_json::json!({"status":"observed","model":path}));
        },
        _=>unreachable!(),
    }
    Ok(())
}
