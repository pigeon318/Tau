use std::process::ExitCode;

const USAGE: &str = "\
tau (parser, type checker, borrow checker and interpreter)

USAGE:
    tau tokens <file>          print the token stream
    tau parse <file>           check that a file parses
    tau parse <file> --ast     ... and print the full AST
    tau parse <file> --sexp    ... and print each top-level function body as an S-expression
    tau check <file>           type-check and borrow-check a file
    tau run <file>             check a program and run it
    tau test <file> [filter]   run the #[test] functions (those whose name contains `filter`)
    tau check <file> --types   ... and list the inferred type of every binding
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, file) = match (args.get(0), args.get(1)) {
        (Some(c), Some(f)) => (c.as_str(), f.as_str()),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let flags: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();

    let src = match std::fs::read_to_string(file) {
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
        "run" => {
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
