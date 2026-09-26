//! Small path and time helpers.

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Make `p` absolute against the current directory and lexically normalize it
/// (no filesystem access, no `\\?\` prefixes).
pub fn absolute(p: &Path) -> PathBuf {
    let base = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |d| d.join(p))
    };
    let mut out = PathBuf::new();
    for c in base.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Path as a forward-slash string (stable across platforms, compact in output).
pub fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// `rel` path of `path` under `root` with forward slashes, if it is under it
/// (case-insensitive on Windows).
pub fn rel_under(root: &Path, path: &Path) -> Option<String> {
    let r = slash(root);
    let p = slash(path);
    let r = r.trim_end_matches('/');
    let (rl, pl) = if cfg!(windows) {
        (r.to_ascii_lowercase(), p.to_ascii_lowercase())
    } else {
        (r.to_string(), p.clone())
    };
    if pl == rl {
        return Some(String::new());
    }
    if pl.starts_with(&rl) && pl.as_bytes().get(rl.len()) == Some(&b'/') {
        return Some(p[r.len() + 1..].to_string());
    }
    None
}

/// UNC / device / verbatim paths (`\\host\share`, `//host/share`, `\\?\UNC\...`,
/// `\\.\pipe\x`). Checked purely lexically so that rejecting them never touches the
/// filesystem (an `exists()` on a UNC path starts SMB authentication and can leak an
/// NTLM hash).
pub fn is_remote_path(s: &str) -> bool {
    let b = s.trim().as_bytes();
    b.len() >= 2 && matches!(b[0], b'\\' | b'/') && matches!(b[1], b'\\' | b'/')
}

/// Drop the `\\?\` prefix `canonicalize` adds on Windows (`\\?\C:\x` -> `C:\x`).
/// Verbatim UNC paths are left as-is (and rejected by callers).
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest.to_string()),
        _ => p,
    }
}

/// Canonicalize configured roots once (non-existent or remote roots are dropped).
pub fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|r| !is_remote_path(&r.to_string_lossy()))
        .filter_map(|r| std::fs::canonicalize(r).ok().map(strip_verbatim))
        .collect()
}

/// Validate an untrusted path argument: reject UNC/device paths lexically, then
/// canonicalize (resolving `..` and links) and require it to sit under one of the
/// canonical `roots`. Returns the canonical path.
pub fn confine(path: &str, roots: &[PathBuf]) -> Result<PathBuf, String> {
    if is_remote_path(path) {
        return Err(format!("`{path}`: UNC and device paths are not allowed"));
    }
    let abs = absolute(Path::new(path.trim()));
    if is_remote_path(&abs.to_string_lossy()) {
        return Err(format!("`{path}`: resolves to a UNC path"));
    }
    if roots.is_empty() {
        return Err(format!("`{path}`: no configured roots"));
    }
    let canon = std::fs::canonicalize(&abs)
        .map(strip_verbatim)
        .map_err(|e| format!("`{path}`: {e}"))?;
    if is_remote_path(&canon.to_string_lossy()) {
        return Err(format!("`{path}`: resolves to a UNC path"));
    }
    if roots.iter().any(|r| rel_under(r, &canon).is_some()) {
        Ok(canon)
    } else {
        Err(format!("`{path}` is outside the configured roots"))
    }
}

/// Default cap for repo-controlled marker files (go.mod, Cargo.toml, package.json,
/// tsconfig.json): 1 MiB.
pub const MAX_REPO_FILE_BYTES: u64 = 1024 * 1024;

/// Read a repo-controlled file as bytes, defensively. `None` unless all hold:
///
/// - neither `root` nor `path` looks like a UNC/device path (checked lexically, before
///   any filesystem call, so a hostile link target can never start SMB auth);
/// - `path` itself is a regular file, not a symlink/junction, FIFO or device
///   (`symlink_metadata`);
/// - its canonical path sits under the canonical `root` (no escape through a
///   symlinked parent directory);
/// - it is at most `max_bytes` long, both by metadata and by what is actually read
///   (the read is bounded, so a file growing underneath us cannot exhaust memory).
pub fn read_repo_bytes(root: &Path, path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    if is_remote_path(&root.to_string_lossy()) || is_remote_path(&path.to_string_lossy()) {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > max_bytes {
        return None;
    }
    let croot = std::fs::canonicalize(root).ok().map(strip_verbatim)?;
    let cpath = std::fs::canonicalize(path).ok().map(strip_verbatim)?;
    if is_remote_path(&croot.to_string_lossy())
        || is_remote_path(&cpath.to_string_lossy())
        || rel_under(&croot, &cpath).is_none()
    {
        return None;
    }
    let file = std::fs::File::open(&cpath).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::with_capacity(meta.len().min(max_bytes) as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > max_bytes {
        return None;
    }
    Some(buf)
}

/// [`read_repo_bytes`] decoded as UTF-8 (`None` if it is not valid UTF-8).
pub fn read_repo_file(root: &Path, path: &Path, max_bytes: u64) -> Option<String> {
    String::from_utf8(read_repo_bytes(root, path, max_bytes)?).ok()
}

/// Test helper: create a file symlink, or return `false` when the platform/user may
/// not (Windows without Developer Mode or admin) so the caller can skip.
#[cfg(test)]
pub(crate) fn try_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(target, link);
    match r {
        Ok(()) => true,
        Err(e) => {
            eprintln!("skipping symlink case: {e}");
            false
        }
    }
}

/// Files that must never be read or indexed (credentials and secrets), by name.
pub fn is_secret_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let ext_deny = [".pem", ".key", ".pfx", ".p12", ".kdbx"];
    n.starts_with(".env")
        || ext_deny.iter().any(|e| n.ends_with(e))
        || n.starts_with("id_")
        || (n.starts_with("credentials")
            && Path::new(&n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json")))
        || n.starts_with("secrets.")
        || n.contains(".tfstate")
        || n == ".npmrc"
        || n == ".netrc"
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A file's mtime in nanoseconds since the epoch (0 when unavailable).
pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64)
}

/// Format seconds-since-epoch as `YYYY-MM-DD HH:MM:SS` UTC.
pub fn format_unix(ts: i64) -> String {
    if ts <= 0 {
        return "never".into();
    }
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

/// `CODEGRAPH_TRACE=1`: print phase timings to stderr (profiling large indexes).
pub fn trace(what: &str, since: std::time::Instant) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("CODEGRAPH_TRACE").is_some()) {
        eprintln!("[trace] {what}: {:.2?}", since.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths() {
        let root = Path::new("C:/a/b");
        assert_eq!(
            rel_under(root, Path::new("C:/a/b/c/d.go")).as_deref(),
            Some("c/d.go")
        );
        assert_eq!(rel_under(root, Path::new("C:/a/bc/d.go")), None);
        assert_eq!(rel_under(root, Path::new("C:/a/b")).as_deref(), Some(""));
    }

    #[test]
    fn normalize() {
        let p = absolute(Path::new("/x/y/../z/./w"));
        assert!(slash(&p).ends_with("x/z/w"));
    }

    #[test]
    fn remote_paths_are_rejected_lexically() {
        for p in [
            r"\\evil-host\share\x.go",
            "//evil-host/share/x.go",
            r"\\?\UNC\evil-host\share\x.go",
            r"\\.\pipe\x",
            r"\/host/x",
        ] {
            assert!(is_remote_path(p), "{p}");
            // With no roots the validator still rejects on the UNC rule first
            // (the message proves the filesystem was never consulted).
            let err = confine(p, &[]).unwrap_err();
            assert!(err.contains("UNC and device paths"), "{p}: {err}");
        }
        assert!(!is_remote_path(r"C:\Users\x.go"));
        assert!(!is_remote_path("src/main.rs"));
    }

    #[test]
    fn confine_requires_a_root() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.go"), "package a").unwrap();
        std::fs::write(other.path().join("b.go"), "package b").unwrap();
        let roots = canonical_roots(&[root.path().to_path_buf()]);
        let ok = confine(&root.path().join("a.go").to_string_lossy(), &roots).unwrap();
        assert!(ok.ends_with("a.go"));
        assert!(!ok.to_string_lossy().starts_with(r"\\?\"));
        let err = confine(&other.path().join("b.go").to_string_lossy(), &roots).unwrap_err();
        assert!(err.contains("outside the configured roots"), "{err}");
        // `..` escapes are resolved before the check.
        let sneaky = root.path().join("..").join(
            other
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
        assert!(confine(&sneaky.join("b.go").to_string_lossy(), &roots).is_err());
        assert!(confine("does/not/exist.go", &roots).is_err());
    }

    #[test]
    fn read_repo_file_accepts_regular_files_under_root() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("go.mod");
        std::fs::write(&p, "module a/b\n").unwrap();
        assert_eq!(
            read_repo_file(root.path(), &p, MAX_REPO_FILE_BYTES).as_deref(),
            Some("module a/b\n")
        );
        // Exactly at the cap is fine, one byte over is not.
        assert!(read_repo_file(root.path(), &p, 11).is_some());
        assert!(read_repo_file(root.path(), &p, 10).is_none());
        // Missing files, directories and non-UTF-8 content.
        assert!(read_repo_file(root.path(), &root.path().join("nope"), 100).is_none());
        assert!(read_repo_file(root.path(), root.path(), 100).is_none());
        std::fs::write(root.path().join("bin"), [0xff, 0xfe]).unwrap();
        assert!(read_repo_file(root.path(), &root.path().join("bin"), 100).is_none());
        assert!(read_repo_bytes(root.path(), &root.path().join("bin"), 100).is_some());
    }

    #[test]
    fn read_repo_file_rejects_oversize_and_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let big = root.path().join("package.json");
        std::fs::write(&big, vec![b' '; (MAX_REPO_FILE_BYTES + 1) as usize]).unwrap();
        assert!(read_repo_file(root.path(), &big, MAX_REPO_FILE_BYTES).is_none());
        let out = other.path().join("tsconfig.json");
        std::fs::write(&out, "{}").unwrap();
        assert!(read_repo_file(root.path(), &out, MAX_REPO_FILE_BYTES).is_none());
        // `..` that lands outside the root.
        let sneaky = root
            .path()
            .join("..")
            .join(other.path().file_name().unwrap())
            .join("tsconfig.json");
        assert!(read_repo_file(root.path(), &sneaky, MAX_REPO_FILE_BYTES).is_none());
    }

    #[test]
    fn read_repo_file_rejects_unc_lexically() {
        let root = tempfile::tempdir().unwrap();
        for p in [r"\\attacker\share\x", "//attacker/share/x", r"\\.\pipe\x"] {
            assert!(
                read_repo_file(root.path(), Path::new(p), 100).is_none(),
                "{p}"
            );
            assert!(
                read_repo_file(Path::new(p), Path::new(p), 100).is_none(),
                "{p}"
            );
        }
    }

    #[test]
    fn read_repo_file_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let secret = other.path().join("secret.txt");
        std::fs::write(&secret, "TOP SECRET").unwrap();
        let inside = root.path().join("real.json");
        std::fs::write(&inside, "{}").unwrap();
        // A link to a file outside the root, and even one to a file inside it.
        let l1 = root.path().join("tsconfig.json");
        let l2 = root.path().join("package.json");
        if !try_symlink(&secret, &l1) || !try_symlink(&inside, &l2) {
            return;
        }
        assert!(read_repo_file(root.path(), &l1, MAX_REPO_FILE_BYTES).is_none());
        assert!(read_repo_file(root.path(), &l2, MAX_REPO_FILE_BYTES).is_none());
        #[cfg(unix)]
        {
            let z = root.path().join("go.mod");
            std::os::unix::fs::symlink("/dev/zero", &z).unwrap();
            assert!(read_repo_file(root.path(), &z, MAX_REPO_FILE_BYTES).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_repo_file_rejects_symlinked_parent_dir() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("Cargo.toml"), "[package]").unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("sub")).unwrap();
        let p = root.path().join("sub").join("Cargo.toml");
        assert!(read_repo_file(root.path(), &p, MAX_REPO_FILE_BYTES).is_none());
    }

    #[test]
    fn secret_files() {
        for n in [
            ".env",
            ".env.local",
            "server.pem",
            "tls.KEY",
            "id_ed25519",
            "id_rsa.pub",
            "cert.pfx",
            "a.p12",
            "vault.kdbx",
            "credentials.json",
            "credentials-prod.json",
            "secrets.yaml",
            "terraform.tfstate",
            "terraform.tfstate.backup",
            ".npmrc",
            ".netrc",
        ] {
            assert!(is_secret_file(n), "{n}");
        }
        for n in [
            "main.go",
            "keys.rs",
            "env.ts",
            "credentials.go",
            "identity.py",
        ] {
            assert!(!is_secret_file(n), "{n}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(format_unix(0), "never");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14 22:13:20Z");
    }
}
