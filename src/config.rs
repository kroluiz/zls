use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub tasks: TasksConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TasksConfig {
    pub path: PathBuf,
}

impl AppConfig {
    pub fn load_or_create(default_task_path: PathBuf) -> Result<Self> {
        Self::load_or_create_at(&config_path()?, default_task_path)
    }

    fn load_or_create_at(path: &Path, default_task_path: PathBuf) -> Result<Self> {
        let mut root = if path.exists() {
            toml::from_str::<toml::Value>(&fs::read_to_string(path)?)
                .with_context(|| format!("invalid TOML in {}", path.display()))?
        } else {
            toml::Value::Table(Default::default())
        };
        let existing = root.clone().try_into::<ConfigFile>()?.tasks;
        let needs_save = existing.is_none();
        let tasks = existing.unwrap_or(TasksConfig {
            path: default_task_path,
        });
        if tasks.path.as_os_str().is_empty() {
            bail!("tasks.path must not be empty in {}", path.display());
        }

        if needs_save {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            let table = root
                .as_table_mut()
                .ok_or_else(|| anyhow!("{} must contain a TOML table", path.display()))?;
            table.insert("tasks".to_owned(), toml::Value::try_from(&tasks)?);
            fs::write(path, toml::to_string_pretty(&root)?)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        Ok(Self { tasks })
    }
}

#[derive(Deserialize)]
struct ConfigFile {
    tasks: Option<TasksConfig>,
}

pub fn config_path() -> Result<PathBuf> {
    xdg_path("XDG_CONFIG_HOME", ".config").map(|path| path.join("zls/config.toml"))
}

pub fn default_data_path() -> Result<PathBuf> {
    xdg_path("XDG_DATA_HOME", ".local/share").map(|path| path.join("zls/todo.md"))
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
        fs::write(&path, "[jira]\nsite = \"https://example.atlassian.net\"\n")?;
        let tasks_path = directory.path().join("todo.md");

        let config = AppConfig::load_or_create_at(&path, tasks_path.clone())?;
        let saved = fs::read_to_string(path)?;

        assert_eq!(config.tasks.path, tasks_path);
        assert!(saved.contains("[jira]"));
        assert!(saved.contains("[tasks]"));
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

        let config = AppConfig::load_or_create_at(&path, directory.path().join("default.md"))?;

        assert_eq!(config.tasks.path, configured);
        Ok(())
    }
}
