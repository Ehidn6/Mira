//! Exercise the Mira module driver without Rust parsing/signature collection,
//! relocation patching or ELF serialization of the program under compilation.
use super::module::Workspace;
use super::*;
use std::{os::unix::fs::PermissionsExt, process::Command};
#[test]
fn mira_compiles_links_and_packages_a_module_from_unmodified_source() -> Result<()> {
    let mut w = Workspace::new()?;
    let source = include_str!("../../../language/examples/bootstrap-module");
    let image = w.compile(source, 40)?;
    assert_eq!(w.field("error")?, 0, "byte {}", w.field("error_offset")?);
    assert_eq!(w.field("count")?, 3);
    assert!(w.field("relocations")? >= 3);
    assert_eq!(&image[..4], b"\x7fELF");
    let original = crate::frontend::parse_seed(source).unwrap();
    for index in 0..w.field("count")? as usize {
        let row = index * 128;
        let length = w.arena.get(w.symbols, row).unwrap() as usize;
        let name: String = (0..length)
            .map(|i| w.arena.get(w.symbols, row + 32 + i).unwrap() as u8 as char)
            .collect();
        let definition = original
            .code_modules
            .iter()
            .find(|d| d.name == name)
            .unwrap();
        assert_eq!(
            w.arena.get(w.symbols, row + 19).unwrap(),
            definition.id as i64
        );
    }
    let dir = std::env::temp_dir().join(format!("mira-module-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    std::fs::write(&file, image).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(&file).output().unwrap();
    assert_eq!(
        result.status.code(),
        Some(88),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let arrays = include_str!("../../../language/examples/bootstrap-arrays");
    let model = crate::frontend::parse_seed(arrays).unwrap();
    assert_eq!(crate::behavior::execute(&model, 3, &[]).unwrap(), 83);
    let image = w.compile(arrays, 3)?;
    assert_eq!(
        w.field("error")?,
        0,
        "array compiler byte {}",
        w.field("error_offset")?
    );
    std::fs::write(&file, image).unwrap();
    assert_eq!(Command::new(&file).status().unwrap().code(), Some(83));
    let records = include_str!("../../../language/examples/bootstrap-records");
    let model = crate::frontend::parse_seed(records).unwrap();
    assert_eq!(crate::behavior::execute(&model, 6, &[]).unwrap(), 1128);
    let image = w.compile(records, 6)?;
    assert_eq!(
        w.field("error")?,
        0,
        "record compiler byte {}",
        w.field("error_offset")?
    );
    std::fs::write(&file, image).unwrap();
    assert_eq!(
        Command::new(&file).status().unwrap().code(),
        Some(1128 % 256)
    );
    let addresses = include_str!("../../../language/examples/bootstrap-addresses");
    let model = crate::frontend::parse_seed(addresses).unwrap();
    assert_eq!(crate::behavior::execute(&model, 1, &[]).unwrap(), 29);
    let image = w.compile(addresses, 1)?;
    assert_eq!(
        w.field("error")?,
        0,
        "address compiler byte {}",
        w.field("error_offset")?
    );
    std::fs::write(&file, image).unwrap();
    assert_eq!(Command::new(&file).status().unwrap().code(), Some(29));
    let nested = include_str!("../../../language/examples/bootstrap-nested");
    let model = crate::frontend::parse_seed(nested).unwrap();
    assert_eq!(crate::behavior::execute(&model, 1, &[]).unwrap(), 6);
    let image = w.compile(nested, 1)?;
    assert_eq!(
        w.field("error")?,
        0,
        "nested compiler byte {}",
        w.field("error_offset")?
    );
    std::fs::write(&file, image).unwrap();
    assert_eq!(Command::new(&file).status().unwrap().code(), Some(6));
    // A long flat block is not a deeply nested program. Keep its sequence tree
    // balanced instead of rejecting it at the expression recursion guard.
    let mut flat = "run @1()->i64 =\n  result = 0\n".to_string();
    for _ in 0..128 {
        flat.push_str("  if true\n    result += 1\n");
    }
    flat.push_str("  result\n");
    let image = w.compile(&flat, 1)?;
    assert_eq!(
        w.field("error")?,
        0,
        "flat block byte {}",
        w.field("error_offset")?
    );
    std::fs::write(&file, image).unwrap();
    assert_eq!(Command::new(&file).status().unwrap().code(), Some(128));
    for (source, status) in [
        (
            "run @1()->i64 = sys.offset(sys.addr(0),-1) == sys.addr(0) ? 1 : 0\n",
            70,
        ),
        (
            "run @1()->i64 = sys.offset(sys.addr(-1),1) == sys.addr(0) ? 1 : 0\n",
            70,
        ),
        ("run @1()->i64 = other()\nother @2()->i64 = 1 / 0\n", 71),
        ("run @1()->i64 = 9223372036854775807 + 1\n", 70),
        ("type V @2 = [i64,2]\nrun @1()->i64 = V()[2]\n", 76),
        ("type V @2 = [i64,2]\nrun @1()->i64 = V()[-1]\n", 76),
        (
            "type V @2 = [i64,65536]\nrun @1()->i64 =\n  v = V()\n  v[0]\n",
            72,
        ),
        ("run @1()->i64 =\n  while true\n    ()\n  0\n", 72),
        (
            "run @1()->i64 = false and failed() ? 0 : 7\nfailed @2()->bool = 1 / 0 == 0\n",
            7,
        ),
    ] {
        let image = w.compile(source, 1)?;
        assert_eq!(w.field("error")?, 0);
        std::fs::write(&file, image).unwrap();
        let result = Command::new(&file).output().unwrap();
        assert_eq!(result.status.code(), Some(status), "{source}");
    }
    // With enough fuel, exhausting the region must trap rather than touch the stack.
    // Calls returning scalar values must release their private arrays each time.
    for (source, expected) in [
        (
            "type V @2 = [i64,65536]\nrun @1()->i64 =\n  v = V()\n  w = V()\n  v[0] + w[0]\n",
            75,
        ),
        (
            "type V @2 = [i64,2048]\nread @3()->i64 = V()[0]\nrun @1()->i64 =\n  result = 0\n  for index in 0..128\n    result += read()\n  result + 19\n",
            19,
        ),
    ] {
        let image = w.compile_with_budget(source, 1, 1_000_000)?;
        assert_eq!(w.field("error")?, 0, "{source}");
        std::fs::write(&file, image).unwrap();
        assert_eq!(
            Command::new(&file).status().unwrap().code(),
            Some(expected),
            "{source}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
    // Reuse the same buffers: errors cannot publish a stale image or entry.
    for (source, entry, error) in [
        ("a @1()->i64 = missing()\n", 1, 2),
        ("a @1()->i64 = 1\nb @1()->i64 = 2\n", 1, 9),
        ("a @1()->i64 = b(true)\nb @2(x:i64)->i64 = x\n", 1, 6),
        ("a @1()->i64 = 1\n", 99, 2),
        ("a @1(x:i64)->i64 = x\n", 1, 2),
        ("a @1(x)->i64 = x\n", 1, 1),
        ("a @1()->i64 = 1\na @2()->i64 = 2\n", 1, 9),
        ("", 1, 4),
        (
            "type V @2 = [bool,2]\nrun @1()->i64 =\n  v = V(true,false)\n  v[0] = 1\n  0\n",
            1,
            6,
        ),
        (
            "type V @2 = [i64,2]\ntype W @3 = [i64,2]\nf @4(v:V)->i64 = v[0]\nrun @1()->i64 = f(W())\n",
            1,
            6,
        ),
        ("type V @2 = [i64,2]\nrun @1()->i64 = V(1)[0]\n", 1, 6),
        ("type V @1 = [i64,2]\nrun @1()->i64 = 0\n", 1, 9),
        (
            "struct V @2:\n  field: bool\nrun @1()->i64 = V(1).field ? 1 : 0\n",
            1,
            6,
        ),
        (
            "struct V @2:\n  field: i64\nrun @1()->i64 = V().missing\n",
            1,
            2,
        ),
        (
            "struct V @2:\n  field: i64\n  field: bool\nrun @1()->i64 = 0\n",
            1,
            9,
        ),
        (
            "type A @2 = [i64,1]\nstruct V @3:\n  field: A\nrun @1()->i64 = V().field[0]\n",
            1,
            6,
        ),
        // A borrowed object must not acquire a reference to a callee's region.
        (
            "type A @2 = [i64,1]\nstruct V @3:\n  field: A\nchange @4(v:V)->unit =\n  v.field = A()\nrun @1()->i64 =\n  v = V(A())\n  change(v)\n  v.field[0]\n",
            1,
            6,
        ),
        (
            "type A @2 = [i64,1]\nrun @1()->i64 =\n  v = A()\n  v.len = 2\n  0\n",
            1,
            6,
        ),
        (
            "first @1()->i64 =\n  hidden()\n  hidden @2()->i64 = 5\nsecond @3()->i64 = hidden()\n",
            1,
            2,
        ),
        (
            "first @1()->i64 =\n  0\n  type Hidden @2 = [i64,1]\nsecond @3(value:Hidden)->i64 = 0\n",
            1,
            2,
        ),
        ("`a` @1()->i64 = 1\n", 1, 2),
        ("a @1(x:i64)->i64 = `x`\n", 1, 2),
    ] {
        assert!(w.compile(source, entry)?.is_empty());
        assert_eq!(w.field("error")?, error, "{source}");
        assert_eq!(w.field("size")?, 0);
        assert_eq!(w.field("entry")?, -1);
    }
    Ok(())
}

#[test]
#[ignore = "complete typed compiler source, native rebuild and byte equality in UEFI"]
fn mira_compiles_its_typed_compiler_source_without_word_lowering() -> Result<()> {
    let compiled = super::module::compile_own()?;
    eprintln!(
        "compiler source {} bytes; machine code {} bytes",
        compiled.source.len(),
        compiled.code.len()
    );
    let exports = compiled
        .exports
        .iter()
        .map(|(name, entry)| (name.clone(), entry.offset))
        .collect();
    super::selftest::regenerate_native(compiled.source, &compiled.code, &exports);
    Ok(())
}

#[test]
fn mira_module_compiler_executes_f32_and_rejects_invalid_numeric_operations() -> Result<()> {
    let mut w = Workspace::new()?;
    let dir = std::env::temp_dir().join(format!("mira-float-module-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    let example = include_str!("../../../language/examples/bootstrap-floats");
    let original = crate::frontend::parse_seed(example)?;
    assert_eq!(crate::behavior::execute(&original, 4, &[])?, 37);
    for (source, entry, expected) in [
        (example, 4, 37),
        ("run @1()->i64 = math.bits(0.1) == 1036831949 and 2.0 > 1.0 and 1.0 <= 1.0 and 2.0 != 1.0 and 1.0 < 2.0 and 2.0 >= 2.0 ? 19 : 0\n", 1, 19),
        ("run @1()->i64 = math.bits(-(-0.0)) == 0 and math.bits(+1.0) == 1065353216 ? 23 : 0\n", 1, 23),
        ("run @1()->i64 = math.bits(1.0 / -0.0)\n", 1, 71),
        ("run @1()->i64 = math.bits(3e38 * 2.0)\n", 1, 74),
        ("run @1()->i64 = math.bits(math.sqrt(-1.0))\n", 1, 74),
        ("run @1()->i64 = math.bits(math.from_bits(2139095040))\n", 1, 74),
        ("run @1()->i64 = math.bits(math.from_bits(4294967296))\n", 1, 74),
        ("run @1()->i64 = math.bits(1e-45 / 2.0)\n", 1, 0),
    ] {
        let image = w.compile(source, entry)?;
        assert_eq!(w.field("error")?, 0, "byte {}: {source}", w.field("error_offset")?);
        std::fs::write(&file, image).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = Command::new(&file).output().unwrap();
        assert_eq!(result.status.code(), Some(expected), "{source}: {}", String::from_utf8_lossy(&result.stderr));
    }
    for source in [
        "run @1()->f32 = 1 + 1.0\n",
        "run @1()->f32 = 1.0 % 1.0\n",
        "run @1()->f32 = 1.0 & 1.0\n",
        "run @1()->f32 = 1e999\n",
        "run @1()->i64 = math.bits(1)\n",
        "run @1()->f32 =\n  x = 1.0\n  x %= 1.0\n  x\n",
    ] {
        let image = w.compile(source, 1)?;
        assert_ne!(w.field("error")?, 0, "{source}");
        assert!(image.is_empty(), "{source}");
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn mira_module_compiler_checks_tensor_shapes_and_executes_bulk_operations() -> Result<()> {
    let mut w = Workspace::new()?;
    let dir = std::env::temp_dir().join(format!("mira-tensor-module-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    let example = include_str!("../../../language/examples/bootstrap-tensors");
    let model = crate::frontend::parse_seed(example)?;
    assert_eq!(crate::behavior::execute(&model, 4, &[])?, 41);
    for (source, expected, budget) in [
        (example, 41, 10000),
        ("type A @2 = tensor[f32,2,3]\ntype B @3 = tensor[f32,3,2]\ntype O @4 = tensor[f32,2,2]\nrun @1()->i64 =\n  a = A(1.0,2.0,3.0,4.0,5.0,6.0)\n  b = B(7.0,8.0,9.0,10.0,11.0,12.0)\n  o = O()\n  tensor.matmul_into(o,a,b)\n  o[0] == 58.0 and o[1] == 64.0 and o[2] == 139.0 and o[3] == 154.0 and tensor.sum(o) == 415.0 ? 43 : 0\n", 43, 10000),
        ("type A @2 = tensor[f32,1,3]\ntype B @3 = tensor[f32,3,1]\ntype O @4 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A(16777216.0,1.0,-16777216.0)\n  b = B(1.0,1.0,1.0)\n  o = O()\n  tensor.matmul_into(o,a,b)\n  math.bits(o[0]) == 0 ? 19 : 0\n", 19, 10000),
        ("type A @2 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A(3e38)\n  b = A(2.0)\n  o = A()\n  tensor.matmul_into(o,a,b)\n  0\n", 74, 10000),
        ("type A @2 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A(1.0)\n  b = A(2.0)\n  tensor.matmul_into(a,a,b)\n  0\n", 74, 10000),
        ("type A @2 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A(1.0)\n  b = A(2.0)\n  tensor.matmul_into(b,a,b)\n  0\n", 74, 10000),
        ("type A @2 = tensor[f32,1,64]\ntype B @3 = tensor[f32,64,1]\ntype O @4 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A()\n  b = B()\n  o = O()\n  9\n", 9, 180),
        ("type A @2 = tensor[f32,1,64]\ntype B @3 = tensor[f32,64,1]\ntype O @4 = tensor[f32,1,1]\nrun @1()->i64 =\n  a = A()\n  b = B()\n  o = O()\n  tensor.matmul_into(o,a,b)\n  9\n", 72, 180),
        ("type V @2 = tensor[bool,2]\ntype A @3 = [bool,2]\nrun @1()->i64 =\n  a = A(true,false)\n  b = V()\n  tensor.copy_into(b,a)\n  b[0] and not b[1] ? 7 : 0\n", 7, 10000),
        ("type V @2 = [f32,0]\nrun @1()->i64 =\n  v = V()\n  tensor.fill(v,1.0)\n  math.bits(tensor.sum(v))\n", 0, 10000),
        ("type V @2 = tensor[f32,2,2,2,2,2,2,2,2]\nrun @1()->i64 =\n  v = V()\n  tensor.fill(v,1.0)\n  tensor.sum(v) == 256.0 ? 23 : 0\n", 23, 10000),
        ("type V @2 = tensor[f32,2]\nrun @1()->i64 =\n  v = V(3e38,3e38)\n  math.bits(tensor.sum(v))\n", 74, 10000),
        ("type V @2 = tensor[f32,2]\nrun @1()->i64 =\n  v = V(1.0,0.0)\n  tensor.div_into(v,v,v)\n  0\n", 71, 10000),
        ("type V @2 = tensor[f32,64]\nrun @1()->i64 =\n  v = V()\n  9\n", 9, 100),
        ("type V @2 = tensor[f32,64]\nrun @1()->i64 =\n  v = V()\n  tensor.fill(v,1.0)\n  9\n", 72, 100),
    ] {
        let image = w.compile_with_budget(source, if source == example {4} else {1}, budget)?;
        assert_eq!(w.field("error")?, 0, "byte {}: {source}", w.field("error_offset")?);
        std::fs::write(&file, image).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = Command::new(&file).output().unwrap();
        assert_eq!(result.status.code(), Some(expected), "{source}: {}", String::from_utf8_lossy(&result.stderr));
    }
    for source in [
        "type A @2 = tensor[f32,2,3]\nrun @1()->unit = tensor.matmul_into(A(),A(),A())\n",
        "type A @2 = tensor[f32,2,3]\ntype B @3 = tensor[f32,3,2]\nrun @1()->unit = tensor.matmul_into(A(),A(),B())\n",
        "type A @2 = [f32,4]\nrun @1()->unit = tensor.matmul_into(A(),A(),A())\n",
        "type A @2 = tensor[i64,2,2]\nrun @1()->unit = tensor.matmul_into(A(),A(),A())\n",
        "type V @2 = tensor[f32,0]\nrun @1()->i64 = 0\n",
        "type V @2 = tensor[f32,1024,1025]\nrun @1()->i64 = 0\n",
        "type V @2 = tensor[f32,1,1,1,1,1,1,1,1,1]\nrun @1()->i64 = 0\n",
        "type M @2 = tensor[f32,2,2]\ntype V @3 = tensor[f32,4]\nrun @1()->unit = tensor.copy_into(M(),V())\n",
        "type M @2 = tensor[f32,2,2]\ntype V @3 = tensor[f32,4]\nrun @1()->unit = tensor.add_into(M(),M(),V())\n",
        "type V @2 = tensor[i64,2]\nrun @1()->f32 = tensor.sum(V())\n",
        "type V @2 = tensor[f32,2]\nrun @1()->unit = tensor.fill(V(),1)\n",
        "type V @2 = tensor[f32,2]\nrun @1()->unit = tensor.scale_into(V(),V(),true)\n",
    ] {
        assert!(crate::frontend::parse_seed(source).is_err(), "host accepted: {source}");
        let image = w.compile(source, 1)?;
        assert_ne!(w.field("error")?, 0, "module accepted: {source}");
        assert!(image.is_empty(), "{source}");
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn mira_module_compiler_executes_typed_relations() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-relations");
    let unmasked = example.replace("  active: EdgeActive\n", "")
        .replace("GraphLinks(mask,to,", "GraphLinks(to,")
        .replace("EdgeIndices(0,2,1,999)", "EdgeIndices(0,2,1,0)")
        .replace("EdgeIndices(1,0,1,-999)", "EdgeIndices(1,0,1,0)")
        .replace("tensor.sum(output) == 21.0", "tensor.sum(output) == 24.0")
        .replace("output[6] == 0.0", "output[6] == 1.0").replace("output[7] == 0.0", "output[7] == 2.0")
        .replace("targets[0] == 5.0", "targets[0] == 6.0").replace("targets[1] == 6.0", "targets[1] == 8.0");
    let dir = std::env::temp_dir().join(format!("mira-relations-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    for source in [example, &unmasked] {
        let model = crate::frontend::parse_seed(source)?;
        assert_eq!(crate::behavior::execute(&model,108,&[])?,53);
        let image = w.compile(source,108)?;
        assert_eq!(w.field("error")?,0,"byte {}",w.field("error_offset")?);
        std::fs::write(&file,image).unwrap();
        std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(Command::new(&file).status().unwrap().code(),Some(53));
    }
    for source in [
        example.replace("EdgeIndices(0,2,1,999)","EdgeIndices(0,2,3,999)"),
        example.replace("EdgeIndices(1,0,1,-999)","EdgeIndices(1,0,2,-999)"),
        example.replace("tensor[f32,3,2]","tensor[f32,4,2]")
            .replace("NodeFeatures(1.0,2.0,3.0,4.0,5.0,6.0)","NodeFeatures(1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0)")
            .replace("  messages(output,links)","  relation.messages_into(nodes,links)"),
        example.replace("type TargetFeatures @102 = tensor[f32,2,2]","type TargetFeatures @102 = tensor[f32,4,2]")
            .replace("  aggregate(targets,links,output)","  relation.aggregate_into(targets,links,targets)"),
    ] {
        let model = crate::frontend::parse_seed(&source)?;
        assert!(crate::behavior::execute(&model,108,&[]).is_err());
        let image = w.compile(&source,108)?;
        assert_eq!(w.field("error")?,0,"byte {}",w.field("error_offset")?);
        std::fs::write(&file,image).unwrap();
        assert_eq!(Command::new(&file).status().unwrap().code(),Some(74));
    }
    for source in [
        example.replace("type EdgeActive @104 = [bool,4]","type EdgeActive @104 = tensor[bool,2,2]"),
        example.replace("type EdgeFeatures @106 = tensor[f32,4,1]","type EdgeFeatures @106 = tensor[f32,3,1]"),
        format!("type TargetClone @111 = tensor[f32,2,2]\n{}", example.replace("  aggregate(targets,links,output)","  relation.aggregate_into(TargetClone(),links,output)")),
    ] {
        assert!(crate::frontend::parse_seed(&source).is_err());
        assert!(w.compile(&source,108)?.is_empty());
        assert_ne!(w.field("error")?,0);
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn mira_module_compiler_executes_relation_mutation_and_neighbors() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-plasticity");
    let dir = std::env::temp_dir().join(format!("mira-plasticity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    let unmasked = example.replace("  active: PMask\n", "")
        .replace("PLinks(mask,to,", "PLinks(to,");
    // Execute only a replacement entry: unused add/remove declarations are removed
    // because both compilers correctly reject mutation without an active mask.
    let unmasked = unmasked.lines().filter(|line| !line.starts_with("add @") && !line.starts_with("remove @"))
        .collect::<Vec<_>>().join("\n");
    let head = unmasked.split("plastic_run @131").next().unwrap();
    let unmasked = format!("{head}plastic_run @131()->i64 =\n  from = PIndices(0,2,1,0)\n  to = PIndices(1,0,1,0)\n  links = PLinks(to,PNodes(),PFeatures(),from,PTargets())\n  output = PIndices(77,77,77,77)\n  found = neighbors(output,links,1)\n  count(links) == 4 and found == 2 and output[0] == 0 and output[1] == 1 and output[2] == 77 ? 61 : 0\n");
    let empty = "type N @121 = tensor[f32,1,1]\ntype E @123 = [i64,0]\ntype B @124 = [bool,0]\nstruct R @126:\n  sources: N\n  targets: N\n  source_indices: E\n  target_indices: E\n  active: B\nplastic_run @131()->i64 =\n  links = R(N(),N(),E(),E(),B())\n  output = E()\n  relation.active_count(links) == 0 and relation.add_edge(links,0,0) == -1 and relation.neighbors_into(output,links,0) == 0 ? 61 : 0\n";
    for source in [example, &unmasked, empty] {
        let model = crate::frontend::parse_seed(source)?;
        assert_eq!(crate::behavior::execute(&model,131,&[])?,61);
        let image = w.compile(source,131)?;
        assert_eq!(w.field("error")?,0,"byte {}",w.field("error_offset")?);
        std::fs::write(&file,image).unwrap();
        std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(Command::new(&file).status().unwrap().code(),Some(61));
    }
    let head = example.split("plastic_run @131").next().unwrap();
    let setup = "  from = PIndices(0,0,999,999)\n  to = PIndices(1,0,-999,-999)\n  mask = PMask(true,false,false,false)\n  links = PLinks(mask,to,PNodes(),PFeatures(),from,PTargets())\n  output = PIndices(77,77,77,77)\n";
    for action in [
        "  unused = add(links,-1,0)\n", "  unused = add(links,3,0)\n",
        "  unused = add(links,0,2)\n", "  unused = add(links,0,-1)\n",
        "  unused = remove(links,4)\n", "  unused = remove(links,-1)\n",
        "  unused = neighbors(output,links,2)\n", "  unused = neighbors(output,links,-1)\n",
        "  unused = neighbors(from,links,1)\n", "  unused = neighbors(to,links,1)\n",
        "  from[0] = 3\n  unused = count(links)\n",
        "  to[0] = 2\n  unused = add(links,0,0)\n",
        "  from[0] = -1\n  unused = neighbors(output,links,1)\n",
        "  links = PLinks(mask,from,PNodes(),PFeatures(),from,PTargets())\n  unused = add(links,1,0)\n",
    ] {
        let source = format!("{head}plastic_run @131()->i64 =\n{setup}{action}  0\n");
        let model = crate::frontend::parse_seed(&source)?;
        assert!(crate::behavior::execute(&model,131,&[]).is_err(),"{action}");
        let image = w.compile(&source,131)?;
        assert_eq!(w.field("error")?,0,"{action}, byte {}",w.field("error_offset")?);
        std::fs::write(&file,image).unwrap();
        assert_eq!(Command::new(&file).status().unwrap().code(),Some(74),"{action}");
    }
    for bad in [
        example.replace("type PIndices @123 = [i64,4]","type PIndices @123 = [bool,4]"),
        format!("type Short @136 = [i64,3]\n{head}plastic_run @131()->i64 = relation.neighbors_into(Short(),PLinks(PMask(),PIndices(),PNodes(),PFeatures(),PIndices(),PTargets()),0)\n"),
        example.replace("relation.add_edge(links,from,to)","relation.add_edge(links,true,to)"),
        example.replace("  active: PMask\n", "").replace("PLinks(mask,to,", "PLinks(to,"),
    ] {
        assert!(crate::frontend::parse_seed(&bad).is_err());
        assert!(w.compile(&bad,131)?.is_empty());
        assert_ne!(w.field("error")?,0);
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn mira_module_compiler_executes_indexed_tensors_and_normalization() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-indexed");
    let dir = std::env::temp_dir().join(format!("mira-indexed-{}",std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("program");
    let cases = [
        (example.to_owned(),148,67),
        (example.replace("DenseIndices(2,0,2,1)","DenseIndices(2,0,2,3)"),148,74),
        (example.replace("DenseIndices(1,0,1,0)","DenseIndices(1,0,1,-1)"),148,74),
        ("type M @2 = tensor[f32,2,2]\ntype I @3 = [i64,2]\nrun @1()->i64 =\n  m = M(1.0,2.0,3.0,4.0)\n  tensor.gather_into(m,m,I(0,1))\n  0\n".into(),1,74),
        ("type M @2 = tensor[f32,2,2]\ntype I @3 = [i64,2]\nrun @1()->i64 =\n  m = M(1.0,2.0,3.0,4.0)\n  tensor.aggregate_into(m,m,I(0,1))\n  0\n".into(),1,74),
        ("type M @2 = tensor[f32,1,2]\nrun @1()->i64 =\n  m = M(1e-45,0.0)\n  tensor.normalize_rows_into(m,m)\n  tensor.sum(m) == 0.0 ? 13 : 0\n".into(),1,13),
        ("type M @2 = tensor[f32,1,2]\nrun @1()->i64 =\n  m = M(3e38,0.0)\n  tensor.normalize_rows_into(m,m)\n  0\n".into(),1,74),
    ];
    for (source,entry,expected) in cases {
        let model = crate::frontend::parse_seed(&source)?;
        if expected == 74 { assert!(crate::behavior::execute(&model,entry as u64,&[]).is_err()); }
        else { assert_eq!(crate::behavior::execute(&model,entry as u64,&[])?,expected); }
        let image = w.compile(&source,entry)?;
        assert_eq!(w.field("error")?,0,"byte {}: {source}",w.field("error_offset")?);
        std::fs::write(&file,image).unwrap();
        std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(Command::new(&file).status().unwrap().code(),Some(expected as i32),"{source}");
    }
    for source in [
        "type M @2 = tensor[f32,2,2]\ntype I @3 = [i64,3]\nrun @1()->unit = tensor.gather_into(M(),M(),I())\n",
        "type M @2 = tensor[f32,2,2]\ntype I @3 = [bool,2]\nrun @1()->unit = tensor.aggregate_into(M(),M(),I())\n",
        "type M @2 = tensor[f32,4]\nrun @1()->unit = tensor.normalize_rows_into(M(),M())\n",
        "type M @2 = tensor[i64,2,2]\nrun @1()->unit = tensor.normalize_rows_into(M(),M())\n",
    ] {
        assert!(crate::frontend::parse_seed(source).is_err());
        assert!(w.compile(source,1)?.is_empty());
        assert_ne!(w.field("error")?,0);
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn mira_module_compiler_checks_raw_memory_signatures() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-memory");
    let model = crate::frontend::parse_seed(example)?;
    assert_eq!(crate::behavior::execute(&model,152,&[])?,73);
    let image = w.compile(example,152)?;
    assert_eq!(w.field("error")?,0,"byte {}",w.field("error_offset")?);
    let path = std::env::temp_dir().join(format!("mira-memory-{}",std::process::id()));
    std::fs::write(&path,image).unwrap();
    std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(Command::new(&path).status().unwrap().code(),Some(73));
    std::fs::remove_file(&path).unwrap();
    for source in [
        "run @1()->i64 = sys.load8(0)\n",
        "run @1()->i64 = sys.load16(true)\n",
        "run @1()->bool = sys.load32(sys.addr(0))\n",
        "run @1()->i64 = sys.load64(sys.addr(0),1)\n",
        "run @1()->unit = sys.store16(sys.addr(0),true)\n",
        "run @1()->unit = sys.store32(0,1)\n",
        "run @1()->i64 = sys.store64(sys.addr(0),1)\n",
        "run @1()->i64 = sys.address_value(1)\n",
    ] {
        assert!(crate::frontend::parse_seed(source).is_err(),"{source}");
        assert!(w.compile(source,1)?.is_empty(),"{source}");
        assert_ne!(w.field("error")?,0);
    }
    Ok(())
}

#[test]
fn mira_module_compiler_resolves_qualified_definition_paths() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-paths");
    let model = crate::frontend::parse_seed(example)?;
    assert_eq!(crate::behavior::execute(&model,175,&[])?,19);
    let image = w.compile(example,175)?;
    assert_eq!(w.field("error")?,0,"byte {}",w.field("error_offset")?);
    let path = std::env::temp_dir().join(format!("mira-paths-{}",std::process::id()));
    std::fs::write(&path,image).unwrap();
    std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(Command::new(&path).status().unwrap().code(),Some(19));
    std::fs::remove_file(&path).unwrap();
    // The explicit path and closest lexical definition must agree with Core.
    let alternate = example.replace("outer.inner.leaf(pair.value)","outer.branch.leaf(pair.value)");
    let model = crate::frontend::parse_seed(&alternate)?;
    assert_eq!(crate::behavior::execute(&model,175,&[])?,29);
    let image = w.compile(&alternate,175)?;
    assert_eq!(w.field("error")?,0);
    std::fs::write(&path,image).unwrap();
    std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(Command::new(&path).status().unwrap().code(),Some(29));
    std::fs::remove_file(&path).unwrap();
    for source in [
        example.replace("outer.inner.leaf(pair.value)","leaf(pair.value)"),
        example.replace("outer.inner.leaf(pair.value)","outer.leaf(pair.value)"),
        example.replace("outer.inner.leaf(pair.value)","outer.branch.inner(pair.value)"),
        example.replace("outer.inner.leaf(pair.value)","outer.inner.missing(pair.value)"),
        example.replace("outer.inner.leaf(pair.value)","outer.inner.leaf(true)"),
    ] {
        assert!(crate::frontend::parse_seed(&source).is_err());
        assert!(w.compile(&source,175)?.is_empty());
        assert_ne!(w.field("error")?,0);
    }
    Ok(())
}

#[test]
fn mira_module_compiler_returns_borrowed_objects_and_rejects_temporary_escape() -> Result<()> {
    let mut w = Workspace::new()?;
    let example = include_str!("../../../language/examples/bootstrap-borrowed");
    let model = crate::frontend::parse_seed(example)?;
    assert_eq!(crate::behavior::execute(&model, 186, &[])?, 108);
    let image = w.compile(example, 186)?;
    assert_eq!(w.field("error")?, 0, "byte {}", w.field("error_offset")?);
    let path = std::env::temp_dir().join(format!("mira-borrowed-{}", std::process::id()));
    std::fs::write(&path, image).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(Command::new(&path).status().unwrap().code(), Some(108));
    std::fs::remove_file(&path).unwrap();
    let prefix = "type V @1 = [i64,2]\nidentity @2(value:V)->V = value\n";
    for body in [
        "bad @3(value:V)->V = V()\n",
        "bad @3(value:V)->V = identity(V())\n",
        "bad @3(value:V)->V = true ? value : V()\n",
        "bad @3(value:V)->V =\n  alias = value\n  alias = V()\n  alias\n",
        "bad @3(value:V)->V =\n  alias = value\n  for step in 0..1\n    alias = V()\n  alias\n",
        "bad @3()->V = V()\n",
        // The analysis deliberately unions all Data arguments of a call.
        "choose @4(left:V,right:V)->V = left\nbad @3(value:V)->V = choose(value,V())\n",
        // Reference fields are immutable; a temporary parent is still local.
        "struct Box @4:\n  item: V\nbad @3(value:V)->V = Box(value).item\n",
    ] {
        let source = format!("{prefix}{body}run @5()->i64 = 0\n");
        assert!(w.compile(&source, 5)?.is_empty(), "{body}");
        assert_eq!(w.field("error")?, 10, "{body}");
        assert_eq!(w.field("size")?, 0);
        assert_eq!(w.field("entry")?, -1);
    }
    // A private nested type cannot leak through an unrelated signature.
    let hidden = "owner @1()->i64 =\n  0\n  type Hidden @2 = [i64,1]\nbad @3()->Hidden = Hidden()\n";
    assert!(w.compile(hidden, 1)?.is_empty());
    assert_eq!(w.field("error")?, 2);
    // The same workspace must remain usable after failed lifetime checks.
    assert!(!w.compile(example, 186)?.is_empty());
    assert_eq!(w.field("error")?, 0);
    Ok(())
}
