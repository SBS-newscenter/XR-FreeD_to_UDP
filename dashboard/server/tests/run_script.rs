//! `dashboard/setup/run.sh`, the service's one entry point, tried in a copy
//! of the tree's shape so that no real binary is started.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A repository root of the test's own, holding only a copy of `run.sh`.
fn tree(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("xrfd-run-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dashboard/setup")).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../setup/run.sh");
    fs::copy(script, root.join("dashboard/setup/run.sh")).unwrap();
    root
}

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new("bash")
        .arg(root.join("dashboard/setup/run.sh"))
        .args(arguments)
        .current_dir("/")
        .output()
        .unwrap()
}

#[test]
fn run_script_says_what_is_missing_when_nothing_is_built() {
    // Red when a start with no binary fails without naming the build step.
    let root = tree("unbuilt");
    let output = run(&root, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("install.sh"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn run_script_starts_the_binary_from_the_repository_root_with_its_arguments() {
    // Red when the binary is started from another directory, where it would
    // find no server.toml, or the arguments are dropped on the way.
    let root = tree("built");
    let release = root.join("dashboard/server/target/release");
    fs::create_dir_all(&release).unwrap();
    let binary = release.join("xrfd-dashboard");
    fs::write(&binary, "#!/bin/sh\npwd -P\necho \"$*\"\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run(&root, &["--port", "9999"]);
    let printed = String::from_utf8_lossy(&output.stdout);
    let mut lines = printed.lines();
    let started_in = PathBuf::from(lines.next().unwrap());
    assert_eq!(started_in, fs::canonicalize(&root).unwrap());
    assert_eq!(lines.next(), Some("--port 9999"));
    let _ = fs::remove_dir_all(&root);
}
