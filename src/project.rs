//! Packages and multi-file programs: reading `tau.toml`, finding the files a program uses, and
//! loading them into a `Workspace`.
//!
//! Layout of a package:
//!
//! ```text
//! orbits/
//!   tau.toml          [package] name, [dependencies], [native]
//!   src/main.tau      the entry module of a program (`tau run`)
//!   src/lib.tau       the root module when other packages use this one
//!   src/util.tau      `use util`        (or src/util/mod.tau)
//!   src/geo/shapes.tau  `use geo.shapes`
//! ```
//!
//! `use a.b.c` is looked up from the root of the package (`src/`), never relative to the file it is
//! in. The first name decides where to look: a module of this package, a dependency named in
//! `tau.toml` (its root module is the dependency's `src/lib.tau`, so `use fastmath` and
//! `use fastmath.vec` are `src/lib.tau` and `src/vec.tau` there), or the compiler's library
//! (`std`, `math`). A program that is a single file with no `tau.toml` can still use its siblings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::prelude::{self, ModuleSrc};
use crate::source::SourceMap;
use crate::toml::{self, Value};
use crate::workspace::Workspace;

// ------------------------------------------------------------------ the manifest

#[derive(Debug, Clone, PartialEq)]
pub enum GitRef {
    Default,
    Tag(String),
    Branch(String),
    Rev(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DepSpec {
    Path(String),
    Git { url: String, reference: GitRef },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub spec: DepSpec,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: Option<String>,
    pub deps: Vec<Dependency>,
    /// `[native] link`: C libraries to link (recorded; the C interface comes later).
    pub native_link: Vec<String>,
    /// The folder that holds `tau.toml`.
    pub dir: PathBuf,
}

pub const MANIFEST: &str = "tau.toml";

/// A path as people write it: on Windows, `canonicalize` gives `\\?\C:\dir`, and the `\\?\` is dropped.
pub fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => s,
    }
}

/// A name that can be used in `use`: letters, digits and `_`, not starting with a digit.
pub fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_alphabetic()) && cs.all(|c| c == '_' || c.is_alphanumeric())
}

const RESERVED: &[&str] = &["std", "math", "core", "prelude", "main", "self", "super", "crate"];

impl Manifest {
    pub fn parse(text: &str, dir: &Path) -> Result<Manifest, String> {
        let t = toml::parse(text).map_err(|e| format!("tau.toml, {e}"))?;
        let Some(pkg) = t.table("package") else {
            return Err("tau.toml has no [package] section (it needs `name = \"...\"`)".into());
        };
        let name = pkg.str("name").ok_or("tau.toml: [package] needs `name = \"...\"`")?.to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("tau.toml: the package name `{name}` may only contain letters, digits, `_` and `-`"));
        }
        let version = pkg.str("version").map(str::to_string);
        let mut deps = Vec::new();
        if let Some(d) = t.get("dependencies") {
            let Value::Table(d) = d else { return Err("tau.toml: [dependencies] must be a table".into()) };
            for (dname, v) in &d.0 {
                if !is_identifier(dname) {
                    return Err(format!("tau.toml: the dependency name `{dname}` cannot be used in `use` (letters, digits and `_` only)"));
                }
                if RESERVED.contains(&dname.as_str()) {
                    return Err(format!("tau.toml: `{dname}` is a reserved name and cannot be a dependency"));
                }
                deps.push(Dependency { name: dname.clone(), spec: parse_dep(dname, v)? });
            }
        }
        let mut native_link = Vec::new();
        if let Some(n) = t.table("native") {
            if let Some(Value::Array(items)) = n.get("link") {
                for i in items {
                    match i {
                        Value::Str(s) => native_link.push(s.clone()),
                        _ => return Err("tau.toml: [native] link must be a list of strings".into()),
                    }
                }
            }
        }
        Ok(Manifest { name, version, deps, native_link, dir: dir.to_path_buf() })
    }

    pub fn read(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", display_path(&path)))?;
        Manifest::parse(&text, dir).map_err(|e| format!("{}: {e}", display_path(&path)))
    }
}

fn parse_dep(name: &str, v: &Value) -> Result<DepSpec, String> {
    match v {
        Value::Str(_) => Err(format!("tau.toml: `{name}`: dependencies by version number need a package registry, which does not exist yet; use `{{ path = \"...\" }}` or `{{ git = \"...\" }}`")),
        Value::Table(t) => {
            let known = ["path", "git", "tag", "branch", "rev"];
            if let Some((k, _)) = t.0.iter().find(|(k, _)| !known.contains(&k.as_str())) {
                return Err(format!("tau.toml: `{name}`: unknown key `{k}` (use path, git, tag, branch or rev)"));
            }
            let get = |k: &str| -> Result<Option<String>, String> {
                match t.get(k) {
                    None => Ok(None),
                    Some(Value::Str(s)) => Ok(Some(s.clone())),
                    Some(_) => Err(format!("tau.toml: `{name}`: `{k}` must be a string")),
                }
            };
            match (get("path")?, get("git")?) {
                (Some(p), None) => {
                    if t.0.len() > 1 {
                        return Err(format!("tau.toml: `{name}`: a path dependency takes only `path`"));
                    }
                    Ok(DepSpec::Path(p))
                }
                (None, Some(url)) => {
                    let refs: Vec<GitRef> = [("tag", GitRef::Tag as fn(String) -> GitRef), ("branch", GitRef::Branch), ("rev", GitRef::Rev)]
                        .iter()
                        .filter_map(|(k, f)| get(k).ok().flatten().map(f))
                        .collect();
                    if refs.len() > 1 {
                        return Err(format!("tau.toml: `{name}`: give only one of tag, branch and rev"));
                    }
                    for k in ["tag", "branch", "rev"] {
                        get(k)?;
                    }
                    Ok(DepSpec::Git { url, reference: refs.into_iter().next().unwrap_or(GitRef::Default) })
                }
                (Some(_), Some(_)) => Err(format!("tau.toml: `{name}`: give either `path` or `git`, not both")),
                (None, None) => Err(format!("tau.toml: `{name}`: needs `path = \"...\"` or `git = \"...\"`")),
            }
        }
        _ => Err(format!("tau.toml: `{name}`: a dependency is `{{ path = \"...\" }}` or `{{ git = \"...\" }}`")),
    }
}

// ------------------------------------------------------------------ finding the project

/// The folder with a `tau.toml` that contains `start` (or is it), searching upwards.
pub fn find_project_dir(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    let mut cur: Option<&Path> = Some(if start.is_dir() { &start } else { start.parent()? });
    while let Some(d) = cur {
        if d.join(MANIFEST).is_file() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// The package folder that `file` is in the `src` of, if any (the file is then a module of that package).
pub fn project_of_file(file: &Path) -> Option<PathBuf> {
    let mut cur = file.parent();
    while let Some(d) = cur {
        if d.join(MANIFEST).is_file() && file.starts_with(d.join("src")) {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// The entry file of a package: `src/main.tau`, or for a library `src/lib.tau`. The flag says
/// whether it is a library (nothing to run, but it can be checked and tested).
pub fn project_entry(dir: &Path) -> Result<(PathBuf, bool), String> {
    let main = dir.join("src").join("main.tau");
    if main.is_file() {
        return Ok((main, false));
    }
    let lib = dir.join("src").join("lib.tau");
    if lib.is_file() {
        return Ok((lib, true));
    }
    Err(format!("{} has neither src/main.tau (a program) nor src/lib.tau (a library)", display_path(dir)))
}

// ------------------------------------------------------------------ loading

/// Why a program could not be loaded. If a file had a syntax error, `diags` and `map` show it.
#[derive(Debug)]
pub struct LoadError {
    pub message: String,
    pub diags: Vec<Diagnostic>,
    pub map: SourceMap,
}

impl LoadError {
    fn plain(message: impl Into<String>) -> LoadError {
        LoadError { message: message.into(), diags: Vec::new(), map: SourceMap::new() }
    }

    /// The text to show the user.
    pub fn render(&self) -> String {
        if self.diags.is_empty() {
            return format!("error: {}\n", self.message);
        }
        self.diags.iter().map(|d| self.map.render(d)).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Default, Clone)]
pub struct LoadOptions {
    /// Also load every `.tau` file under the package's `src/` (so `tau test` finds all tests,
    /// even in modules that nothing imports).
    pub all_modules: bool,
    /// Where git dependencies are kept (default: `crate::gitdeps::home()`).
    pub home: Option<PathBuf>,
    /// Forget the pinned commits of branch dependencies and look them up again.
    pub update_locks: bool,
    /// Fetch every dependency, also those no module uses (`tau fetch`).
    pub fetch_all: bool,
    /// Files whose text is given instead of read from disk (an editor's unsaved buffers); the paths
    /// must be absolute and canonical.
    pub overrides: Vec<(PathBuf, String)>,
}

struct Pkg {
    /// `""` for the program's own package, `"name."` for a dependency.
    prefix: String,
    /// The package's source folder.
    src: PathBuf,
    /// Its dependencies: name and the folder of the dependency.
    deps: Vec<(String, PathBuf)>,
}

struct Loader {
    map: SourceMap,
    mods: Vec<ModuleSrc>,
    by_path: HashMap<String, usize>,
    pkgs: Vec<Pkg>,
    /// Dependency name → package index (a name stands for one package in a whole program).
    pkg_by_name: HashMap<String, usize>,
    pkg_dirs: HashMap<usize, PathBuf>,
    queue: Vec<(Vec<String>, usize)>,
    root_file: PathBuf,
    cwd: Option<PathBuf>,
    lock: crate::gitdeps::Lock,
    home: PathBuf,
    overrides: Vec<(PathBuf, String)>,
}

/// Load the program whose entry is `entry`: a `.tau` file, a project folder, or a `tau.toml`.
pub fn load(entry: &Path, opts: LoadOptions) -> Result<Workspace, LoadError> {
    let (root_file, project) = find_entry(entry).map_err(LoadError::plain)?;
    let mut l = Loader {
        map: SourceMap::new(),
        mods: Vec::new(),
        by_path: HashMap::new(),
        pkgs: Vec::new(),
        pkg_by_name: HashMap::new(),
        pkg_dirs: HashMap::new(),
        queue: Vec::new(),
        root_file: root_file.clone(),
        cwd: std::env::current_dir().ok().and_then(|p| p.canonicalize().ok()),
        lock: crate::gitdeps::Lock::default(),
        home: opts.home.clone().unwrap_or_else(crate::gitdeps::home),
        overrides: opts.overrides.clone(),
    };
    let (src_dir, deps) = match &project {
        Some(dir) => {
            let m = Manifest::read(dir).map_err(LoadError::plain)?;
            l.lock = crate::gitdeps::Lock::read(dir).map_err(LoadError::plain)?;
            if opts.update_locks {
                l.lock.forget_moving();
            }
            let deps = l.resolve_deps(&m).map_err(LoadError::plain)?;
            (dir.join("src"), deps)
        }
        None => (root_file.parent().unwrap_or(Path::new(".")).to_path_buf(), Vec::new()),
    };
    l.pkgs.push(Pkg { prefix: String::new(), src: src_dir.clone(), deps });
    l.add_module(&root_file, "main".to_string(), 0)?;
    l.drain()?;
    if opts.fetch_all {
        l.ensure_all(0)?;
    }
    if opts.all_modules {
        let mut files = Vec::new();
        collect_tau_files(&src_dir, &mut files);
        files.sort();
        for f in files {
            let Some(rel) = rel_segments(&src_dir, &f) else { continue };
            if rel.is_empty() || f == root_file || (rel.len() == 1 && (rel[0] == "main" || rel[0] == "lib")) {
                continue;
            }
            l.add_module(&f, rel.join("."), 0)?;
        }
        l.drain()?;
    }
    if let Some(dir) = &project {
        l.lock.write(dir).map_err(LoadError::plain)?;
    }
    let mut mods = prelude::load_into(&mut l.map);
    let n_lib = mods.len();
    mods.extend(l.mods);
    Ok(Workspace { map: l.map, mods, root: n_lib })
}

/// The entry file, and the project folder when it is part of a package.
fn find_entry(entry: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let p = entry.canonicalize().map_err(|e| format!("cannot read {}: {e}", entry.display()))?;
    if p.is_dir() {
        if p.join(MANIFEST).is_file() {
            return Ok((project_entry(&p)?.0, Some(p)));
        }
        return Err(format!("{} has no {MANIFEST}", entry.display()));
    }
    if p.file_name().map(|n| n == MANIFEST).unwrap_or(false) {
        let dir = p.parent().unwrap().to_path_buf();
        return Ok((project_entry(&dir)?.0, Some(dir)));
    }
    // a file inside a package's `src` belongs to that package
    let mut cur = p.parent();
    while let Some(d) = cur {
        if d.join(MANIFEST).is_file() && p.starts_with(d.join("src")) {
            return Ok((p.clone(), Some(d.to_path_buf())));
        }
        cur = d.parent();
    }
    Ok((p, None))
}

fn collect_tau_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_tau_files(&p, out);
        } else if p.extension().map(|x| x == "tau").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// `src/a/b.tau` is `["a", "b"]`; `src/a/mod.tau` is `["a"]`; `src/mod.tau` is `[]`.
fn rel_segments(src: &Path, file: &Path) -> Option<Vec<String>> {
    let rel = file.strip_prefix(src).ok()?;
    let mut segs: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    let last = segs.pop()?;
    let stem = last.strip_suffix(".tau")?.to_string();
    if stem != "mod" {
        segs.push(stem);
    }
    Some(segs)
}

/// Does the directory `dir` have an entry called exactly `name`? (A case-insensitive file system
/// would otherwise find `Util.tau` for `use util`, and the program would work on one system only.)
fn has_exact(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy() == name)).unwrap_or(false)
}

/// The file of the module `segs` (relative to a package's source folder): `a/b.tau` or `a/b/mod.tau`.
fn module_file(src: &Path, segs: &[String]) -> Option<PathBuf> {
    let (last, parents) = segs.split_last()?;
    let mut dir = src.to_path_buf();
    for s in parents {
        if !has_exact(&dir, s) {
            return None;
        }
        dir.push(s);
    }
    let file = format!("{last}.tau");
    if has_exact(&dir, &file) && dir.join(&file).is_file() {
        return Some(dir.join(file));
    }
    if has_exact(&dir, last) && dir.join(last).join("mod.tau").is_file() {
        return Some(dir.join(last).join("mod.tau"));
    }
    None
}

/// All the `use` paths in a module (a group `use a.{b, c}` gives `a.b` and `a.c`).
fn use_paths(m: &Module) -> Vec<Vec<String>> {
    fn walk(t: &UseTree, prefix: &[String], out: &mut Vec<Vec<String>>) {
        let mut full = prefix.to_vec();
        full.extend(t.path.iter().map(|i| i.name.clone()));
        match &t.kind {
            UseKind::Group(ts) => ts.iter().for_each(|c| walk(c, &full, out)),
            _ => out.push(full),
        }
    }
    let mut out = Vec::new();
    for item in &m.items {
        if let ItemKind::Use(t) = &item.kind {
            walk(t, &[], &mut out);
        }
    }
    out
}

impl Loader {
    fn display(&self, p: &Path) -> String {
        match &self.cwd {
            Some(c) => match p.strip_prefix(c) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => display_path(p),
            },
            None => display_path(p),
        }
    }

    fn fail(&self, message: String) -> LoadError {
        LoadError::plain(message)
    }

    /// Read and parse a module file, and queue the modules it uses.
    fn add_module(&mut self, file: &Path, canon: String, pkg: usize) -> Result<(), LoadError> {
        if self.by_path.contains_key(&canon) {
            return Ok(());
        }
        if canon != "main" && file == self.root_file {
            return Err(self.fail(format!(
                "`use {canon}` names {}, which is the entry file of this run, and no other module can use that. Run the package's src/main.tau instead.",
                self.display(file)
            )));
        }
        let src = match self.overrides.iter().find(|(p, _)| p == file) {
            Some((_, text)) => text.clone(),
            None => std::fs::read_to_string(file).map_err(|e| self.fail(format!("cannot read {}: {e}", self.display(file))))?,
        };
        let name = self.display(file);
        let base = self.map.add(name, src.clone());
        let file_idx = self.map.files.len() - 1;
        self.map.files[file_idx].path = Some(file.to_path_buf());
        let module = match crate::parser::parse_module_at(&src, base) {
            Ok(m) => m,
            Err(d) => {
                // keep the map so that the error can be shown
                let map = std::mem::take(&mut self.map);
                return Err(LoadError { message: "a file could not be parsed".to_string(), diags: vec![d], map });
            }
        };
        for p in use_paths(&module) {
            self.queue.push((p, pkg));
        }
        self.by_path.insert(canon.clone(), self.mods.len());
        self.mods.push(ModuleSrc { path: canon, module, builtin: false, package: self.pkgs[pkg].prefix.clone(), file: Some(file_idx) });
        Ok(())
    }

    fn drain(&mut self) -> Result<(), LoadError> {
        while let Some((path, pkg)) = self.queue.pop() {
            self.resolve_use(&path, pkg)?;
        }
        Ok(())
    }

    /// Load the module files that a `use` path names (each prefix of the path that is a module).
    fn resolve_use(&mut self, segs: &[String], pkg: usize) -> Result<(), LoadError> {
        let Some(first) = segs.first() else { return Ok(()) };
        if RESERVED.contains(&first.as_str()) {
            return Ok(()); // the compiler's library (or an error the checker reports)
        }
        let dep = self.pkgs[pkg].deps.iter().find(|(n, _)| n == first).map(|(n, d)| (n.clone(), d.clone()));
        let local = module_file(&self.pkgs[pkg].src, &segs[..1]).is_some();
        if dep.is_some() && local {
            return Err(self.fail(format!("`use {first}` is ambiguous: there is a module `{first}` in this package and a dependency of the same name")));
        }
        if let Some((name, dir)) = dep {
            let dpkg = self.ensure_package(&name, &dir)?;
            for k in 1..=segs.len() {
                let rel = &segs[1..k];
                let file = if rel.is_empty() { Some(self.pkgs[dpkg].src.join("lib.tau")).filter(|f| f.is_file()) } else { module_file(&self.pkgs[dpkg].src, rel) };
                let Some(file) = file else { continue };
                let canon = if rel.is_empty() { name.clone() } else { format!("{name}.{}", rel.join(".")) };
                self.add_module(&file, canon, dpkg)?;
            }
        } else {
            let prefix = self.pkgs[pkg].prefix.clone();
            for k in 1..=segs.len() {
                let rel = &segs[..k];
                let Some(file) = module_file(&self.pkgs[pkg].src, rel) else { continue };
                let canon = format!("{prefix}{}", rel.join("."));
                self.add_module(&file, canon, pkg)?;
            }
        }
        Ok(())
    }

    /// Register the dependency package `name` in `dir` (once), with its own dependencies.
    fn ensure_package(&mut self, name: &str, dir: &Path) -> Result<usize, LoadError> {
        if let Some(&i) = self.pkg_by_name.get(name) {
            if self.pkg_dirs.get(&i).map(|d| d != dir).unwrap_or(false) {
                return Err(self.fail(format!(
                    "two different packages are both called `{name}`: {} and {}. A name can stand for one package in a program.",
                    self.display(&self.pkg_dirs[&i]),
                    self.display(dir)
                )));
            }
            return Ok(i);
        }
        let deps = if dir.join(MANIFEST).is_file() {
            let m = Manifest::read(dir).map_err(|e| self.fail(e))?;
            self.resolve_deps(&m).map_err(|e| self.fail(e))?
        } else {
            Vec::new()
        };
        if !dir.join("src").join("lib.tau").is_file() {
            return Err(self.fail(format!("the package `{name}` ({}) has no src/lib.tau, the root module that `use {name}` refers to", self.display(dir))));
        }
        self.pkgs.push(Pkg { prefix: format!("{name}."), src: dir.join("src"), deps });
        let i = self.pkgs.len() - 1;
        self.pkg_by_name.insert(name.to_string(), i);
        self.pkg_dirs.insert(i, dir.to_path_buf());
        Ok(i)
    }

    /// Register every dependency of package `pkg`, and theirs (so that all of them are fetched).
    fn ensure_all(&mut self, pkg: usize) -> Result<(), LoadError> {
        let deps = self.pkgs[pkg].deps.clone();
        for (name, dir) in deps {
            let i = self.ensure_package(&name, &dir)?;
            self.ensure_all(i)?;
        }
        Ok(())
    }

    /// The folder of each dependency in a manifest.
    fn resolve_deps(&mut self, m: &Manifest) -> Result<Vec<(String, PathBuf)>, String> {
        let mut out = Vec::new();
        for d in &m.deps {
            let dir = match &d.spec {
                DepSpec::Path(p) => {
                    let dir = m.dir.join(p);
                    dir.canonicalize().map_err(|e| format!("the dependency `{}` of `{}`: cannot find {} ({e})", d.name, m.name, dir.display()))?
                }
                DepSpec::Git { .. } => crate::gitdeps::checkout(&mut self.lock, &self.home, d).map_err(|e| format!("the dependency `{}` of `{}`: {e}", d.name, m.name))?,
            };
            out.push((d.name.clone(), dir));
        }
        let mut seen = HashSet::new();
        for (n, _) in &out {
            if !seen.insert(n.clone()) {
                return Err(format!("the dependency `{n}` is listed twice"));
            }
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------ `tau new` and `tau add`

/// Create a package in `dir/name`: `tau.toml`, `src/main.tau`, `.gitignore`.
pub fn new_package(parent: &Path, name: &str, library: bool) -> Result<PathBuf, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("`{name}` is not a good package name (letters, digits, `_` and `-`)"));
    }
    let dir = parent.join(name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let write = |p: PathBuf, text: &str| std::fs::write(&p, text).map_err(|e| format!("cannot write {}: {e}", p.display()));
    write(dir.join(MANIFEST), &format!("[package]\nname = {}\nversion = \"0.1.0\"\n\n[dependencies]\n", toml::quote(name)))?;
    if library {
        write(dir.join("src").join("lib.tau"), "// The root module of this package: `use NAME` in a program that depends on it.\n\npub fn hello() -> str {\n    \"hello from a library\"\n}\n\n#[test]\nfn hello_says_hello() {\n    assert_eq(hello(), \"hello from a library\")\n}\n".replace("NAME", name).as_str())?;
    } else {
        write(dir.join("src").join("main.tau"), "fn main() {\n    print(\"hello, world\")\n}\n")?;
    }
    write(dir.join(".gitignore"), "tau.lock.tmp\n")?;
    Ok(dir)
}

/// Add `name = <spec>` under `[dependencies]` in the manifest text; the rest of the file is kept as it is.
pub fn add_dependency_text(text: &str, name: &str, spec: &str) -> Result<String, String> {
    let t = toml::parse(text).map_err(|e| format!("tau.toml, {e}"))?;
    if t.table("dependencies").and_then(|d| d.get(name)).is_some() {
        return Err(format!("the dependency `{name}` is already in tau.toml"));
    }
    let line = format!("{name} = {spec}");
    let lines: Vec<&str> = text.lines().collect();
    let header = lines.iter().position(|l| l.trim() == "[dependencies]");
    let mut out: Vec<String> = Vec::new();
    match header {
        Some(h) => {
            // after the last non-blank line of the section
            let mut end = lines.len();
            for (i, l) in lines.iter().enumerate().skip(h + 1) {
                if l.trim_start().starts_with('[') {
                    end = i;
                    break;
                }
            }
            let mut insert = h + 1;
            for (i, l) in lines.iter().enumerate().take(end).skip(h + 1) {
                if !l.trim().is_empty() {
                    insert = i + 1;
                }
            }
            for (i, l) in lines.iter().enumerate() {
                if i == insert {
                    out.push(line.clone());
                }
                out.push(l.to_string());
            }
            if insert >= lines.len() {
                out.push(line);
            }
        }
        None => {
            out.extend(lines.iter().map(|l| l.to_string()));
            if out.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                out.push(String::new());
            }
            out.push("[dependencies]".to_string());
            out.push(line);
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    // make sure that what we wrote is still a valid manifest
    toml::parse(&s).map_err(|e| format!("internal error: the edited tau.toml is not valid ({e})"))?;
    Ok(s)
}
