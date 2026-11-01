//! AC-9: plugin pins. A run pins the directory of each plugin it declares, and the check before a
//! launch finds a plugin that changed since `arm` (ADR-0007).

use std::path::{Path, PathBuf};

use pf_workflow::package::Package;
use pf_workflow::pin::{
    Change, Hasher, PinError, check, check_with, pin_dir, pin_dir_with, pin_package,
};

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn plugin() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "hello");
    write(dir.path(), "skills/run/SKILL.md", "# run\n");
    write(dir.path(), "skills/run/refs/one.md", "one\n");
    write(dir.path(), "hooks.json", "{}\n");
    dir
}

const SHA256_OF_HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

#[test]
fn a_pin_of_a_directory_gives_a_stable_content() {
    let dir = plugin();
    let one = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    let two = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    assert_eq!(one, two);
    assert_eq!(one.name, "toolkit");
    assert_eq!(one.version, "1.30.0");
    assert_eq!(one.path, dir.path());
    assert_eq!(one.content.len(), 64);
    assert!(one.content.chars().all(|c| c.is_ascii_hexdigit()));
    // The file hashes are plain SHA-256, keyed by sorted relative path.
    assert_eq!(one.files["a.txt"], SHA256_OF_HELLO);
    let keys: Vec<&str> = one.files.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "a.txt",
            "hooks.json",
            "skills/run/SKILL.md",
            "skills/run/refs/one.md"
        ]
    );

    // The content does not depend on where the directory is.
    let other = plugin();
    let moved = pin_dir("toolkit", "1.30.0", None, other.path()).unwrap();
    assert_eq!(moved.content, one.content);
    // It depends on the paths and on the bytes.
    write(other.path(), "extra.txt", "x");
    assert_ne!(
        pin_dir("toolkit", "1.30.0", None, other.path())
            .unwrap()
            .content,
        one.content
    );
}

#[test]
fn a_directory_that_does_not_exist_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_dir("toolkit", "1.0.0", None, &dir.path().join("missing")).unwrap_err();
    assert!(matches!(err, PinError::Io { .. }), "{err:?}");
    assert!(err.to_string().contains("missing"), "{err}");
}

#[test]
fn an_unchanged_directory_passes_the_check() {
    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    check(&pin).unwrap();
}

#[test]
fn a_changed_file_is_a_mismatch_that_names_it() {
    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    write(dir.path(), "a.txt", "hello, changed");
    let m = check(&pin).unwrap_err();
    assert_eq!(m.name, "toolkit");
    assert_eq!(m.changes, [Change::Changed("a.txt".to_string())]);
    assert!(m.to_string().contains("a.txt"), "{m}");
    assert_eq!(m.park_reason(), "plugin toolkit changed since arm");
}

#[test]
fn an_added_file_and_a_removed_file_are_mismatches_that_name_them() {
    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    write(dir.path(), "skills/new/SKILL.md", "new");
    let m = check(&pin).unwrap_err();
    assert_eq!(
        m.changes,
        [Change::Added("skills/new/SKILL.md".to_string())]
    );

    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    std::fs::remove_file(dir.path().join("hooks.json")).unwrap();
    let m = check(&pin).unwrap_err();
    assert_eq!(m.changes, [Change::Removed("hooks.json".to_string())]);

    // A rename is a removal and an addition. All three kinds in one mismatch.
    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    std::fs::rename(
        dir.path().join("hooks.json"),
        dir.path().join("hooks2.json"),
    )
    .unwrap();
    write(dir.path(), "a.txt", "changed");
    let m = check(&pin).unwrap_err();
    assert_eq!(
        m.changes,
        [
            Change::Changed("a.txt".to_string()),
            Change::Added("hooks2.json".to_string()),
            Change::Removed("hooks.json".to_string()),
        ]
    );
    let shown = m.to_string();
    assert!(
        shown.contains("a.txt") && shown.contains("hooks2.json") && shown.contains("hooks.json"),
        "{shown}"
    );
}

#[test]
fn a_directory_that_went_away_is_a_mismatch() {
    let dir = plugin();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists());
    let m = check(&pin).unwrap_err();
    assert_eq!(m.changes.len(), 4);
    assert!(m.changes.iter().all(|c| matches!(c, Change::Removed(_))));
}

#[test]
fn a_symlink_is_pinned_by_its_target_and_never_followed() {
    let dir = plugin();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.txt", "one");
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("link")).unwrap();
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    assert!(pin.files.contains_key("link"));
    // The file behind the link changes: the plugin does not.
    write(outside.path(), "secret.txt", "two");
    check(&pin).unwrap();
    // The link is pointed elsewhere: that is a change.
    std::fs::remove_file(dir.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("other"), dir.path().join("link")).unwrap();
    assert_eq!(
        check(&pin).unwrap_err().changes,
        [Change::Changed("link".to_string())]
    );
}

#[test]
fn many_files_hash_one_call_each_so_no_hash_is_cut() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..150 {
        write(
            dir.path(),
            &format!("d{}/f{i}.txt", i % 7),
            &format!("content {i}\n"),
        );
    }
    let pin = pin_dir("big", "1.0.0", None, dir.path()).unwrap();
    assert_eq!(pin.files.len(), 150);
    assert!(pin.files.values().all(|h| h.len() == 64));
    check(&pin).unwrap();
}

#[test]
fn a_pin_below_the_minimum_version_is_refused_and_the_error_names_both_versions() {
    let dir = plugin();
    let err = pin_dir("toolkit", "1.20.0", Some("1.26.0"), dir.path()).unwrap_err();
    assert!(matches!(err, PinError::BelowMinimum { .. }), "{err:?}");
    let text = err.to_string();
    assert!(
        text.contains("1.20.0") && text.contains("1.26.0") && text.contains("toolkit"),
        "{text}"
    );

    // The comparison is numeric, part by part.
    assert!(pin_dir("toolkit", "1.9.0", Some("1.26.0"), dir.path()).is_err());
    assert!(pin_dir("toolkit", "1.26", Some("1.26.1"), dir.path()).is_err());
    assert!(pin_dir("toolkit", "0.99.9", Some("1.0.0"), dir.path()).is_err());
    // At the minimum and above it pass.
    assert!(pin_dir("toolkit", "1.26.0", Some("1.26.0"), dir.path()).is_ok());
    assert!(pin_dir("toolkit", "1.32.2", Some("1.26.0"), dir.path()).is_ok());
    assert!(pin_dir("toolkit", "10.0.0", Some("9.9.9"), dir.path()).is_ok());
    assert!(pin_dir("toolkit", "1.26", Some("1.26.0"), dir.path()).is_ok());
    assert!(pin_dir("toolkit", "2", Some("1.26.0"), dir.path()).is_ok());
    // A version that is not whole numbers is refused.
    let err = pin_dir("toolkit", "1.x.0", Some("1.26.0"), dir.path()).unwrap_err();
    assert!(matches!(err, PinError::BadVersion { .. }), "{err:?}");
    assert!(pin_dir("toolkit", "", None, dir.path()).is_err());
}

#[test]
fn pinning_refuses_when_sha256sum_is_absent() {
    let dir = plugin();
    let hasher = Hasher::new("definitely-not-sha256sum-xyz");
    let err = pin_dir_with(&hasher, "toolkit", "1.30.0", None, dir.path()).unwrap_err();
    assert!(matches!(err, PinError::NoSha256sum { .. }), "{err:?}");
    assert!(err.to_string().contains("sha256sum"), "{err}");
    // So does the check.
    let pin = pin_dir("toolkit", "1.30.0", None, dir.path()).unwrap();
    assert!(check_with(&hasher, &pin).is_err());
    // An empty directory needs no per-file call, but the manifest hash still needs the tool.
    let empty = tempfile::tempdir().unwrap();
    assert!(pin_dir_with(&hasher, "toolkit", "1.30.0", None, empty.path()).is_err());
}

fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workflows/ossify-spine")
}

#[test]
fn the_same_function_pins_the_workflow_file_its_briefs_and_its_rule_files() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&package_dir(), dir.path());
    let package = Package::load(dir.path()).unwrap();
    let pin = pin_package(&package).unwrap();
    assert_eq!(pin.name, "ossify-spine");
    let keys: Vec<String> = pin.files.keys().cloned().collect();
    let expected: Vec<String> = package
        .pinned_files()
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert_eq!(keys, expected);
    assert!(keys.contains(&"workflow.yaml".to_string()));
    assert!(keys.contains(&"briefs/lane.md".to_string()));
    assert!(keys.contains(&"rules/close.yaml".to_string()));
    check(&pin).unwrap();

    // A file the package does not name is not part of the pin.
    write(dir.path(), "NOTES.txt", "scratch");
    check(&pin).unwrap();

    // Any pinned file that changes is named.
    write(dir.path(), "briefs/lane.md", "changed brief");
    let m = check(&pin).unwrap_err();
    assert_eq!(m.changes, [Change::Changed("briefs/lane.md".to_string())]);
    write(
        dir.path(),
        "rules/close.yaml",
        "version: 1\nrole: close\nallow: []\n",
    );
    let m = check(&pin).unwrap_err();
    assert_eq!(m.changes.len(), 2);
    assert!(
        m.changes
            .contains(&Change::Changed("rules/close.yaml".to_string()))
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
