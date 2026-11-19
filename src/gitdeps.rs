//! Dependencies fetched with git, and the lock file that pins them.
//!
//! A dependency `{ git = "URL", tag = "v1.2" }` is fetched once into the Tau home folder
//! (`$TAU_HOME`, default `~/.tau`) and checked out at an exact commit. The commit is recorded in
//! `tau.lock` next to `tau.toml`, so that the same program builds with the same code later (a tag or
//! branch may move; the lock does not). `tau fetch --update` forgets the pinned commits of branches
//! (and of dependencies without a tag or revision) and looks them up again.
//!
//! The `git` command does the work, so nothing here needs a network library.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::project::{DepSpec, Dependency, GitRef};
use crate::toml::{self, Value};

pub const LOCK: &str = "tau.lock";

#[derive(Debug, Clone, PartialEq)]
pub struct LockEntry {
    pub name: String,
    pub git: String,
    /// `default`, `tag:v1`, `branch:main` or `rev:abc`: what the manifest asked for.
    pub reference: String,
    /// The commit that was resolved.
    pub rev: String,
}

#[derive(Debug, Default)]
pub struct Lock {
    pub entries: Vec<LockEntry>,
    changed: bool,
}

impl Lock {
    /// The lock file of the package in `dir` (empty if there is none).
    pub fn read(dir: &Path) -> Result<Lock, String> {
        let path = dir.join(LOCK);
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(Lock::default()) };
        let t = toml::parse(&text).map_err(|e| format!("{}: {e}", crate::project::display_path(&path)))?;
        let mut entries = Vec::new();
        if let Some(Value::Array(items)) = t.get("package") {
            for i in items {
                let Value::Table(i) = i else { continue };
                let (Some(name), Some(git), Some(reference), Some(rev)) = (i.str("name"), i.str("git"), i.str("ref"), i.str("rev")) else {
                    return Err(format!("{}: a [[package]] entry needs name, git, ref and rev (delete the file to make a new one)", crate::project::display_path(&path)));
                };
                entries.push(LockEntry { name: name.into(), git: git.into(), reference: reference.into(), rev: rev.into() });
            }
        }
        Ok(Lock { entries, changed: false })
    }

    /// Write the file if something was added or changed.
    pub fn write(&self, dir: &Path) -> Result<(), String> {
        if !self.changed {
            return Ok(());
        }
        let mut text = String::from("# This file is made by tau. It pins the exact commit of each git dependency.\n# Commit it to keep builds reproducible.\n");
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| (&a.name, &a.git, &a.reference).cmp(&(&b.name, &b.git, &b.reference)));
        for e in &entries {
            text.push_str(&format!("\n[[package]]\nname = {}\ngit = {}\nref = {}\nrev = {}\n", toml::quote(&e.name), toml::quote(&e.git), toml::quote(&e.reference), toml::quote(&e.rev)));
        }
        let path = dir.join(LOCK);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", crate::project::display_path(&path)))
    }

    fn find(&self, git: &str, reference: &str) -> Option<&LockEntry> {
        self.entries.iter().find(|e| e.git == git && e.reference == reference)
    }

    fn set(&mut self, name: &str, git: &str, reference: &str, rev: &str) {
        match self.entries.iter_mut().find(|e| e.git == git && e.reference == reference) {
            Some(e) => {
                if e.rev != rev || e.name != name {
                    e.rev = rev.to_string();
                    e.name = name.to_string();
                    self.changed = true;
                }
            }
            None => {
                self.entries.push(LockEntry { name: name.into(), git: git.into(), reference: reference.into(), rev: rev.into() });
                self.changed = true;
            }
        }
    }

    /// Forget the pinned commit of every dependency that is not fixed by a tag or a revision.
    pub fn forget_moving(&mut self) {
        let before = self.entries.len();
        self.entries.retain(|e| e.reference.starts_with("tag:") || e.reference.starts_with("rev:"));
        if self.entries.len() != before {
            self.changed = true;
        }
    }
}

fn reference_text(r: &GitRef) -> String {
    match r {
        GitRef::Default => "default".into(),
        GitRef::Tag(t) => format!("tag:{t}"),
        GitRef::Branch(b) => format!("branch:{b}"),
        GitRef::Rev(r) => format!("rev:{r}"),
    }
}

/// Where fetched dependencies are kept.
pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("TAU_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let base = if cfg!(windows) { std::env::var("USERPROFILE") } else { std::env::var("HOME") };
    PathBuf::from(base.unwrap_or_else(|_| ".".into())).join(".tau")
}

/// A path in the form git understands (on Windows without the `\\?\` prefix).
fn plain(p: &Path) -> PathBuf {
    PathBuf::from(crate::project::display_path(p))
}

fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A name that git cannot mistake for an option, and that cannot escape the folder.
fn safe_arg(what: &str, s: &str) -> Result<(), String> {
    // (a git address may contain `..`; it is only hashed, never used as a path)
    let dots = what != "git address" && s.contains("..");
    if s.is_empty() || s.starts_with('-') || dots || s.chars().any(|c| c.is_control() || c == ' ') {
        return Err(format!("the {what} `{s}` is not allowed"));
    }
    Ok(())
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    if let Some(d) = cwd {
        // (git does not understand Windows' `\\?\C:\...` form of a path)
        c.current_dir(plain(d));
    }
    let out = c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "`git` was not found; install git to use dependencies with `git = \"...\"`".to_string()
        } else {
            format!("cannot run git: {e}")
        }
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git failed");
        return Err(format!("git {}: {}", args.first().unwrap_or(&""), line.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Make the dependency available (fetching it if need be) and return the folder of the package.
/// `lock` pins the commit; a new or changed pin marks the lock as changed.
pub fn checkout(lock: &mut Lock, home: &Path, dep: &Dependency) -> Result<PathBuf, String> {
    let DepSpec::Git { url, reference } = &dep.spec else { return Err("not a git dependency".into()) };
    safe_arg("git address", url)?;
    let refname = reference_text(reference);
    if let GitRef::Tag(x) | GitRef::Branch(x) | GitRef::Rev(x) = reference {
        safe_arg("reference", x)?;
    }
    let key = short_hash(url);
    let db = home.join("git").join("db").join(&key);
    let pinned = lock.find(url, &refname).map(|e| e.rev.clone());

    // the repository, as a bare clone that all checkouts come from
    let have_db = db.join("HEAD").is_file();
    let mut rev = pinned.clone();
    let mut fetched = false;
    let ensure_db = |fetched: &mut bool| -> Result<(), String> {
        if *fetched {
            return Ok(());
        }
        std::fs::create_dir_all(db.parent().unwrap()).map_err(|e| format!("cannot create {}: {e}", db.display()))?;
        let db_s = plain(&db).to_string_lossy().to_string();
        if have_db {
            git(&["--git-dir", &db_s, "fetch", "--quiet", "--force", "--tags", "--prune", "origin", "+refs/heads/*:refs/heads/*"], None)?;
        } else {
            git(&["clone", "--quiet", "--bare", "--", url, &db_s], None).map_err(|e| format!("cannot fetch {url}: {e}"))?;
        }
        *fetched = true;
        Ok(())
    };

    // is the pinned commit already checked out?
    let dest_for = |rev: &str| home.join("git").join("src").join(format!("{key}-{}", &rev[..rev.len().min(16)]));
    let ready = |dest: &Path, rev: &str| std::fs::read_to_string(dest.join(".tau-ready")).map(|t| t == rev).unwrap_or(false);
    if let Some(r) = &rev {
        let dest = dest_for(r);
        if ready(&dest, r) {
            return Ok(dest);
        }
    }

    if rev.is_none() {
        ensure_db(&mut fetched)?;
        let db_s = plain(&db).to_string_lossy().to_string();
        let spec = match reference {
            GitRef::Default => "HEAD".to_string(),
            GitRef::Tag(t) => format!("refs/tags/{t}^{{commit}}"),
            GitRef::Branch(b) => format!("refs/heads/{b}^{{commit}}"),
            GitRef::Rev(r) => format!("{r}^{{commit}}"),
        };
        let full = git(&["--git-dir", &db_s, "rev-parse", "--verify", "--quiet", &spec], None).map_err(|_| match reference {
            GitRef::Default => format!("{url} has no default branch to use"),
            GitRef::Tag(t) => format!("{url} has no tag `{t}`"),
            GitRef::Branch(b) => format!("{url} has no branch `{b}`"),
            GitRef::Rev(r) => format!("{url} has no commit `{r}`"),
        })?;
        rev = Some(full);
    }
    let rev = rev.unwrap();
    let dest = dest_for(&rev);
    if !ready(&dest, &rev) {
        ensure_db(&mut fetched)?;
        let db_s = plain(&db).to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
        let dest_s = plain(&dest).to_string_lossy().to_string();
        let result = git(&["clone", "--quiet", "--no-checkout", "--", &db_s, &dest_s], None).and_then(|_| git(&["checkout", "--quiet", "--detach", &rev], Some(&dest)));
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("cannot check out {url} at {rev}: {e}"));
        }
        std::fs::write(dest.join(".tau-ready"), &rev).map_err(|e| format!("cannot write in {}: {e}", dest.display()))?;
    }
    lock.set(&dep.name, url, &refname, &rev);
    Ok(dest)
}
