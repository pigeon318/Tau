//! `tau update`: replace this executable with the newest release.
//!
//! It asks GitHub for the latest release, downloads the program for this platform, checks it against
//! the release's `SHA256SUMS`, runs it once (`--version`) to see that it works, and then swaps it in.
//! Downloading is done by `curl` (which comes with Windows 10 and later), so Tau needs no HTTP library.
//!
//! A public repository needs no token. For a private one a token is needed: `GITHUB_TOKEN` or `GH_TOKEN`
//! (a token that can read the repository), or a logged-in GitHub CLI (`gh auth login`).
//!
//! `TAU_UPDATE_URL` replaces the GitHub API address (`https://api.github.com/repos/pigeon318/Tau`);
//! the tests use it to talk to a server on this machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_API: &str = "https://api.github.com/repos/pigeon318/Tau";

// ------------------------------------------------------------------ versions

/// `0.1.0` or `v0.1.0` as numbers; pre-release suffixes (`-rc1`) are ignored.
pub fn parse_version(s: &str) -> Option<[u64; 3]> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut v = [0u64; 3];
    for slot in v.iter_mut() {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(v)
}

/// The name of the release file for this platform.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("tau-windows-x86_64.exe"),
        ("linux", "x86_64") => Some("tau-linux-x86_64"),
        _ => None,
    }
}

// ------------------------------------------------------------------ SHA-256

pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// The hash listed for `name` in the text of a `SHA256SUMS` file (`<hash>  <name>` per line).
pub fn hash_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        (file.trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

// ------------------------------------------------------------------ a small JSON reader

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = JsonParser { s: text.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err("extra text after the JSON value".into());
    }
    Ok(v)
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected `{}` at byte {}", c as char, self.i))
        }
    }
    fn word(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(format!("unexpected text at byte {}", self.i))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 64 {
            return Err("JSON nested too deeply".into());
        }
        self.ws();
        match self.s.get(self.i) {
            None => Err("unexpected end of JSON".into()),
            Some(b'n') => self.word("null", Json::Null),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(format!("expected `,` or `]` at byte {}", self.i)),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(items));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    let v = self.value(depth + 1)?;
                    items.push((k, v));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(items));
                        }
                        _ => return Err(format!("expected `,` or `}}` at byte {}", self.i)),
                    }
                }
            }
            Some(_) => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i]).ok().and_then(|t| t.parse().ok()).map(Json::Num).ok_or_else(|| format!("unexpected text at byte {start}"))
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let t = self.s.get(self.i..self.i + 4).and_then(|b| std::str::from_utf8(b).ok()).ok_or("bad \\u escape")?;
        self.i += 4;
        u32::from_str_radix(t, 16).map_err(|_| "bad \\u escape".to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err("unterminated string".into()) };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else { return Err("unterminated string".into()) };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF)
                            } else {
                                hi
                            };
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return Err("bad escape in a string".into()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "a string is not UTF-8".into())
    }
}

// ------------------------------------------------------------------ talking to GitHub

fn token() -> Option<String> {
    for var in ["TAU_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    // a logged-in GitHub CLI
    let out = Command::new("gh").args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// GET `url`; the body, or a message that says what went wrong. `accept` is the `Accept` header.
fn get(url: &str, accept: &str, token: &Option<String>, to_file: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--location", "--max-time", "600", "--write-out", "\n%{http_code}", "-H", &format!("Accept: {accept}"), "-H", "User-Agent: tau-update"]);
    if let Some(t) = token {
        cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
    }
    if let Some(p) = to_file {
        cmd.arg("--output").arg(p);
    }
    cmd.arg(url).stdin(Stdio::null());
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "`curl` was not found. Install curl (Windows 10 and later have it) to use `tau update`.".to_string()
        } else {
            format!("could not run curl: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let body = out.stdout;
    let cut = body.iter().rposition(|&b| b == b'\n').ok_or("curl gave no status")?;
    let code: u32 = String::from_utf8_lossy(&body[cut + 1..]).trim().parse().unwrap_or(0);
    let body = body[..cut].to_vec();
    match code {
        200 => Ok(body),
        401 | 403 | 404 if token.is_none() => Err("GitHub says there is nothing there. The Tau repository is private, so `tau update` needs a token: set GITHUB_TOKEN to a token that can read the repository's contents, or install the GitHub CLI and run `gh auth login`.".into()),
        401 | 403 => Err("GitHub refused the token. Is it allowed to read the repository's contents, and has it not expired?".into()),
        404 => Err("GitHub says there is nothing there. There may be no release yet, or the token cannot see the repository.".into()),
        c => Err(format!("GitHub answered with status {c}")),
    }
}

// ------------------------------------------------------------------ replacing the executable

/// Move the new program into the place of `exe`. On Windows a running program cannot be overwritten
/// but it can be renamed, so the old one is kept as `<exe>.old` (deleted by the next `tau` that starts).
fn install(new: &Path, exe: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("cannot make the new program executable: {e}"))?;
        std::fs::rename(new, exe).map_err(|e| format!("cannot replace {}: {e}", exe.display()))
    }
    #[cfg(not(unix))]
    {
        let old = old_path(exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old).map_err(|e| format!("cannot move the old program aside ({}): {e}", exe.display()))?;
        if let Err(e) = std::fs::rename(new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(format!("cannot put the new program in place: {e}"));
        }
        Ok(())
    }
}

fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// Called when `tau` starts: remove the program left behind by the last update (Windows).
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

// ------------------------------------------------------------------ the command

pub struct Options {
    /// Only say whether a newer version exists.
    pub check_only: bool,
    /// Install the latest release even if it is not newer.
    pub force: bool,
}

/// Run `tau update`. Returns the lines to print, or an error message.
pub fn run(opts: &Options) -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    // a program built from source updates with git and cargo
    let in_target = exe.components().any(|c| c.as_os_str() == "target") && exe.parent().and_then(|p| p.file_name()).map(|n| n == "debug" || n == "release").unwrap_or(false);
    if in_target && std::env::var("TAU_UPDATE_URL").is_err() {
        return Err("this tau was built from source (it is in a `target` folder). Update it with `git pull` and `cargo build --release`.".into());
    }
    let asset = asset_name().ok_or_else(|| format!("there is no prebuilt tau for {} {} yet. Build it from source.", std::env::consts::OS, std::env::consts::ARCH))?;
    let api = std::env::var("TAU_UPDATE_URL").unwrap_or_else(|_| DEFAULT_API.to_string());
    let api = api.trim_end_matches('/');
    let token = token();

    let body = get(&format!("{api}/releases/latest"), "application/vnd.github+json", &token, None)?;
    let release = parse_json(&String::from_utf8_lossy(&body)).map_err(|e| format!("GitHub's answer was not understood: {e}"))?;
    let tag = release.get("tag_name").and_then(Json::as_str).ok_or("GitHub's answer has no release name")?.to_string();
    let latest = parse_version(&tag).ok_or_else(|| format!("the latest release is called `{tag}`, which is not a version number"))?;
    let have = parse_version(current).ok_or("this program's own version is not a number")?;
    if latest <= have && !opts.force {
        return Ok(format!("tau {current} is up to date (the latest release is {tag})"));
    }
    if opts.check_only {
        return Ok(format!("a newer tau is available: {tag} (you have {current}). Run `tau update` to install it."));
    }
    let assets = release.get("assets").and_then(Json::as_arr).ok_or("the release has no files")?;
    let find = |name: &str| assets.iter().find(|a| a.get("name").and_then(Json::as_str) == Some(name)).and_then(|a| a.get("url")).and_then(Json::as_str).map(str::to_string);
    let url = find(asset).ok_or_else(|| format!("release {tag} has no file called {asset}"))?;
    let sums_url = find("SHA256SUMS").ok_or_else(|| format!("release {tag} has no SHA256SUMS file"))?;

    let dir = exe.parent().ok_or("this program has no folder")?;
    let new = dir.join(format!(".tau-update-{}", std::process::id()));
    let cleanup_new = |r: Result<String, String>| {
        if r.is_err() {
            let _ = std::fs::remove_file(&new);
        }
        r
    };
    cleanup_new((|| {
        eprintln!("downloading tau {tag}...");
        let sums = get(&sums_url, "application/octet-stream", &token, None)?;
        let want = hash_for(&String::from_utf8_lossy(&sums), asset).ok_or_else(|| format!("SHA256SUMS does not list {asset}"))?;
        get(&url, "application/octet-stream", &token, Some(&new)).map_err(|e| format!("{e} (the folder {} must be writable)", dir.display()))?;
        let bytes = std::fs::read(&new).map_err(|e| format!("cannot read the download: {e}"))?;
        let got = sha256_hex(&bytes);
        if got != want {
            return Err(format!("the download is damaged (its checksum is {got}, expected {want}); nothing was changed"));
        }
        // it must at least start
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755));
        }
        let out = Command::new(&new).arg("--version").stdin(Stdio::null()).output().map_err(|e| format!("the new program does not start: {e}"))?;
        if !out.status.success() || !String::from_utf8_lossy(&out.stdout).starts_with("tau ") {
            return Err("the new program does not work on this machine; nothing was changed".into());
        }
        install(&new, &exe)?;
        Ok(format!("updated tau {current} -> {tag}"))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.1.0"), Some([0, 1, 0]));
        assert_eq!(parse_version("v1.20.3"), Some([1, 20, 3]));
        assert_eq!(parse_version("v2.0.0-rc1"), Some([2, 0, 0]));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("v0.0.1") < parse_version("0.1.0"));
    }

    #[test]
    fn sha256_known_values() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let long = vec![b'a'; 1_000_000];
        assert_eq!(sha256_hex(&long), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // the padding boundary: 55, 56 and 64 bytes
        assert_eq!(sha256_hex(&[b'a'; 55]), "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
        assert_eq!(sha256_hex(&[b'a'; 56]), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
        assert_eq!(sha256_hex(&[b'a'; 64]), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
    }

    #[test]
    fn checksum_files() {
        let sums = "AAAA11  tau-linux-x86_64\nbbbb22 *tau-windows-x86_64.exe\n";
        assert_eq!(hash_for(sums, "tau-linux-x86_64").as_deref(), Some("aaaa11"));
        assert_eq!(hash_for(sums, "tau-windows-x86_64.exe").as_deref(), Some("bbbb22"));
        assert_eq!(hash_for(sums, "nope"), None);
    }

    #[test]
    fn json() {
        let j = parse_json(r#" {"tag_name": "v0.1.0", "n": -1.5e2, "ok": true, "x": null, "assets": [{"name": "aé\n\"q\"", "url": "http://h/1"}, {}], "emoji": "😀"} "#).unwrap();
        assert_eq!(j.get("tag_name").and_then(Json::as_str), Some("v0.1.0"));
        assert_eq!(j.get("n"), Some(&Json::Num(-150.0)));
        assert_eq!(j.get("assets").and_then(Json::as_arr).unwrap().len(), 2);
        assert_eq!(j.get("assets").unwrap().as_arr().unwrap()[0].get("name").and_then(Json::as_str), Some("a\u{e9}\n\"q\""));
        assert_eq!(j.get("emoji").and_then(Json::as_str), Some("\u{1F600}"));
        assert!(parse_json("{").is_err());
        assert!(parse_json("[1,]").is_err());
        assert!(parse_json("{\"a\":1} x").is_err());
        assert!(parse_json(&"[".repeat(100)).is_err());
    }
}
