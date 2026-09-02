use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub tasks: TasksConfig,
    pub docs: DocsConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TasksConfig {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DocsConfig {
    pub path: PathBuf,
}

impl AppConfig {
    pub fn load_or_create(default_task_path: PathBuf) -> Result<Self> {
        Self::load_or_create_at(&config_path()?, default_task_path, default_docs_path()?)
    }

    fn load_or_create_at(
        path: &Path,
        default_task_path: PathBuf,
        default_docs_path: PathBuf,
    ) -> Result<Self> {
        let contents = path
            .exists()
            .then(|| fs::read_to_string(path))
            .transpose()?;
        let existing = contents.as_deref().map_or_else(
            || Ok(ConfigFile::default()),
            |contents| {
                toml::from_str::<ConfigFile>(contents)
                    .with_context(|| format!("invalid TOML in {}", path.display()))
            },
        )?;
        let needs_tasks = existing.tasks.is_none();
        let needs_docs = existing.docs.is_none();
        let tasks = existing.tasks.unwrap_or(TasksConfig {
            path: default_task_path,
        });
        let docs = existing.docs.unwrap_or(DocsConfig {
            path: default_docs_path,
        });
        if tasks.path.as_os_str().is_empty() {
            bail!("tasks.path must not be empty in {}", path.display());
        }
        if docs.path.as_os_str().is_empty() {
            bail!("docs.path must not be empty in {}", path.display());
        }

        if needs_tasks || needs_docs {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            let mut updated = contents.unwrap_or_default();
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            if !updated.trim().is_empty() {
                updated.push('\n');
            }
            if needs_tasks {
                updated.push_str("[tasks]\n");
                updated.push_str(&toml::to_string(&tasks)?);
                updated.push('\n');
            }
            if needs_docs {
                updated.push_str("[docs]\n");
                updated.push_str(&toml::to_string(&docs)?);
            }
            let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
                format!("failed to create temporary file in {}", parent.display())
            })?;
            temporary.write_all(updated.as_bytes())?;
            temporary.flush()?;
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .with_context(|| format!("failed to replace {}", path.display()))?;
        }

        Ok(Self { tasks, docs })
    }
}

#[derive(Default, Deserialize)]
struct ConfigFile {
    tasks: Option<TasksConfig>,
    docs: Option<DocsConfig>,
}

pub fn config_path() -> Result<PathBuf> {
    xdg_path("XDG_CONFIG_HOME", ".config").map(|path| path.join("zls/config.toml"))
}

pub fn default_data_path() -> Result<PathBuf> {
    xdg_path("XDG_DATA_HOME", ".local/share").map(|path| path.join("zls/todo.md"))
}

pub fn default_docs_path() -> Result<PathBuf> {
    xdg_path("XDG_DATA_HOME", ".local/share").map(|path| path.join("zls/docs"))
}

fn xdg_path(variable: &str, home_suffix: &str) -> Result<PathBuf> {
    if let Some(path) = env::var_os(variable).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(home_suffix))
        .ok_or_else(|| anyhow!("neither {variable} nor HOME is set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_task_path_without_replacing_other_sections() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# preserve this comment\n[jira]\nsite = \"https://example.atlassian.net\"\n",
        )?;
        let tasks_path = directory.path().join("todo.md");

        let docs_path = directory.path().join("docs");
        let config = AppConfig::load_or_create_at(&path, tasks_path.clone(), docs_path.clone())?;
        let saved = fs::read_to_string(path)?;

        assert_eq!(config.tasks.path, tasks_path);
        assert_eq!(config.docs.path, docs_path);
        assert!(saved.contains("[jira]"));
        assert!(saved.contains("# preserve this comment"));
        assert!(saved.contains("[tasks]"));
        assert!(saved.contains("[docs]"));
        Ok(())
    }

    #[test]
    fn loads_an_existing_task_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.toml");
        let configured = directory.path().join("configured.md");
        fs::write(
            &path,
            format!("[tasks]\npath = {:?}\n", configured.to_string_lossy()),
        )?;

        let config = AppConfig::load_or_create_at(
            &path,
            directory.path().join("default.md"),
            directory.path().join("docs"),
        )?;

        assert_eq!(config.tasks.path, configured);
        Ok(())
    }
}
