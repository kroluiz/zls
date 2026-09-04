use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, Table, Value, value};

pub const DEFAULT_ACCENT: &str = "#00FFFF";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub tasks: TasksConfig,
    pub docs: DocsConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TasksConfig {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DocsConfig {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UiConfig {
    pub accent: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            accent: DEFAULT_ACCENT.to_owned(),
        }
    }
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
        let needs_ui = existing
            .ui
            .as_ref()
            .and_then(|ui| ui.accent.as_ref())
            .is_none();
        let tasks = existing.tasks.unwrap_or(TasksConfig {
            path: default_task_path,
        });
        let docs = existing.docs.unwrap_or(DocsConfig {
            path: default_docs_path,
        });
        let ui = UiConfig {
            accent: existing
                .ui
                .and_then(|ui| ui.accent)
                .unwrap_or_else(|| DEFAULT_ACCENT.to_owned()),
        };
        if tasks.path.as_os_str().is_empty() {
            bail!("tasks.path must not be empty in {}", path.display());
        }
        if docs.path.as_os_str().is_empty() {
            bail!("docs.path must not be empty in {}", path.display());
        }
        parse_hex_color(&ui.accent).with_context(|| "invalid ui.accent")?;

        if needs_tasks || needs_docs || needs_ui {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            let mut document = contents
                .as_deref()
                .unwrap_or_default()
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid TOML in {}", path.display()))?;
            if needs_tasks {
                ensure_table(&mut document, "tasks");
                document["tasks"]["path"] = value(tasks.path.to_string_lossy().as_ref());
            }
            if needs_docs {
                ensure_table(&mut document, "docs");
                document["docs"]["path"] = value(docs.path.to_string_lossy().as_ref());
            }
            if needs_ui {
                ensure_table(&mut document, "ui");
                document["ui"]["accent"] = value(&ui.accent);
            }
            write_atomic(path, document.to_string().as_bytes())?;
        }

        Ok(Self { tasks, docs, ui })
    }

    pub fn set_accent(&mut self, accent: &str) -> Result<()> {
        self.set_accent_at(&config_path()?, accent)
    }

    fn set_accent_at(&mut self, path: &Path, accent: &str) -> Result<()> {
        parse_hex_color(accent)?;
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let mut document = contents
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        let mut accent_value = Value::from(accent);
        if let Some(existing) = document["ui"]["accent"].as_value() {
            *accent_value.decor_mut() = existing.decor().clone();
        }
        document["ui"]["accent"] = Item::Value(accent_value);
        write_atomic(path, document.to_string().as_bytes())?;
        self.ui.accent = accent.to_owned();
        Ok(())
    }
}

#[derive(Default, Deserialize)]
struct ConfigFile {
    tasks: Option<TasksConfig>,
    docs: Option<DocsConfig>,
    ui: Option<RawUiConfig>,
}

#[derive(Deserialize)]
struct RawUiConfig {
    accent: Option<String>,
}

fn ensure_table(document: &mut DocumentMut, name: &str) {
    if !document.as_table().contains_key(name) {
        document[name] = Item::Table(Table::new());
    }
}

pub fn parse_hex_color(value: &str) -> Result<(u8, u8, u8)> {
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("accent must use #RRGGBB format, got {value:?}");
    }
    Ok((
        u8::from_str_radix(&value[1..3], 16)?,
        u8::from_str_radix(&value[3..5], 16)?,
        u8::from_str_radix(&value[5..7], 16)?,
    ))
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let destination = if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) {
        fs::canonicalize(path)
            .with_context(|| format!("failed to resolve config symlink {}", path.display()))?
    } else {
        path.to_owned()
    };
    let permissions = fs::metadata(&destination)
        .ok()
        .map(|metadata| metadata.permissions());
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .map(|_| ())
        .with_context(|| format!("failed to replace {}", destination.display()))
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
        assert_eq!(config.ui.accent, DEFAULT_ACCENT);
        assert!(saved.contains("[jira]"));
        assert!(saved.contains("# preserve this comment"));
        assert!(saved.contains("[tasks]"));
        assert!(saved.contains("[docs]"));
        assert!(saved.contains("[ui]"));
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

    #[test]
    fn adds_accent_to_an_existing_ui_table() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[tasks]\npath = \"todo.md\"\n[docs]\npath = \"docs\"\n[ui]\n# future settings live here\n",
        )?;

        let config = AppConfig::load_or_create_at(
            &path,
            directory.path().join("default.md"),
            directory.path().join("default-docs"),
        )?;
        let saved = fs::read_to_string(path)?;

        assert_eq!(config.ui.accent, DEFAULT_ACCENT);
        assert!(saved.contains("# future settings live here"));
        assert!(saved.contains("accent = \"#00FFFF\""));
        Ok(())
    }

    #[test]
    fn validates_and_updates_accent_without_losing_comments() -> Result<()> {
        assert_eq!(parse_hex_color("#00FFFF")?, (0, 255, 255));
        assert!(parse_hex_color("00FFFF").is_err());
        assert!(parse_hex_color("#0FF").is_err());
        assert!(parse_hex_color("#GGFFFF").is_err());

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep me\n[tasks]\npath = \"todo.md\"\n[docs]\npath = \"docs\"\n[ui]\naccent = \"#00FFFF\" # accent comment\n",
        )?;
        let mut config = AppConfig::load_or_create_at(
            &path,
            directory.path().join("default.md"),
            directory.path().join("default-docs"),
        )?;

        config.set_accent_at(&path, "#89B4FA")?;
        let saved = fs::read_to_string(path)?;
        assert_eq!(config.ui.accent, "#89B4FA");
        assert!(saved.contains("# keep me"));
        assert!(saved.contains("# accent comment"));
        assert!(saved.contains("accent = \"#89B4FA\""));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn accent_updates_follow_symlinks_and_preserve_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir()?;
        let target = directory.path().join("managed.toml");
        let link = directory.path().join("config.toml");
        fs::write(
            &target,
            "[tasks]\npath = \"todo.md\"\n[docs]\npath = \"docs\"\n[ui]\naccent = \"#00FFFF\"\n",
        )?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))?;
        symlink(&target, &link)?;
        let mut config = AppConfig::load_or_create_at(
            &link,
            directory.path().join("default.md"),
            directory.path().join("default-docs"),
        )?;

        config.set_accent_at(&link, "#CBA6F7")?;

        assert!(fs::symlink_metadata(&link)?.is_symlink());
        assert!(fs::read_to_string(target)?.contains("#CBA6F7"));
        assert_eq!(fs::metadata(link)?.permissions().mode() & 0o777, 0o640);
        Ok(())
    }
}
