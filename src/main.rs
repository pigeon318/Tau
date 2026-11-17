use std::process::ExitCode;

const USAGE: &str = "\
tau (stage 1: lexer and parser)

USAGE:
    tau tokens <file>          print the token stream
    tau parse <file>           check that a file parses
    tau parse <file> --ast     ... and print the full AST
    tau parse <file> --sexp    ... and print each top-level function body as an S-expression
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
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
