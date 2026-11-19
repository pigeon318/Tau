//! `tau update`, end to end: a copy of the real program updates itself from a release server that
//! runs on this machine (`TAU_UPDATE_URL`). The same test runs on Windows, where replacing a running
//! program works differently.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_tau");

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop
        let _ = std::net::TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

/// A fake GitHub: `tag` is the latest release; the program for this platform is `payload`, and
/// `sums` is the text of SHA256SUMS. `release_status` is what `/releases/latest` answers with.
fn serve(tag: &'static str, payload: Vec<u8>, sums: String, release_status: u16) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let stop = Arc::new(AtomicBool::new(false));
    let (stop2, base) = (stop.clone(), url.clone());
    let asset = tau::update::asset_name().unwrap_or("tau-test");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if stop2.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body): (u16, Vec<u8>) = if path == "/releases/latest" {
                if release_status != 200 {
                    (release_status, b"{\"message\":\"Not Found\"}".to_vec())
                } else {
                    let json = format!(
                        "{{\"tag_name\":\"{tag}\",\"assets\":[{{\"name\":\"{asset}\",\"url\":\"{base}/assets/1\"}},{{\"name\":\"SHA256SUMS\",\"url\":\"{base}/assets/2\"}}]}}"
                    );
                    (200, json.into_bytes())
                }
            } else if path == "/assets/1" {
                (200, payload.clone())
            } else if path == "/assets/2" {
                (200, sums.clone().into_bytes())
            } else {
                (404, Vec::new())
            };
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = c.write_all(head.as_bytes());
            let _ = c.write_all(&body);
        }
    });
    Server { url, stop }
}

fn have_curl() -> bool {
    Command::new("curl").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// A private copy of the program in a fresh folder.
fn copy_of_tau(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tau-update-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(if cfg!(windows) { "tau.exe" } else { "tau" });
    std::fs::copy(BIN, &exe).unwrap();
    exe
}

fn run(exe: &PathBuf, server: &Server, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(exe).arg("update").args(args).env("TAU_UPDATE_URL", &server.url).env("GITHUB_TOKEN", "test-token").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

/// The program as it will be in the release: still runs, but is not the same file.
fn new_release_bytes() -> Vec<u8> {
    let mut b = std::fs::read(BIN).unwrap();
    b.extend_from_slice(b"\0tau-update-test-payload");
    b
}

fn sums_for(bytes: &[u8]) -> String {
    format!("{}  {}\n", tau::update::sha256_hex(bytes), tau::update::asset_name().unwrap_or("tau-test"))
}

#[test]
fn updates_itself_to_a_newer_release() {
    if !have_curl() || tau::update::asset_name().is_none() {
        eprintln!("skipped: needs curl and a platform with prebuilt releases");
        return;
    }
    let payload = new_release_bytes();
    let server = serve("v99.0.0", payload.clone(), sums_for(&payload), 200);
    let exe = copy_of_tau("ok");
    let (ok, out, err) = run(&exe, &server, &[]);
    assert!(ok, "stdout: {out}\nstderr: {err}");
    assert!(out.contains("updated tau") && out.contains("v99.0.0"), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), payload, "the program was not replaced");
    // the updated program still works (and tidies up what was left behind)
    let v = Command::new(&exe).arg("--version").output().unwrap();
    assert!(v.status.success() && String::from_utf8_lossy(&v.stdout).starts_with("tau "));
    let leftovers: Vec<_> = std::fs::read_dir(exe.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.contains(".tau-update") || n.ends_with(".old")).collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn check_only_changes_nothing() {
    if !have_curl() || tau::update::asset_name().is_none() {
        return;
    }
    let payload = new_release_bytes();
    let server = serve("v99.0.0", payload.clone(), sums_for(&payload), 200);
    let exe = copy_of_tau("check");
    let before = std::fs::read(&exe).unwrap();
    let (ok, out, _) = run(&exe, &server, &["--check"]);
    assert!(ok && out.contains("a newer tau is available: v99.0.0"), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), before);
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn does_nothing_when_up_to_date() {
    if !have_curl() || tau::update::asset_name().is_none() {
        return;
    }
    let payload = new_release_bytes();
    let tag: &'static str = Box::leak(format!("v{}", env!("CARGO_PKG_VERSION")).into_boxed_str());
    let server = serve(tag, payload.clone(), sums_for(&payload), 200);
    let exe = copy_of_tau("same");
    let before = std::fs::read(&exe).unwrap();
    let (ok, out, _) = run(&exe, &server, &[]);
    assert!(ok && out.contains("up to date"), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), before);
    // --force installs it anyway
    let (ok, out, err) = run(&exe, &server, &["--force"]);
    assert!(ok, "{out}{err}");
    assert_eq!(std::fs::read(&exe).unwrap(), payload);
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn a_damaged_download_is_refused() {
    if !have_curl() || tau::update::asset_name().is_none() {
        return;
    }
    let payload = new_release_bytes();
    // the checksum is for different bytes
    let server = serve("v99.0.0", payload.clone(), sums_for(b"something else"), 200);
    let exe = copy_of_tau("bad");
    let before = std::fs::read(&exe).unwrap();
    let (ok, _, err) = run(&exe, &server, &[]);
    assert!(!ok && err.contains("damaged") && err.contains("nothing was changed"), "{err}");
    assert_eq!(std::fs::read(&exe).unwrap(), before, "the program must be untouched");
    let leftovers = std::fs::read_dir(exe.parent().unwrap()).unwrap().count();
    assert_eq!(leftovers, 1, "the failed download was not cleaned up");
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn a_download_that_does_not_run_is_refused() {
    if !have_curl() || tau::update::asset_name().is_none() {
        return;
    }
    let payload = b"this is not a program".to_vec();
    let server = serve("v99.0.0", payload.clone(), sums_for(&payload), 200);
    let exe = copy_of_tau("broken");
    let before = std::fs::read(&exe).unwrap();
    let (ok, _, err) = run(&exe, &server, &[]);
    assert!(!ok && err.contains("does not"), "{err}");
    assert_eq!(std::fs::read(&exe).unwrap(), before);
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn explains_a_private_repository() {
    if !have_curl() || tau::update::asset_name().is_none() {
        return;
    }
    let server = serve("v99.0.0", Vec::new(), String::new(), 404);
    let exe = copy_of_tau("private");
    // no token (unless the GitHub CLI on this machine is logged in): the message says how to give one
    let o = Command::new(&exe).arg("update").env("TAU_UPDATE_URL", &server.url).env_remove("GITHUB_TOKEN").env_remove("GH_TOKEN").env_remove("TAU_GITHUB_TOKEN").output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!o.status.success() && err.contains("GitHub says there is nothing there"), "{err}");
    if !err.contains("There may be no release yet") {
        assert!(err.contains("GITHUB_TOKEN") && err.contains("private"), "{err}");
    }
    // with a token that GitHub does not accept
    let (ok, _, err) = run(&exe, &server, &[]);
    assert!(!ok && err.contains("nothing there"), "{err}");
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
}

#[test]
fn a_program_built_from_source_is_not_updated() {
    // BIN is in target/debug or target/release
    let server = serve("v99.0.0", Vec::new(), String::new(), 200);
    let o = Command::new(BIN).arg("update").env_remove("TAU_UPDATE_URL").output().unwrap();
    let _ = server;
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("built from source"), "{err}");
}

#[test]
fn bad_options_are_rejected() {
    let o = Command::new(BIN).args(["update", "--frobnicate"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}
