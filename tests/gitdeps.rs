//! Dependencies from git repositories, and `tau.lock`. The repositories are made on this machine
//! (a folder is a valid git address), so no network is needed; the tests are skipped without `git`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use tau::project::{self, LoadOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let dir = std::env::temp_dir().join(format!("tau-git-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir.canonicalize().unwrap())
    }
    fn write(&self, path: &str, text: &str) {
        let p = self.0.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    fn path(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(tau::project::display_path(dir))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A repository holding the package `fastmath`, whose `version()` returns `version`.
fn make_repo(t: &Tmp, version: &str) -> PathBuf {
    let repo = t.path("fastmath-repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["checkout", "-q", "-b", "main"]);
    commit(&repo, version);
    repo
}

fn commit(repo: &Path, version: &str) -> String {
    std::fs::write(repo.join("tau.toml"), "[package]\nname = \"fastmath\"\n").unwrap();
    std::fs::write(repo.join("src").join("lib.tau"), format!("pub fn version() -> str {{ \"{version}\" }}\n")).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", version]);
    git(repo, &["rev-parse", "HEAD"])
}

fn app(t: &Tmp, dep: &str) -> PathBuf {
    t.write("app/tau.toml", &format!("[package]\nname = \"app\"\n\n[dependencies]\nfastmath = {dep}\n"));
    t.write("app/src/main.tau", "use fastmath\nfn main() {\n    print(fastmath.version())\n}\n");
    t.path("app")
}

fn quote_path(p: &Path) -> String {
    format!("\"{}\"", tau::project::display_path(p).replace('\\', "/"))
}

fn run(app: &Path, opts: LoadOptions) -> Result<String, String> {
    let ws = project::load(app, opts).map_err(|e| e.render())?;
    let cap = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    #[derive(Clone)]
    struct W(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let r = tau::run_workspace(&ws, Box::new(W(cap.clone())), u64::MAX);
    if !r.diagnostics.is_empty() {
        return Err(r.diagnostics.iter().map(|d| ws.render(d)).collect::<Vec<_>>().join("\n"));
    }
    let out = String::from_utf8(cap.lock().unwrap().clone()).unwrap();
    Ok(out)
}

fn opts(home: &Path) -> LoadOptions {
    LoadOptions { home: Some(home.to_path_buf()), ..Default::default() }
}

#[test]
fn a_tag_is_fetched_pinned_and_then_works_without_the_repository() {
    if !have_git() {
        eprintln!("skipped: no git");
        return;
    }
    let t = Tmp::new();
    let repo = make_repo(&t, "1.0");
    git(&repo, &["tag", "v1"]);
    let app = app(&t, &format!("{{ git = {}, tag = \"v1\" }}", quote_path(&repo)));
    let home = t.path("home");
    assert_eq!(run(&app, opts(&home)).unwrap(), "1.0\n");
    // the lock file pins the commit
    let lock = std::fs::read_to_string(app.join("tau.lock")).unwrap();
    let rev = git(&repo, &["rev-parse", "v1"]);
    assert!(lock.contains(&rev) && lock.contains("ref = \"tag:v1\"") && lock.contains("name = \"fastmath\""), "{lock}");
    // the tag moves to new code: the pinned commit is still used
    commit(&repo, "2.0");
    git(&repo, &["tag", "-f", "v1"]);
    assert_eq!(run(&app, opts(&home)).unwrap(), "1.0\n");
    assert_eq!(run(&app, LoadOptions { update_locks: true, ..opts(&home) }).unwrap(), "1.0\n", "a tag stays pinned even with --update");
    // and the repository itself is no longer needed
    std::fs::remove_dir_all(&repo).unwrap();
    assert_eq!(run(&app, opts(&home)).unwrap(), "1.0\n");
    // without the lock file the moved tag is seen... but the repository is gone, so this is an error
    std::fs::remove_file(app.join("tau.lock")).unwrap();
    let e = run(&app, opts(&t.path("another-home"))).unwrap_err();
    assert!(e.contains("cannot fetch"), "{e}");
}

#[test]
fn a_branch_moves_only_when_asked() {
    if !have_git() {
        return;
    }
    let t = Tmp::new();
    let repo = make_repo(&t, "1.0");
    let app = app(&t, &format!("{{ git = {}, branch = \"main\" }}", quote_path(&repo)));
    let home = t.path("home");
    assert_eq!(run(&app, opts(&home)).unwrap(), "1.0\n");
    commit(&repo, "2.0");
    assert_eq!(run(&app, opts(&home)).unwrap(), "1.0\n", "the lock keeps the old commit");
    assert_eq!(run(&app, LoadOptions { update_locks: true, ..opts(&home) }).unwrap(), "2.0\n");
    let lock = std::fs::read_to_string(app.join("tau.lock")).unwrap();
    assert!(lock.contains(&git(&repo, &["rev-parse", "HEAD"])), "{lock}");
    assert_eq!(run(&app, opts(&home)).unwrap(), "2.0\n");
}

#[test]
fn a_revision_and_the_default_branch() {
    if !have_git() {
        return;
    }
    let t = Tmp::new();
    let repo = make_repo(&t, "1.0");
    let first = git(&repo, &["rev-parse", "HEAD"]);
    commit(&repo, "2.0");
    let home = t.path("home");
    let a = app(&t, &format!("{{ git = {}, rev = \"{first}\" }}", quote_path(&repo)));
    assert_eq!(run(&a, opts(&home)).unwrap(), "1.0\n");
    // a short revision works too
    let t2 = Tmp::new();
    let a2 = app(&t2, &format!("{{ git = {}, rev = \"{}\" }}", quote_path(&repo), &first[..10]));
    assert_eq!(run(&a2, opts(&home)).unwrap(), "1.0\n");
    // no reference at all: the latest commit
    let t3 = Tmp::new();
    let a3 = app(&t3, &format!("{{ git = {} }}", quote_path(&repo)));
    assert_eq!(run(&a3, opts(&home)).unwrap(), "2.0\n");
}

#[test]
fn problems_with_git_dependencies_are_explained() {
    if !have_git() {
        return;
    }
    let t = Tmp::new();
    let repo = make_repo(&t, "1.0");
    let home = t.path("home");
    let a = app(&t, &format!("{{ git = {}, tag = \"nope\" }}", quote_path(&repo)));
    let e = run(&a, opts(&home)).unwrap_err();
    assert!(e.contains("has no tag `nope`"), "{e}");
    let t2 = Tmp::new();
    let a = app(&t2, &format!("{{ git = {}, branch = \"nope\" }}", quote_path(&repo)));
    assert!(run(&a, opts(&home)).unwrap_err().contains("has no branch `nope`"));
    let t3 = Tmp::new();
    let a = app(&t3, &format!("{{ git = {}, rev = \"deadbeefdeadbeef\" }}", quote_path(&repo)));
    assert!(run(&a, opts(&home)).unwrap_err().contains("has no commit"));
    let t4 = Tmp::new();
    let a = app(&t4, "{ git = \"/no/such/repository\", tag = \"v1\" }");
    assert!(run(&a, opts(&home)).unwrap_err().contains("cannot fetch"));
    // values that git could take for options, or that climb out of the folder
    for bad in ["{ git = \"--upload-pack=x\" }", "{ git = \"/x\", tag = \"--x\" }", "{ git = \"/x\", branch = \"../x\" }", "{ git = \"/x\", rev = \"a b\" }"] {
        let tb = Tmp::new();
        let a = app(&tb, bad);
        assert!(run(&a, opts(&home)).unwrap_err().contains("is not allowed"), "{bad}");
    }
    // a repository that is not a package
    let t5 = Tmp::new();
    let plain = t5.path("plain");
    std::fs::create_dir_all(&plain).unwrap();
    git(&plain, &["init", "-q"]);
    std::fs::write(plain.join("README"), "x").unwrap();
    git(&plain, &["add", "-A"]);
    git(&plain, &["commit", "-q", "-m", "x"]);
    let a = app(&t5, &format!("{{ git = {} }}", quote_path(&plain)));
    assert!(run(&a, opts(&home)).unwrap_err().contains("no src/lib.tau"));
}

#[test]
fn a_damaged_lock_file_is_reported() {
    let t = Tmp::new();
    let a = app(&t, "{ path = \"../nowhere\" }");
    std::fs::write(a.join("tau.lock"), "[[package]]\nname = \"x\"\n").unwrap();
    let e = run(&a, LoadOptions::default()).unwrap_err();
    assert!(e.contains("tau.lock") && e.contains("delete the file"), "{e}");
}

#[test]
fn the_command_line_fetches_and_runs() {
    if !have_git() {
        return;
    }
    let t = Tmp::new();
    let repo = make_repo(&t, "1.0");
    git(&repo, &["tag", "v1"]);
    let a = app(&t, &format!("{{ git = {}, tag = \"v1\" }}", quote_path(&repo)));
    let home = t.path("home");
    let tau = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_tau")).current_dir(&a).env("TAU_HOME", &home).args(args).output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    let (ok, out, err) = tau(&["fetch"]);
    assert!(ok && out.contains("dependencies are ready"), "{out}{err}");
    assert!(a.join("tau.lock").is_file());
    let (ok, out, err) = tau(&["run", "--interp"]);
    assert!(ok && out == "1.0\n", "{out}{err}");
    let (ok, _, err) = tau(&["fetch", "--bogus"]);
    assert!(!ok && err.contains("unknown option"), "{err}");
}
