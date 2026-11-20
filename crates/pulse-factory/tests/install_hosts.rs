//! AC-14 of `r2.s2.w3` (G8, audit 9): `scripts/install-hosts.sh <share> <built pf-host> <json file>`.
//! It copies the host binary to `<share>/hosts/<sha256>/pf-host` (0755; an existing hash directory is
//! left as it is), points `~/.local/bin/pf-host` at it with a symlink, and removes every other
//! `<share>/hosts/<sha256>/` directory except those whose `pf-host` path is in the JSON's
//! `host_binaries`. It removes no path in `cli_copies`. It needs `jq`. The tests run it on a temp share
//! directory with a stub JSON; nothing here builds or installs anything for real.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install-hosts.sh")
}

fn sha(bytes: &[u8]) -> String {
    pf_host::sha256::sha256_hex(bytes)
}

struct Rig {
    dir: tempfile::TempDir,
    share: PathBuf,
    home: PathBuf,
    built: PathBuf,
}

impl Rig {
    fn new(binary: &[u8]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let built = dir.path().join("pf-host-built");
        std::fs::write(&built, binary).unwrap();
        std::fs::set_permissions(&built, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rig {
            dir,
            share,
            home,
            built,
        }
    }

    fn hash_dir(&self, hash: &str) -> PathBuf {
        self.share.join("hosts").join(hash)
    }

    /// A hash directory with a `pf-host` of the given bytes.
    fn old_host(&self, hash: &str, bytes: &[u8]) -> PathBuf {
        let d = self.hash_dir(hash);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("pf-host");
        std::fs::write(&f, bytes).unwrap();
        f
    }

    fn json(&self, host_binaries: &[&Path], cli_copies: &[&Path]) -> PathBuf {
        let path = self.dir.path().join("check.json");
        let list = |v: &[&Path]| {
            v.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
        };
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "host_binaries": list(host_binaries),
                "cli_copies": list(cli_copies),
                "can_start": true
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    fn run_with_path(&self, json: &Path, path: &str) -> Output {
        Command::new("bash")
            .arg(script())
            .arg(&self.share)
            .arg(&self.built)
            .arg(json)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .output()
            .unwrap()
    }

    fn run(&self, json: &Path) -> Output {
        self.run_with_path(json, &std::env::var("PATH").unwrap())
    }
}

fn hex_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn it_installs_the_binary_under_its_hash_with_mode_0755_and_points_the_symlink_at_it() {
    let rig = Rig::new(b"host binary one");
    let json = rig.json(&[], &[]);
    let out = rig.run(&json);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hash = sha(b"host binary one");
    let installed = rig.hash_dir(&hash).join("pf-host");
    assert_eq!(std::fs::read(&installed).unwrap(), b"host binary one");
    assert_eq!(
        std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let link = rig.home.join(".local/bin/pf-host");
    assert_eq!(std::fs::read_link(&link).unwrap(), installed);
    // The controller resolves the link to the content-hash path.
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&installed).unwrap()
    );
    // Nothing else is left in the hosts directory (no temp directory).
    assert_eq!(hex_names(&rig.share.join("hosts")), vec![hash]);
}

#[test]
fn an_existing_hash_directory_is_left_as_it_is() {
    let rig = Rig::new(b"host binary one");
    let hash = sha(b"host binary one");
    // A file a live host runs from: its bytes are not rewritten, even when they differ from the
    // build (they cannot, by the name, but the script must not rely on it).
    let existing = rig.old_host(&hash, b"already here");
    let before = std::fs::metadata(&existing).unwrap().modified().unwrap();
    let out = rig.run(&rig.json(&[], &[]));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"already here");
    assert_eq!(
        std::fs::metadata(&existing).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_link(rig.home.join(".local/bin/pf-host")).unwrap(),
        existing
    );
}

#[test]
fn it_keeps_a_directory_a_live_record_names_and_removes_one_it_does_not() {
    let rig = Rig::new(b"host binary two");
    let live = "a".repeat(64);
    let stale = "b".repeat(64);
    let live_bin = rig.old_host(&live, b"live host");
    let stale_bin = rig.old_host(&stale, b"stale host");
    let json = rig.json(&[&live_bin], &[]);
    let out = rig.run(&json);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        live_bin.exists(),
        "a host binary a live record names was removed"
    );
    assert!(!rig.hash_dir(&stale).exists(), "a stale host binary stays");
    assert!(stale_bin.parent().unwrap().parent().unwrap().exists());
    let hash = sha(b"host binary two");
    let mut expected = vec![live, hash];
    expected.sort();
    assert_eq!(hex_names(&rig.share.join("hosts")), expected);
}

#[test]
fn it_removes_only_hash_directories_and_touches_no_cli_copy() {
    let rig = Rig::new(b"host binary three");
    // A directory that is not a SHA-256 stays, and so does a file in `hosts/`.
    std::fs::create_dir_all(rig.share.join("hosts/not-a-hash")).unwrap();
    std::fs::write(rig.share.join("hosts/README"), "notes").unwrap();
    // A CLI copy that a live record names, inside the share but outside `hosts/`, and one that sits in
    // a hash directory's neighbour: neither is touched.
    let copy = rig.share.join("claude/2.1.0/claude");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, "cli copy").unwrap();
    // A stale host binary directory that also holds a copy the JSON names: the copy's path is under
    // a directory the script would prune, so the script must keep the copy.
    let stale = "c".repeat(64);
    rig.old_host(&stale, b"stale host");
    let inside = rig.hash_dir(&stale).join("claude-copy");
    std::fs::write(&inside, "a copy kept by name").unwrap();
    let json = rig.json(&[], &[&copy, &inside]);
    let out = rig.run(&json);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&copy).unwrap(), b"cli copy");
    assert_eq!(
        std::fs::read(&inside).unwrap(),
        b"a copy kept by name",
        "a cli copy was removed"
    );
    assert!(rig.share.join("hosts/not-a-hash").is_dir());
    assert!(rig.share.join("hosts/README").is_file());
    // The stale host binary itself is gone; only the named copy remains in its directory.
    assert!(!rig.hash_dir(&stale).join("pf-host").exists());
}

#[test]
fn it_needs_jq_and_changes_nothing_without_it() {
    let rig = Rig::new(b"host binary four");
    let json = rig.json(&[], &[]);
    // A PATH with the coreutils the script needs and no `jq`.
    let bare = rig.dir.path().join("bare-bin");
    std::fs::create_dir_all(&bare).unwrap();
    for tool in [
        "bash",
        "sh",
        "sha256sum",
        "cut",
        "mkdir",
        "mktemp",
        "mv",
        "ln",
        "rm",
        "install",
        "cat",
        "dirname",
        "basename",
        "readlink",
        "cp",
        "chmod",
        "pwd",
        "tr",
        "sort",
        "grep",
        "sed",
        "ls",
        "head",
        "env",
    ] {
        if let Ok(p) = which(tool) {
            let _ = std::os::unix::fs::symlink(p, bare.join(tool));
        }
    }
    let out = rig.run_with_path(&json, bare.to_str().unwrap());
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("jq"), "{err}");
    assert!(
        !rig.share.join("hosts").exists(),
        "the script installed before it checked for jq"
    );
    assert!(!rig.home.join(".local/bin/pf-host").exists());
}

fn which(tool: &str) -> Result<PathBuf, ()> {
    for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let p = Path::new(dir).join(tool);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(())
}

#[test]
fn it_refuses_a_missing_binary_or_report_and_a_wrong_argument_count() {
    let rig = Rig::new(b"host binary five");
    let json = rig.json(&[], &[]);
    let out = Command::new("bash")
        .arg(script())
        .arg(&rig.share)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("bash")
        .arg(script())
        .arg(&rig.share)
        .arg(rig.dir.path().join("no-such-binary"))
        .arg(&json)
        .env("HOME", &rig.home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = Command::new("bash")
        .arg(script())
        .arg(&rig.share)
        .arg(&rig.built)
        .arg(rig.dir.path().join("no-such.json"))
        .env("HOME", &rig.home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!rig.share.join("hosts").exists());
}
