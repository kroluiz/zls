use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    jira::JiraConfig,
    store::{Entry, Task, today},
};

pub fn ensure_document(
    root: &Path,
    entry: &Entry,
    jira: Option<&JiraConfig>,
) -> Result<(PathBuf, String)> {
    let link = entry
        .task
        .doc
        .clone()
        .unwrap_or_else(|| format!("{}.md", entry.task.id));
    let path = document_path(root, &link)?;
    let parent = path.parent().unwrap_or(root);
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create documentation directory {}",
            parent.display()
        )
    })?;

    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file
            .write_all(template(entry, jira).as_bytes())
            .with_context(|| format!("failed to create documentation at {}", path.display()))?,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => reject_symlink(&path)?,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to create documentation at {}", path.display()));
        }
    }
    Ok((path, link))
}

pub fn read_document(root: &Path, task: &Task) -> Result<Option<String>> {
    let Some(link) = task.doc.as_deref() else {
        return Ok(None);
    };
    let path = document_path(root, link)?;
    reject_symlink(&path)?;
    fs::read_to_string(&path)
        .with_context(|| format!("failed to read documentation at {}", path.display()))
        .map(Some)
}

pub fn linked_document_path(root: &Path, task: &Task) -> Result<Option<PathBuf>> {
    task.doc
        .as_deref()
        .map(|link| document_path(root, link))
        .transpose()
}

pub fn edit_document(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .map_err(|_| anyhow!("neither VISUAL nor EDITOR is set"))?;
    if editor.trim().is_empty() {
        bail!("VISUAL or EDITOR must not be empty");
    }
    let status = Command::new("sh")
        .args(["-c", "exec $ZLS_EDITOR \"$1\"", "zls-editor"])
        .arg(path)
        .env("ZLS_EDITOR", editor)
        .status()
        .with_context(|| format!("failed to open editor for {}", path.display()))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

fn document_path(root: &Path, link: &str) -> Result<PathBuf> {
    let relative = Path::new(link);
    let mut components = relative.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("invalid task documentation link: {link}");
    }
    Ok(root.join(relative))
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!(
            "refusing to follow documentation symlink at {}",
            path.display()
        );
    }
    Ok(())
}

fn template(entry: &Entry, jira: Option<&JiraConfig>) -> String {
    let jira_line = entry.task.jira.as_ref().map_or_else(
        || "Jira: Not linked".to_owned(),
        |key| {
            jira.map_or_else(
                || format!("Jira: {key}"),
                |config| format!("Jira: {}/browse/{key}", config.site.trim_end_matches('/')),
            )
        },
    );
    format!(
        "# {}\n\nTask ID: {}\n{}\nCreated: {}\n\n## Context\n\n## Notes\n\n## Outcome\n",
        entry.task.text,
        entry.task.id,
        jira_line,
        today(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Task;

    fn entry() -> Entry {
        Entry {
            date: "2026-09-02".to_owned(),
            task: Task {
                id: "abc12345".to_owned(),
                text: "Document the feature".to_owned(),
                completed: false,
                done: None,
                jira: Some("MP-396".to_owned()),
                doc: None,
            },
        }
    }

    #[test]
    fn creates_a_stable_document_without_overwriting_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (path, link) = ensure_document(directory.path(), &entry(), None)?;
        assert_eq!(link, "abc12345.md");
        assert!(fs::read_to_string(&path)?.contains("Jira: MP-396"));

        fs::write(&path, "custom notes\n")?;
        ensure_document(directory.path(), &entry(), None)?;
        assert_eq!(fs::read_to_string(path)?, "custom notes\n");
        Ok(())
    }

    #[test]
    fn rejects_links_outside_the_document_root() {
        assert!(document_path(Path::new("/tmp/docs"), "../secret.md").is_err());
        assert!(document_path(Path::new("/tmp/docs"), "nested/task.md").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_document_symlinks() -> Result<()> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let outside = directory.path().join("outside.md");
        fs::write(&outside, "secret")?;
        let docs = directory.path().join("docs");
        fs::create_dir(&docs)?;
        symlink(&outside, docs.join("abc12345.md"))?;

        assert!(ensure_document(&docs, &entry(), None).is_err());
        Ok(())
    }
}
