use std::process::ExitCode;

const USAGE: &str = "\
tau (parser, type checker, borrow checker and interpreter)

USAGE:
    tau tokens <file>          print the token stream
    tau parse <file>           check that a file parses
    tau parse <file> --ast     ... and print the full AST
    tau parse <file> --sexp    ... and print each top-level function body as an S-expression
    tau check <file>           type-check and borrow-check a file
    tau run <file>             check a program and run it (natively when possible)
    tau run <file> --native    ... natively only: fail if the program cannot be compiled to C
    tau run <file> --interp    ... with the interpreter only
    tau run <file> -v          ... and say why a program ran on the interpreter instead
    tau build <file>           compile a program to a native executable (cached) and print its path
    tau build <file> --emit-c  print the generated C instead
    tau test <file> [filter]   run the #[test] functions (those whose name contains `filter`)
    tau check <file> --types   ... and list the inferred type of every binding
    tau check <file> --json    ... and print diagnostics and hover types as JSON, for editors
                               (`-` as the file reads the program from stdin)
    tau clean                  delete the cache of compiled programs
    tau --version              show the version and which C compiler the native backend would use

ENVIRONMENT:
    TAU_THREADS=n   threads for parallel code (default: all cores; results never depend on it)
    TAU_CC=cc       the C compiler to use (default: clang, gcc, cc)
    TAU_CACHE=dir   where compiled programs are cached (default: ~/.cache/tau)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("clean") => {
            println!("removed {} cached program(s)", tau::native::clean_cache());
            return ExitCode::SUCCESS;
        }
        Some("--version") | Some("version") => {
            let cc = tau::native::find_cc();
            println!("tau {} (native backend: {})", env!("CARGO_PKG_VERSION"), cc.as_deref().unwrap_or("no C compiler found, interpreter only"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let (cmd, file) = match (args.get(0), args.get(1)) {
        (Some(c), Some(f)) => (c.as_str(), f.as_str()),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let flags: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();

    let read = if file == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map(|_| buf)
    } else {
        std::fs::read_to_string(file)
    };
    let src = match read {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    match cmd {
        "tokens" => match tau::lexer::lex(&src) {
            Ok(toks) => {
                for t in toks {
                    println!("{:>5}..{:<5} {:?}", t.span.start, t.span.end, t.kind);
                }
                ExitCode::SUCCESS
            }
            Err(d) => {
                eprint!("{}", d.render(&src, file));
                ExitCode::from(1)
            }
        },
        "parse" => match tau::parse_module(&src) {
            Ok(module) => {
                if flags.contains(&"--ast") {
                    println!("{module:#?}");
                } else if flags.contains(&"--sexp") {
                    for item in &module.items {
                        if let tau::ast::ItemKind::Fn(f) = &item.kind {
                            if let Some(body) = &f.body {
                                println!("{} {}", f.name.name, tau::sexp::block(body));
                            }
                        }
                    }
                } else {
                    println!("{file}: ok ({} items)", module.items.len());
                }
                ExitCode::SUCCESS
            }
            Err(d) => {
                eprint!("{}", d.render(&src, file));
                ExitCode::from(1)
            }
        },
        "check" if flags.contains(&"--json") => {
            // for editors: always exits 0 when the output was produced; errors are in the JSON
            println!("{}", tau::editor::check_json(&src));
            ExitCode::SUCCESS
        }
        "check" => match tau::analyze_source(&src) {
            Err(d) => {
                eprint!("{}", d.render(&src, file));
                ExitCode::from(1)
            }
            Ok(result) => {
                if flags.contains(&"--types") {
                    for b in &result.results.bindings {
                        println!(
                            "{}{}: {}",
                            if b.mutable { "mut " } else { "" },
                            b.name,
                            result.show(&b.ty)
                        );
                    }
                }
                for d in &result.diagnostics {
                    eprint!("{}", d.render(&src, file));
                    eprintln!();
                }
                if result.diagnostics.is_empty() {
                    println!("{file}: ok");
                    ExitCode::SUCCESS
                } else {
                    eprintln!("{} error(s)", result.diagnostics.len());
                    ExitCode::from(1)
                }
            }
        },
        "build" => match tau::native::emit_c(&src) {
            Err(e) => report_native_error(&e, &src, file),
            Ok(c) if flags.contains(&"--emit-c") => {
                print!("{c}");
                ExitCode::SUCCESS
            }
            Ok(c) => match tau::native::compile_c(&c) {
                Ok(exe) => {
                    println!("{}", exe.display());
                    ExitCode::SUCCESS
                }
                Err(e) => report_native_error(&e, &src, file),
            },
        },
        "run" if !flags.contains(&"--interp") => {
            let verbose = flags.contains(&"-v");
            let forced = flags.contains(&"--native");
            match tau::native::build_source(&src) {
                Ok(exe) => match tau::native::exec(&exe) {
                    Ok(code) => ExitCode::from(code as u8),
                    Err(e) => report_native_error(&e, &src, file),
                },
                Err(tau::native::NativeError::Unsupported(why)) if !forced => {
                    if verbose {
                        eprintln!("note: running with the interpreter: {why}");
                    }
                    run_interpreted(&src, file)
                }
                Err(tau::native::NativeError::NoCompiler) if !forced => {
                    eprintln!("note: no C compiler found (install clang or gcc for native speed); running with the interpreter");
                    run_interpreted(&src, file)
                }
                Err(e) => report_native_error(&e, &src, file),
            }
        }
        "run" => run_interpreted(&src, file),
        "test" => {
            let filter = args.get(2).map(String::as_str);
            match tau::test_source(&src, filter, u64::MAX) {
                Err(d) => {
                    eprint!("{}", d.render(&src, file));
                    ExitCode::from(1)
                }
                Ok(r) if !r.diagnostics.is_empty() => {
                    for d in &r.diagnostics {
                        eprint!("{}", d.render(&src, file));
                        eprintln!();
                    }
                    eprintln!("{} error(s); no tests were run", r.diagnostics.len());
                    ExitCode::from(1)
                }
                Ok(r) => {
                    println!("running {} test{}", r.results.len(), if r.results.len() == 1 { "" } else { "s" });
                    for t in &r.results {
                        println!("test {} ... {}", t.name, if t.passed { "ok" } else { "FAILED" });
                    }
                    let failed: Vec<_> = r.results.iter().filter(|t| !t.passed).collect();
                    if !failed.is_empty() {
                        println!("\nfailures:");
                        for t in &failed {
                            println!("\n---- {} ----", t.name);
                            if !t.output.is_empty() {
                                print!("{}", t.output);
                            }
                            println!("{}", t.message);
                        }
                    }
                    let passed = r.results.len() - failed.len();
                    println!("\ntest result: {}. {} passed; {} failed", if failed.is_empty() { "ok" } else { "FAILED" }, passed, failed.len());
                    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
                }
            }
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_interpreted(src: &str, file: &str) -> ExitCode {
    let out = Box::new(std::io::BufWriter::new(std::io::stdout()));
    match tau::run_source(&src, out) {
        Err(d) => {
            eprint!("{}", d.render(&src, file));
            ExitCode::from(1)
        }
        Ok(r) if !r.diagnostics.is_empty() => {
            for d in &r.diagnostics {
                eprint!("{}", d.render(&src, file));
                eprintln!();
            }
            eprintln!("{} error(s); the program was not run", r.diagnostics.len());
            ExitCode::from(1)
        }
        Ok(r) => match r.outcome {
            Some(tau::interp::Outcome::Panic(m)) => {
                eprintln!("panic: {m}");
                ExitCode::from(101)
            }
            Some(tau::interp::Outcome::Error(m)) => {
                eprintln!("error: {m}");
                ExitCode::from(1)
            }
            _ => ExitCode::SUCCESS,
        },
    }
}

fn report_native_error(e: &tau::native::NativeError, src: &str, file: &str) -> ExitCode {
    use tau::native::NativeError::*;
    match e {
        Parse(d) => eprint!("{}", d.render(src, file)),
        Rejected(ds) => {
            for d in ds {
                eprint!("{}", d.render(src, file));
                eprintln!();
            }
            eprintln!("{} error(s); the program was not built", ds.len());
        }
        Unsupported(why) => eprintln!("error: {why}\nhint: `tau run --interp` runs it with the interpreter"),
        NoCompiler => eprintln!("error: no C compiler found: install clang or gcc (or set TAU_CC) to build native programs"),
        Other(m) => eprintln!("error: {m}"),
    }
    ExitCode::from(1)
}
