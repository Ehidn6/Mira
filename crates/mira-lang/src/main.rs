use std::{env, path::Path, process::ExitCode};
fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    if args.get(1).is_some_and(|s| s == "system") {
        return mira_lang::system::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "source") {
        return mira_lang::source_cli::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s|s == "build-compiler") {
        let directory = Path::new(args.get(2).ok_or("build-compiler needs OUTPUT_DIRECTORY")?);
        let (procedures,bytes) = mira_lang::build_compiler(directory)?;
        println!("Built Mira compiler: {procedures} procedures, {bytes} bytes; {}",directory.display());
        return Ok(());
    }
    if args.len() < 3 {
        return Err("usage: mira <inspect|check|run|fmt|build|build-compiler|build-kernel|build-uefi|run-uefi> INPUT [OUTPUT]\nbuild emits Linux x86-64 ELF; build-kernel emits a freestanding Multiboot image; run uses the sequential interpreter".into());
    }
    let (program, source) = mira_lang::open_source(Path::new(&args[2]))?;
    match args[1].as_str() {
        "inspect" => println!("{}", mira_lang::inspect::command(&program, &args[3..])?),
        "check" => {
            mira_lang::runtime::lower(&program)?;
            println!("OK: typed finite graph");
        }
        "run" => {
            let activation = mira_lang::runtime::execute(&program, 1)?;
            for (name, value) in &activation.terminals {
                let ty = activation
                    .terminal_types
                    .get(name)
                    .copied()
                    .unwrap_or(mira_core::Type::I64);
                let value = if matches!(ty, mira_core::Type::Data(_) | mira_core::Type::F32) {
                    activation.data.format(&program.data_types(), ty, *value)
                } else {
                    value.to_string()
                };
                println!("{name} = {value}");
            }
        }
        "fmt" => {
            if let Some(output) = args.get(3) {
                mira_lang::save(Path::new(output), &program)?;
            } else {
                print!("{}", mira_lang::syntax::format(&program));
            }
        }
        "build-uefi" | "run-uefi" => {
            let directory = Path::new(args.get(3).ok_or("UEFI commands need OUTPUT_DIRECTORY")?);
            mira_lang::uefi::prepare(&program, directory)?;
            // The editor owns the user's text, including comments and contained source parts.
            // Expanded compiler definitions belong to the compiled image.
            std::fs::write(directory.join("source.mira"), source).map_err(|e| e.to_string())?;
            println!("built UEFI/GOP project: {}", directory.display());
            if args[1] == "run-uefi" {
                let runtime = directory.with_extension("runtime");
                let mut command = mira_lang::uefi::qemu_command(directory, &runtime)?;
                command
                    .args([
                        "-serial",
                        "mon:stdio",
                        "-monitor",
                        "none",
                        "-nic",
                        "none",
                        "-debugcon",
                    ])
                    .arg(format!("file:{}", runtime.join("debug.log").display()));
                if args.get(4).is_some_and(|s| s == "--headless") {
                    command.args(["-display", "none"]);
                } else {
                    command.args([
                        "-device",
                        "qemu-xhci,id=mira-usb,addr=04.0",
                        "-device",
                        "usb-kbd,bus=mira-usb.0",
                    ]);
                }
                let status = command.status().map_err(|e| format!("QEMU: {e}"))?;
                if !status.success() {
                    return Err(format!("QEMU exited: {status}"));
                }
            }
        }
        "build" | "build-kernel" => {
            let out = args.get(3).ok_or("build needs OUTPUT")?;
            if Path::new(out) == Path::new(&args[2]) {
                return Err("output must differ from source".into());
            }
            let kernel = args[1] == "build-kernel";
            let bytes = if kernel {
                mira_lang::kernel::compile(&program)?
            } else {
                mira_lang::native::compile(&program)?
            };
            std::fs::write(out, bytes).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    out,
                    std::fs::Permissions::from_mode(if kernel { 0o644 } else { 0o755 }),
                )
                .map_err(|e| e.to_string())?;
            }
            println!(
                "built {out} ({})",
                if kernel {
                    "freestanding x86-64, Multiboot/QEMU"
                } else {
                    "Linux x86-64"
                }
            );
        }
        _ => return Err("unknown command".into()),
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mira: {e}");
            ExitCode::FAILURE
        }
    }
}
