//! Navigation follows recursive source/Z containment, never physical groups.
use crate::source_tree::{self, SourceTree};
use mira_core::Result;
use std::path::Path;

pub fn command(args: &[String]) -> Result<()> {
    let input = Path::new(args.first().ok_or("source needs INPUT and an action")?);
    let action = args.get(1).map(String::as_str).unwrap_or("list");
    if matches!(action, "analyze" | "suggest" | "learn" | "resolve") {
        return draft_action(input, action, &args[2..]);
    }
    let mut tree = source_tree::read_storage(input)?;
    let selector = || args.get(2).map(String::as_str).ok_or("source action needs NAME/PATH".to_string());
    match action {
        "enter" => {
            let view = tree.enter(args.get(2).map(String::as_str).unwrap_or(&tree.root.name))?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string(&view).map_err(|e| e.to_string())?);
            } else {
                println!("{}\n", view.entry.path);
                print!("{}", view.source);
                for child in &view.children { println!("  ↳ {}", child.path); }
            }
        }
        "behavior" => {
            let view = tree.behavior(selector()?)?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string(&view).map_err(|e| e.to_string())?);
            } else {
                println!("{}\n", view.entry.path);
                print!("{}", view.source);
                for child in &view.children { println!("  ↳ {}", child.path); }
            }
        }
        "list" => {
            let parent = args.get(2).map(|name| tree.resolve(name)).transpose()?.unwrap_or(tree.root.id);
            let after = args.get(3).map(|s| s.parse::<u64>()).transpose()
                .map_err(|_| "invalid page cursor")?.unwrap_or(0);
            let mut entries: Vec<_> = tree.enter(&parent.to_string())?.children.into_iter()
                .filter(|e| e.id > after).collect();
            entries.sort_by_key(|e| e.id);
            let more = entries.len() > 24;
            entries.truncate(24);
            println!("{}", serde_json::json!({"parent":parent,"entries":entries,
                "more_after":if more {entries.last().map(|e|e.id)} else {None}}));
        }
        "find" => {
            let query = selector()?.to_lowercase();
            let entries: Vec<_> = tree.entries()
                .into_iter().filter(|e| e.name.to_lowercase().contains(&query)).take(40).collect();
            println!("{}", serde_json::json!({"entries":entries}));
        }
        "read" => {
            let id = tree.resolve(selector()?)?;
            let element = tree.get(id).ok_or("source object missing")?.clone();
            print!("{}", SourceTree { root: element }.assembled()?);
        }
        "text" => print!("{}", tree.assembled()?),
        "compare" => {
            let other = Path::new(args.get(2).ok_or("source compare needs OTHER")?);
            if tree.validate()? != crate::load(other)? { return Err("programs or identities differ".into()); }
            println!("Identical Program: identities, Z graphs, definitions and types preserved");
        }
        "write" => {
            let id = tree.resolve(selector()?)?;
            let replacement = bounded_text(Path::new(args.get(3).ok_or("source write needs PATH REPLACEMENT")?), 4*1024*1024)?;
            tree.replace(id, &replacement).map_err(|e| format!("source object {id}: {e}"))?;
            source_tree::write(input, &tree)?;
            println!("Updated source object; Z containment and program validated");
        }
        "contain" => {
            let output = Path::new(args.get(2).ok_or("source contain needs OUTPUT")?);
            source_tree::write(output, &tree)?;
            println!("Stored recursive source object: {}", output.display());
        }
        _ => return Err("source actions: enter [NAME/PATH], behavior NAME/PATH, list [NAME/PATH [AFTER]], find NAME, read NAME/PATH, text, write NAME/PATH REPLACEMENT, contain OUTPUT, compare OTHER".into()),
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
    let source=source_tree::draft_source(input)?;
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
