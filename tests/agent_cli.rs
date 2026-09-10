use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use chrono::{DateTime, Utc};
use serde_json::Value;

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zls"));
    command
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .arg("--file")
        .arg(root.join("tasks.md"));
    command
}

fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn guide_needs_no_local_state_and_matches_installed_version() {
    let directory = tempfile::tempdir().unwrap();
    // A regular file blocks all config/data paths, so even attempted initialization fails.
    let blocked = directory.path().join("blocked");
    fs::write(&blocked, "unchanged").unwrap();
    let guide = json(cli(&blocked).arg("agent-guide").output().unwrap());
    let version = cli(&blocked).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        format!("zls {}", guide["version"].as_str().unwrap())
    );
    assert_eq!(guide["schema_version"], 1);
    assert_eq!(fs::read_to_string(blocked).unwrap(), "unchanged");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn document_writes_acknowledge_persisted_content_and_context() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    // Explicit config avoids the legacy task-store discovery path.
    fs::create_dir_all(root.join("config/zls")).unwrap();
    fs::write(
        root.join("config/zls/config.toml"),
        format!(
            "[tasks]\npath = {:?}\n[docs]\npath = {:?}\n",
            root.join("tasks.md"),
            root.join("docs")
        ),
    )
    .unwrap();
    let task = json(cli(root).args(["add", "Investigate"]).output().unwrap());
    let id = task["id"].as_str().unwrap();
    let context = json(cli(root).args(["context", id]).output().unwrap());
    assert!(context["task"].get("jira").is_none());
    assert_eq!(context["issue"], Value::Null);
    assert_eq!(context["documentation"], Value::Null);
    assert_eq!(context["document_updated_at"], Value::Null);
    let failed = cli(root).args(["context", id, "--jira"]).output().unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("task is not linked to Jira"));
    assert!(failed.stdout.is_empty());

    json(
        cli(root)
            .args(["jira", "link", id, "MP-467"])
            .output()
            .unwrap(),
    );
    let input = root.join("input.md");
    let mut expected = String::new();
    for (operation, content) in [
        ("set", "Custom Jira: WRONG-1\n"),
        ("append", "## Discovery\nEvidence\n"),
    ] {
        fs::write(&input, content).unwrap();
        expected.push_str(content);
        let ack = json(
            cli(root)
                .args(["docs", operation, id, "--file"])
                .arg(&input)
                .output()
                .unwrap(),
        );
        let path = root.join("docs").join(format!("{id}.md"));
        assert_eq!(ack["task_id"], id);
        assert_eq!(ack["path"], path.to_str().unwrap());
        assert_eq!(ack["operation"], operation);
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        let modified = DateTime::<Utc>::from(fs::metadata(&path).unwrap().modified().unwrap());
        let reported =
            DateTime::parse_from_rfc3339(ack["document_updated_at"].as_str().unwrap()).unwrap();
        assert_eq!(reported, modified);
        assert_eq!(reported.offset().local_minus_utc(), 0);
        let context = json(cli(root).args(["context", id]).output().unwrap());
        assert_eq!(context["task"]["jira"], "MP-467");
        assert_eq!(context["issue"], Value::Null);
        assert!(context.get("jira").is_none());
        assert_eq!(context["documentation"], expected);
        assert_eq!(context["document_updated_at"], ack["document_updated_at"]);
    }
    fs::write(&input, " ").unwrap();
    let failed = cli(root)
        .args(["docs", "set", id, "--file"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(root.join("docs").join(format!("{id}.md"))).unwrap(),
        expected
    );
}

#[test]
fn skill_installation_is_repeatable() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory
        .path()
        .join(".config/opencode/skills/zls/SKILL.md");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/zls/SKILL.md");
    for _ in 0..2 {
        let output = Command::new("make")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("install-skill")
            .arg(format!("HOME={}", directory.path().display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
        fs::write(&destination, "old installed copy").unwrap();
    }
}
