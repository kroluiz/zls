use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use tempfile::NamedTempFile;

#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;

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

pub fn write_document(path: &Path, content: &str, append: bool) -> Result<()> {
    reject_symlink(path)?;
    if append {
        let mut options = fs::OpenOptions::new();
        options.append(true);
        #[cfg(target_os = "linux")]
        options.custom_flags(0o400000); // O_NOFOLLOW on Linux.
        return options
            .open(path)
            .and_then(|mut file| file.write_all(content.as_bytes()))
            .with_context(|| format!("failed to append documentation at {}", path.display()));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let permissions = fs::metadata(path)?.permissions();
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary.write_all(content.as_bytes())?;
    temporary.flush()?;
    temporary.as_file().set_permissions(permissions)?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .map(|_| ())
        .with_context(|| format!("failed to replace documentation at {}", path.display()))
}

pub fn update_jira_link(
    root: &Path,
    task: &Task,
    key: Option<&str>,
    jira: Option<&JiraConfig>,
) -> Result<bool> {
    let Some(path) = linked_document_path(root, task)? else {
        return Ok(false);
    };
    let mut content = read_document(root, task)?.expect("linked document was read");
    let pattern = Regex::new(&format!(
        r"\A#[^\r\n]*\r?\n\r?\nTask ID: {}\r?\nJira: ([^\r\n]*)\r?\nCreated: \d{{4}}-\d{{2}}-\d{{2}}\r?\n",
        regex::escape(&task.id)
    ))?;
    let range = pattern
        .captures(&content)
        .and_then(|captures| captures.get(1))
        .map(|value| value.range())
        .ok_or_else(|| {
            anyhow!(
                "documentation header for task {} was not recognized; left unchanged",
                task.id
            )
        })?;
    content.replace_range(range, &jira_value(key, jira));
    write_document(&path, &content, false)?;
    Ok(true)
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

fn jira_value(key: Option<&str>, jira: Option<&JiraConfig>) -> String {
    key.map_or_else(
        || "Not linked".to_owned(),
        |key| {
            jira.map_or_else(
                || key.to_owned(),
                |config| format!("{}/browse/{key}", config.site.trim_end_matches('/')),
            )
        },
    )
}

fn template(entry: &Entry, jira: Option<&JiraConfig>) -> String {
    format!(
        "# {}\n\nTask ID: {}\nJira: {}\nCreated: {}\n\n## Context\n\n## Notes\n\n## Outcome\n",
        entry.task.text,
        entry.task.id,
        jira_value(entry.task.jira.as_deref(), jira),
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
    fn sets_and_appends_document_content_exactly() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (path, _) = ensure_document(directory.path(), &entry(), None)?;

        write_document(&path, "replacement", false)?;
        write_document(&path, "\naddition\n", true)?;

        assert_eq!(fs::read_to_string(path)?, "replacement\naddition\n");
        Ok(())
    }

    #[test]
    fn updates_only_the_generated_jira_header() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let entry = entry();
        let (path, link) = ensure_document(directory.path(), &entry, None)?;
        write_document(&path, "\nCustom Jira: keep this\n", true)?;

        assert!(update_jira_link(
            directory.path(),
            &Task {
                doc: Some(link),
                ..entry.task
            },
            Some("MP-999"),
            None,
        )?);

        let content = fs::read_to_string(path)?;
        assert!(content.contains("Jira: MP-999\nCreated:"));
        assert!(content.ends_with("\nCustom Jira: keep this\n"));

        write_document(
            &directory.path().join("abc12345.md"),
            "custom document\n",
            false,
        )?;
        let task = Task {
            id: "abc12345".to_owned(),
            text: String::new(),
            completed: false,
            done: None,
            jira: None,
            doc: Some("abc12345.md".to_owned()),
        };
        assert!(update_jira_link(directory.path(), &task, None, None).is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("abc12345.md"))?,
            "custom document\n"
        );
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
        assert!(write_document(&docs.join("abc12345.md"), "overwrite", false).is_err());
        assert!(write_document(&docs.join("abc12345.md"), "append", true).is_err());
        Ok(())
    }
}
