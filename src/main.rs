use std::process::ExitCode;

const USAGE: &str = "\
tau (parser, type checker, borrow checker, interpreter and native compiler)

USAGE:
    tau new <name> [--lib]     create a package in a new folder (a program, or with --lib a library)
    tau add <name> --path DIR  add a dependency on a package in a folder to tau.toml
    tau add <name> --git URL [--tag T | --branch B | --rev R]
                               ... or on a git repository
    tau fetch [--update]       download the dependencies and pin git ones in tau.lock (--update: look up branches again)
    tau check [path]           type-check and borrow-check a program
    tau run [path]             check a program and run it (natively when possible)
    tau run [path] --native    ... natively only: fail if the program cannot be compiled to C
    tau run [path] --interp    ... with the interpreter only
    tau run [path] -v          ... and say why a program ran on the interpreter instead
    tau build [path]           compile a program to a native executable (cached) and print its path
    tau build [path] --emit-c  print the generated C instead
    tau test [path] [filter]   run the #[test] functions (those whose name contains `filter`)
    tau check [path] --types   ... and list the inferred type of every binding
    tau check <file> --json    ... and print diagnostics and hover types as JSON, for editors
                               (`-` as the file reads the program from stdin; `--file PATH` says which file
                               the text on stdin is, and checks it as part of its package)
    tau tokens <file>          print the token stream
    tau parse <file>           check that a file parses (--ast prints the tree, --sexp the function bodies)
    tau clean                  delete the cache of compiled programs
    tau update                 replace this tau with the newest release (needs curl; a private repository needs a GitHub token)
    tau update --check         only say whether a newer release exists
    tau --version              show the version and which C compiler the native backend would use

`path` is a .tau file, or a package folder (one with a tau.toml); without it the package in the
current folder (or above it) is used. A program may use other files and packages: see SPEC.md, section 9.

ENVIRONMENT:
    TAU_THREADS=n   threads for parallel code (default: all cores; results never depend on it)
    TAU_CC=cc       the C compiler to use (default: clang, gcc, cc)
    TAU_CACHE=dir   where compiled programs are cached (default: ~/.cache/tau)
    TAU_HOME=dir    where git dependencies are kept (default: ~/.tau)
    GITHUB_TOKEN    a token that can read the Tau repository, for `tau update` (or log in with `gh auth login`)
";

fn main() -> ExitCode {
    tau::update::cleanup();
    // the whole compiler runs on a thread with a big stack: parsing and checking deeply nested programs recurses
    tau::on_big_stack(real_main).unwrap_or(ExitCode::from(101))
}

fn real_main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("update") => {
            let flags: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
            if let Some(bad) = flags.iter().find(|f| !matches!(**f, "--check" | "--force")) {
                eprintln!("error: unknown option `{bad}` (use `tau update`, `tau update --check` or `tau update --force`)");
                return ExitCode::from(2);
            }
            return match tau::update::run(&tau::update::Options { check_only: flags.contains(&"--check"), force: flags.contains(&"--force") }) {
                Ok(msg) => {
                    println!("{msg}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            };
        }
        Some("clean") => {
            println!("removed {} cached program(s)", tau::native::clean_cache());
            return ExitCode::SUCCESS;
        }
        Some("--version") | Some("version") => {
            let cc = tau::native::find_cc();
            println!("tau {} (native backend: {})", env!("CARGO_PKG_VERSION"), cc.as_deref().unwrap_or("no C compiler found, interpreter only"));
            return ExitCode::SUCCESS;
        }
        Some("new") => return cmd_new(&args[1..]),
        Some("add") => return cmd_add(&args[1..]),
        Some("fetch") => return cmd_fetch(&args[1..]),
        _ => {}
    }
    let Some(cmd) = args.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match cmd {
        "tokens" | "parse" => single_file_tools(cmd, rest),
        "check" | "run" | "build" | "test" => program_commands(cmd, rest),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

// ------------------------------------------------------------------ tau new, tau add

fn cmd_new(args: &[String]) -> ExitCode {
    let library = args.iter().any(|a| a == "--lib");
    let names: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--lib") {
        eprintln!("error: unknown option `{bad}`");
        return ExitCode::from(2);
    }
    let [name] = names.as_slice() else {
        eprintln!("usage: tau new <name> [--lib]");
        return ExitCode::from(2);
    };
    match tau::project::new_package(std::path::Path::new("."), name, library) {
        Ok(dir) => {
            println!("created {} ({})", dir.display(), if library { "library" } else { "program" });
            println!("next: cd {name} && tau {}", if library { "test" } else { "run" });
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_add(args: &[String]) -> ExitCode {
    let mut name: Option<&str> = None;
    let (mut path, mut git, mut tag, mut branch, mut rev): (Option<&str>, Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| -> Option<&str> {
            let v = it.next().map(String::as_str);
            if v.is_none() {
                eprintln!("error: {what} needs a value");
            }
            v
        };
        match a.as_str() {
            "--path" => path = value("--path"),
            "--git" => git = value("--git"),
            "--tag" => tag = value("--tag"),
            "--branch" => branch = value("--branch"),
            "--rev" => rev = value("--rev"),
            s if s.starts_with('-') => {
                eprintln!("error: unknown option `{s}`");
                return ExitCode::from(2);
            }
            s if name.is_none() => name = Some(s),
            _ => {
                eprintln!("error: too many arguments");
                return ExitCode::from(2);
            }
        }
    }
    let Some(name) = name else {
        eprintln!("usage: tau add <name> --path DIR | --git URL [--tag T | --branch B | --rev R]");
        return ExitCode::from(2);
    };
    if !tau::project::is_identifier(name) {
        eprintln!("error: `{name}` cannot be used in `use` (letters, digits and `_` only)");
        return ExitCode::from(1);
    }
    use tau::toml::quote;
    let spec = match (path, git) {
        (Some(p), None) if tag.is_none() && branch.is_none() && rev.is_none() => format!("{{ path = {} }}", quote(p)),
        (None, Some(g)) => {
            let refs: Vec<(&str, &str)> = [("tag", tag), ("branch", branch), ("rev", rev)].iter().filter_map(|(k, v)| v.map(|v| (*k, v))).collect();
            if refs.len() > 1 {
                eprintln!("error: give only one of --tag, --branch and --rev");
                return ExitCode::from(2);
            }
            match refs.first() {
                Some((k, v)) => format!("{{ git = {}, {k} = {} }}", quote(g), quote(v)),
                None => format!("{{ git = {} }}", quote(g)),
            }
        }
        _ => {
            eprintln!("error: give either --path DIR, or --git URL (with at most one of --tag, --branch, --rev)");
            return ExitCode::from(2);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(dir) = tau::project::find_project_dir(&cwd) else {
        eprintln!("error: there is no tau.toml here or in a folder above (create a package with `tau new`)");
        return ExitCode::from(1);
    };
    let file = dir.join(tau::project::MANIFEST);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(1);
        }
    };
    match tau::project::add_dependency_text(&text, name, &spec).and_then(|t| std::fs::write(&file, t).map_err(|e| format!("cannot write {}: {e}", file.display()))) {
        Ok(()) => {
            println!("added {name} to {}", file.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// `tau fetch [--update]`: download the dependencies of the package here (all of them, and theirs) and pin
/// git dependencies in `tau.lock`; `--update` looks up branches again.
fn cmd_fetch(args: &[String]) -> ExitCode {
    if let Some(bad) = args.iter().find(|a| *a != "--update") {
        eprintln!("error: unknown option `{bad}` (use `tau fetch` or `tau fetch --update`)");
        return ExitCode::from(2);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(dir) = tau::project::find_project_dir(&cwd) else {
        eprintln!("error: there is no tau.toml here or in a folder above");
        return ExitCode::from(1);
    };
    let opts = tau::project::LoadOptions { fetch_all: true, update_locks: !args.is_empty(), ..Default::default() };
    match tau::project::load(&dir, opts) {
        Ok(_) => {
            println!("dependencies are ready");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprint!("{}", e.render());
            ExitCode::from(1)
        }
    }
}

// ------------------------------------------------------------------ tokens and parse (one file)

fn single_file_tools(cmd: &str, rest: &[String]) -> ExitCode {
    let Some(file) = rest.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let flags: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    let src = match read_source(file) {
        Ok(s) => s,
        Err(c) => return c,
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
        _ => match tau::parse_module(&src) {
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
    }
}

fn read_source(file: &str) -> Result<String, ExitCode> {
    let read = if file == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map(|_| buf)
    } else {
        std::fs::read_to_string(file)
    };
    read.map_err(|e| {
        eprintln!("error: cannot read {file}: {e}");
        ExitCode::from(1)
    })
}

// ------------------------------------------------------------------ check, run, build, test (whole programs)

/// What the command line names as the program: a path, `-` (standard input), or the package here.
enum Target {
    Stdin,
    Path(std::path::PathBuf),
}

/// Split `args` into the target and the rest. The first argument is the target when it is `-` or an
/// existing file or folder; otherwise (a flag, a test filter, nothing) the package in the current folder.
fn split_target(cmd: &str, args: &[String]) -> Result<(Target, String, Vec<String>), ExitCode> {
    let first = args.first().map(String::as_str);
    match first {
        Some("-") => Ok((Target::Stdin, "<stdin>".to_string(), args[1..].to_vec())),
        Some(f) if !f.starts_with('-') && std::path::Path::new(f).exists() => Ok((Target::Path(f.into()), f.to_string(), args[1..].to_vec())),
        Some(f) if !f.starts_with('-') && cmd != "test" => {
            eprintln!("error: cannot read {f}: no such file or folder");
            Err(ExitCode::from(1))
        }
        _ => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match tau::project::find_project_dir(&cwd) {
                Some(dir) => {
                    let label = tau::project::Manifest::read(&dir).map(|m| m.name).unwrap_or_else(|_| dir.display().to_string());
                    Ok((Target::Path(dir), label, args.to_vec()))
                }
                None => {
                    eprintln!("error: no program given and no tau.toml in this folder or above it\nhint: `tau run file.tau`, or create a package with `tau new <name>`");
                    Err(ExitCode::from(1))
                }
            }
        }
    }
}

fn load_program(target: &Target, all_modules: bool) -> Result<tau::Workspace, ExitCode> {
    match target {
        Target::Stdin => {
            let src = read_source("-")?;
            tau::Workspace::single("<stdin>", &src).map_err(|d| {
                eprint!("{}", d.render(&src, "<stdin>"));
                ExitCode::from(1)
            })
        }
        Target::Path(p) => tau::project::load(p, tau::project::LoadOptions { all_modules, ..Default::default() }).map_err(|e| {
            eprint!("{}", e.render());
            ExitCode::from(1)
        }),
    }
}

fn program_commands(cmd: &str, args: &[String]) -> ExitCode {
    // editors: the text of one file on stdin
    if cmd == "check" && args.iter().any(|a| a == "--json") {
        return check_json_command(args);
    }
    let (target, label, flags) = match split_target(cmd, args) {
        Ok(t) => t,
        Err(c) => return c,
    };
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let ws = match load_program(&target, cmd == "test") {
        Ok(w) => w,
        Err(c) => return c,
    };
    if matches!(cmd, "run" | "build") && !has_main(&ws) {
        eprintln!("error: the program has no `fn main()`{}", if is_library(&target) { "\nhint: this package is a library (it has src/lib.tau, not src/main.tau); use `tau test` or `tau check`" } else { "" });
        return ExitCode::from(1);
    }
    match cmd {
        "check" => {
            let result = ws.analyze();
            if flags.contains(&"--types") {
                for b in &result.results.bindings {
                    println!("{}{}: {}", if b.mutable { "mut " } else { "" }, b.name, result.show(&b.ty));
                }
            }
            for d in &result.diagnostics {
                eprint!("{}", ws.render(d));
                eprintln!();
            }
            if result.diagnostics.is_empty() {
                println!("{label}: ok");
                ExitCode::SUCCESS
            } else {
                eprintln!("{} error(s)", result.diagnostics.len());
                ExitCode::from(1)
            }
        }
        "build" => match tau::native::emit_c_workspace(&ws) {
            Err(e) => report_native_error(&e, &ws),
            Ok(c) if flags.contains(&"--emit-c") => {
                print!("{c}");
                ExitCode::SUCCESS
            }
            Ok(c) => match tau::native::compile_c(&c) {
                Ok(exe) => {
                    println!("{}", exe.display());
                    ExitCode::SUCCESS
                }
                Err(e) => report_native_error(&e, &ws),
            },
        },
        "run" if !flags.contains(&"--interp") => {
            let verbose = flags.contains(&"-v");
            let forced = flags.contains(&"--native");
            match tau::native::build_workspace(&ws) {
                Ok(exe) => match tau::native::exec(&exe) {
                    Ok(code) => ExitCode::from(code as u8),
                    Err(e) => report_native_error(&e, &ws),
                },
                Err(tau::native::NativeError::Unsupported(why)) if !forced => {
                    if verbose {
                        eprintln!("note: running with the interpreter: {why}");
                    }
                    run_interpreted(&ws)
                }
                Err(tau::native::NativeError::NoCompiler) if !forced => {
                    eprintln!("note: no C compiler found (install clang or gcc for native speed); running with the interpreter");
                    run_interpreted(&ws)
                }
                Err(e) => report_native_error(&e, &ws),
            }
        }
        "run" => run_interpreted(&ws),
        _ => {
            // test: the rest of the command line is the filter
            let filter = flags.iter().find(|f| !f.starts_with('-')).copied();
            let r = tau::test_workspace(&ws, filter, u64::MAX);
            if !r.diagnostics.is_empty() {
                for d in &r.diagnostics {
                    eprint!("{}", ws.render(d));
                    eprintln!();
                }
                eprintln!("{} error(s); no tests were run", r.diagnostics.len());
                return ExitCode::from(1);
            }
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

/// Does the entry module define `main`?
fn has_main(ws: &tau::Workspace) -> bool {
    ws.mods[ws.root].module.items.iter().any(|i| matches!(&i.kind, tau::ast::ItemKind::Fn(f) if f.name.name == "main"))
}

fn is_library(target: &Target) -> bool {
    match target {
        Target::Path(p) if p.is_dir() => tau::project::project_entry(p).map(|(_, lib)| lib).unwrap_or(false),
        _ => false,
    }
}

/// `tau check <file|-> --json [--file PATH]`: for editors. Always exits 0 once the output was produced;
/// errors are in the JSON. With `--file`, the text on standard input is the current text of that file,
/// and it is checked as part of its package.
fn check_json_command(args: &[String]) -> ExitCode {
    let mut file_arg: Option<&str> = None;
    let mut real_path: Option<&str> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => {}
            "--file" => real_path = it.next().map(String::as_str),
            s if file_arg.is_none() && (!s.starts_with('-') || s == "-") => file_arg = Some(s),
            _ => {}
        }
    }
    let Some(file) = file_arg else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let src = match read_source(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match real_path {
        Some(p) => println!("{}", tau::editor::check_json_file(std::path::Path::new(p), &src)),
        None => println!("{}", tau::editor::check_json(&src)),
    }
    ExitCode::SUCCESS
}

fn run_interpreted(ws: &tau::Workspace) -> ExitCode {
    let out = Box::new(std::io::BufWriter::new(std::io::stdout()));
    let r = tau::run_workspace(ws, out, u64::MAX);
    if !r.diagnostics.is_empty() {
        for d in &r.diagnostics {
            eprint!("{}", ws.render(d));
            eprintln!();
        }
        eprintln!("{} error(s); the program was not run", r.diagnostics.len());
        return ExitCode::from(1);
    }
    match r.outcome {
        Some(tau::interp::Outcome::Panic(m)) => {
            eprintln!("panic: {m}");
            ExitCode::from(101)
        }
        Some(tau::interp::Outcome::Error(m)) => {
            eprintln!("error: {m}");
            ExitCode::from(1)
        }
        _ => ExitCode::SUCCESS,
    }
}

fn report_native_error(e: &tau::native::NativeError, ws: &tau::Workspace) -> ExitCode {
    use tau::native::NativeError::*;
    match e {
        Parse(d) => eprint!("{}", ws.render(d)),
        Rejected(ds) => {
            for d in ds {
                eprint!("{}", ws.render(d));
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
