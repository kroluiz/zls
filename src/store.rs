use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate, SecondsFormat};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const BACKLOG: &str = "Backlog";

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: String,
    pub text: String,
    pub completed: bool,
    pub done: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Clone, Debug)]
struct Section {
    date: String,
    tasks: Vec<Task>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub date: String,
    #[serde(flatten)]
    pub task: Task,
}

pub struct Store {
    path: PathBuf,
    sections: Vec<Section>,
    legacy: bool,
}

pub fn today() -> String {
    Local::now().date_naive().to_string()
}

pub fn tomorrow() -> String {
    Local::now()
        .date_naive()
        .succ_opt()
        .expect("the next calendar day exists")
        .to_string()
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn new_task(text: &str) -> Task {
    Task {
        id: Uuid::new_v4().simple().to_string()[..8].to_owned(),
        text: clean_text(text),
        completed: false,
        done: None,
        jira: None,
        doc: None,
    }
}

fn heading_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^## (.+)$").expect("valid heading regex"))
}

fn task_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^- \[([ xX])\] (.*?)(?: <!-- (.*?) -->)?$").expect("valid task regex")
    })
}

impl Store {
    pub fn load(path: PathBuf) -> Result<Self> {
        let mut store = Self {
            path,
            sections: Vec::new(),
            legacy: false,
        };
        store.reload()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reload(&mut self) -> Result<()> {
        self.sections.clear();
        self.legacy = false;
        if !self.path.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        if content.trim().is_empty() {
            return Ok(());
        }
        if content.lines().any(|line| heading_regex().is_match(line)) {
            self.load_markdown(&content);
        } else {
            self.load_legacy(&content);
        }
        Ok(())
    }

    fn load_markdown(&mut self, content: &str) {
        let mut current = None;
        for line in content.lines() {
            if let Some(captures) = heading_regex().captures(line) {
                self.sections.push(Section {
                    date: captures[1].to_owned(),
                    tasks: Vec::new(),
                });
                current = Some(self.sections.len() - 1);
                continue;
            }
            let (Some(index), Some(captures)) = (current, task_regex().captures(line)) else {
                continue;
            };
            let metadata = captures
                .get(3)
                .map(|value| value.as_str())
                .unwrap_or_default();
            let value = |name: &str| {
                metadata.split_whitespace().find_map(|field| {
                    field
                        .strip_prefix(name)
                        .and_then(|value| value.strip_prefix(':'))
                })
            };
            self.sections[index].tasks.push(Task {
                id: value("id").map_or_else(|| new_task(&captures[2]).id, str::to_owned),
                text: captures[2].to_owned(),
                completed: captures[1].eq_ignore_ascii_case("x"),
                done: value("done").map(str::to_owned),
                jira: value("jira").map(str::to_owned),
                doc: value("doc").map(str::to_owned),
            });
        }
    }

    fn load_legacy(&mut self, content: &str) {
        self.legacy = true;
        let mut blocks = vec![Vec::new()];
        for line in content.lines() {
            let line = line.trim();
            if line == "---" {
                blocks.push(Vec::new());
            } else if !line.is_empty() {
                blocks.last_mut().expect("a legacy block exists").push(line);
            }
        }

        for (block_index, lines) in blocks.into_iter().enumerate() {
            if lines.is_empty() {
                continue;
            }
            let date = if block_index == 0 {
                today()
            } else {
                format!("Legacy {block_index}")
            };
            let tasks = lines
                .into_iter()
                .enumerate()
                .map(|(line_index, line)| {
                    let mut task = new_task(line);
                    let identity = format!("{block_index}\0{line_index}\0{line}");
                    task.id = format!("{:x}", Sha256::digest(identity.as_bytes()))[..8].to_owned();
                    task
                })
                .collect();
            self.sections.push(Section { date, tasks });
        }
    }

    pub fn entries_today(&self) -> Vec<Entry> {
        let current = today();
        self.sections
            .iter()
            .find(|section| section.date == current)
            .map(|section| {
                section
                    .tasks
                    .iter()
                    .cloned()
                    .map(|task| Entry {
                        date: current.clone(),
                        task,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn entries_all(&self) -> Vec<Entry> {
        self.sections
            .iter()
            .flat_map(|section| {
                section.tasks.iter().cloned().map(|task| Entry {
                    date: section.date.clone(),
                    task,
                })
            })
            .collect()
    }

    pub fn entries_backlog(&self) -> Vec<Entry> {
        self.sections
            .iter()
            .find(|section| section.date == BACKLOG)
            .map(|section| {
                section
                    .tasks
                    .iter()
                    .cloned()
                    .map(|task| Entry {
                        date: BACKLOG.to_owned(),
                        task,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn add(&mut self, text: &str) -> Result<Task> {
        self.add_linked(text, None)
    }

    pub fn add_linked(&mut self, text: &str, jira: Option<&str>) -> Result<Task> {
        let text = clean_text(text);
        if text.is_empty() {
            bail!("task text cannot be empty");
        }
        let mut task = new_task(&text);
        task.jira = jira.map(normalize_jira_key).transpose()?;
        let index = self.ensure_today();
        self.sections[index].tasks.push(task.clone());
        self.save()?;
        Ok(task)
    }

    pub fn add_backlog(&mut self, text: &str) -> Result<Task> {
        let text = clean_text(text);
        if text.is_empty() {
            bail!("task text cannot be empty");
        }
        let task = new_task(&text);
        let index = self.ensure_backlog();
        self.sections[index].tasks.push(task.clone());
        self.save()?;
        Ok(task)
    }

    pub fn promote_backlog(&mut self, reference: &str) -> Result<Task> {
        let section_index = self
            .sections
            .iter()
            .position(|section| section.date == BACKLOG)
            .ok_or_else(|| anyhow::anyhow!("backlog is empty"))?;
        let tasks = &self.sections[section_index].tasks;
        let task_index = if let Ok(number) = reference.parse::<usize>() {
            (number > 0 && number <= tasks.len()).then_some(number - 1)
        } else {
            let matches = tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| task.id.starts_with(reference))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            (matches.len() == 1).then_some(matches[0])
        }
        .ok_or_else(|| anyhow::anyhow!("backlog task not found or ambiguous: {reference}"))?;

        let task = self.sections[section_index].tasks.remove(task_index);
        let today_index = self.ensure_today();
        self.sections[today_index].tasks.push(task.clone());
        self.sections.retain(|section| !section.tasks.is_empty());
        self.save()?;
        Ok(task)
    }

    pub fn move_task(
        &mut self,
        reference: &str,
        destination: &str,
        today_only_for_number: bool,
    ) -> Result<Task> {
        let destination = normalize_destination(destination)?;
        let (section_index, task_index) = self.find(reference, today_only_for_number)?;
        if self.sections[section_index].date == destination {
            return Ok(self.sections[section_index].tasks[task_index].clone());
        }

        let task = self.sections[section_index].tasks.remove(task_index);
        self.sections.retain(|section| !section.tasks.is_empty());
        let destination_index = self.ensure_section(&destination);
        self.sections[destination_index].tasks.push(task.clone());
        self.save()?;
        Ok(task)
    }

    pub fn link_jira(&mut self, reference: &str, key: &str) -> Result<Task> {
        let key = normalize_jira_key(key)?;
        let (section_index, task_index) = self.find(reference, true)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.jira = Some(key);
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn unlink_jira(&mut self, reference: &str) -> Result<Task> {
        let (section_index, task_index) = self.find(reference, true)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.jira = None;
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn entry(&self, reference: &str, today_only_for_number: bool) -> Result<Entry> {
        let (section_index, task_index) = self.find(reference, today_only_for_number)?;
        Ok(Entry {
            date: self.sections[section_index].date.clone(),
            task: self.sections[section_index].tasks[task_index].clone(),
        })
    }

    pub fn link_document(&mut self, reference: &str, link: &str) -> Result<Task> {
        let path = Path::new(link);
        if link.trim().is_empty() || path.file_name().is_none_or(|name| name != path.as_os_str()) {
            bail!("invalid task documentation link: {link}");
        }
        let (section_index, task_index) = self.find(reference, false)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.doc = Some(link.to_owned());
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn set_completed(
        &mut self,
        reference: &str,
        today_only_for_number: bool,
        completed: bool,
    ) -> Result<Task> {
        let (section_index, task_index) = self.find(reference, today_only_for_number)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.completed = completed;
        task.done = completed.then(|| Local::now().to_rfc3339_opts(SecondsFormat::Secs, false));
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    fn find(&self, reference: &str, today_only_for_number: bool) -> Result<(usize, usize)> {
        if let Ok(number) = reference.parse::<usize>() {
            let candidates = if today_only_for_number {
                let current = today();
                self.sections
                    .iter()
                    .enumerate()
                    .filter(|(_, section)| section.date == current)
                    .flat_map(|(section_index, section)| {
                        (0..section.tasks.len()).map(move |task_index| (section_index, task_index))
                    })
                    .collect::<Vec<_>>()
            } else {
                self.positions()
            };
            if number > 0 && number <= candidates.len() {
                return Ok(candidates[number - 1]);
            }
        }

        let matches = self
            .positions()
            .into_iter()
            .filter(|(section, task)| {
                self.sections[*section].tasks[*task]
                    .id
                    .starts_with(reference)
            })
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            Ok(matches[0])
        } else {
            bail!("task not found or ambiguous: {reference}")
        }
    }

    fn positions(&self) -> Vec<(usize, usize)> {
        self.sections
            .iter()
            .enumerate()
            .flat_map(|(section_index, section)| {
                (0..section.tasks.len()).map(move |task_index| (section_index, task_index))
            })
            .collect()
    }

    pub fn carry(&mut self) -> Result<usize> {
        let current = today();
        let current_date = Local::now().date_naive();
        let mut moved = Vec::new();
        for section in &mut self.sections {
            if section.date == current || section.date == BACKLOG {
                continue;
            }
            if NaiveDate::parse_from_str(&section.date, "%Y-%m-%d")
                .is_ok_and(|date| date >= current_date)
            {
                continue;
            }
            let tasks = std::mem::take(&mut section.tasks);
            for task in tasks {
                if task.completed {
                    section.tasks.push(task);
                } else {
                    moved.push(task);
                }
            }
        }
        let count = moved.len();
        if count == 0 {
            return Ok(0);
        }
        self.ensure_today();
        self.sections
            .retain(|section| !section.tasks.is_empty() || section.date == current);
        let destination = self
            .sections
            .iter_mut()
            .find(|section| section.date == current)
            .expect("today section exists");
        destination.tasks.extend(moved);
        self.save()?;
        Ok(count)
    }

    fn ensure_today(&mut self) -> usize {
        self.ensure_section(&today())
    }

    fn ensure_section(&mut self, label: &str) -> usize {
        if let Some(index) = self
            .sections
            .iter()
            .position(|section| section.date == label)
        {
            return index;
        }
        let section = Section {
            date: label.to_owned(),
            tasks: Vec::new(),
        };
        if label == BACKLOG {
            self.sections.push(section);
            self.sections.len() - 1
        } else {
            self.sections.insert(0, section);
            0
        }
    }

    fn ensure_backlog(&mut self) -> usize {
        self.ensure_section(BACKLOG)
    }

    fn save(&mut self) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        if self.legacy && self.path.exists() {
            let mut backup_name = self.path.as_os_str().to_owned();
            backup_name.push(".legacy.bak");
            let backup = PathBuf::from(backup_name);
            if !backup.exists() {
                fs::copy(&self.path, &backup)
                    .with_context(|| format!("failed to back up {}", self.path.display()))?;
            }
        }

        let mut output = String::from("# ZLS Tasks\n\n");
        for section in &self.sections {
            if section.tasks.is_empty() {
                continue;
            }
            output.push_str(&format!("## {}\n\n", section.date));
            for task in &section.tasks {
                let state = if task.completed { 'x' } else { ' ' };
                let done = task
                    .done
                    .as_ref()
                    .map_or_else(String::new, |done| format!(" done:{done}"));
                let jira = task
                    .jira
                    .as_ref()
                    .map_or_else(String::new, |key| format!(" jira:{key}"));
                let doc = task
                    .doc
                    .as_ref()
                    .map_or_else(String::new, |path| format!(" doc:{path}"));
                output.push_str(&format!(
                    "- [{state}] {} <!-- id:{}{jira}{doc}{done} -->\n",
                    task.text, task.id,
                ));
            }
            output.push('\n');
        }

        let mut temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
        temporary.write_all(output.trim_end().as_bytes())?;
        temporary.write_all(b"\n")?;
        temporary.flush()?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        self.legacy = false;
        Ok(())
    }
}

fn normalize_destination(destination: &str) -> Result<String> {
    match destination.trim().to_ascii_lowercase().as_str() {
        "today" => Ok(today()),
        "tomorrow" => Ok(tomorrow()),
        "backlog" => Ok(BACKLOG.to_owned()),
        _ => NaiveDate::parse_from_str(destination.trim(), "%Y-%m-%d")
            .map(|date| date.to_string())
            .with_context(|| {
                format!(
                    "invalid destination {destination:?}; use today, tomorrow, backlog, or YYYY-MM-DD"
                )
            }),
    }
}

fn normalize_jira_key(key: &str) -> Result<String> {
    let key = key.trim().to_ascii_uppercase();
    let valid = key.split_once('-').is_some_and(|(project, number)| {
        !project.is_empty()
            && project
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            && !number.is_empty()
            && number.chars().all(|character| character.is_ascii_digit())
    });
    if !valid {
        bail!("invalid Jira issue key: {key}");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_complete_and_reopen() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let mut store = Store::load(path)?;
        let added = store.add("write   the report")?;
        assert_eq!(store.entries_today()[0].task.text, "write the report");

        store.set_completed(&added.id, true, true)?;
        assert!(store.entries_all()[0].task.completed);
        assert!(store.entries_all()[0].task.done.is_some());

        store.set_completed(&added.id, false, false)?;
        assert!(!store.entries_today()[0].task.completed);
        Ok(())
    }

    #[test]
    fn legacy_file_is_stable_and_backed_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let original = "today one\n\ntoday two\n\n---\n\nolder task\n";
        fs::write(&path, original)?;

        let mut store = Store::load(path.clone())?;
        let ids = store
            .entries_all()
            .into_iter()
            .map(|entry| entry.task.id)
            .collect::<Vec<_>>();
        let loaded_again = Store::load(path.clone())?;
        assert_eq!(
            ids,
            loaded_again
                .entries_all()
                .into_iter()
                .map(|entry| entry.task.id)
                .collect::<Vec<_>>()
        );

        store.set_completed("1", true, true)?;
        assert_eq!(
            fs::read_to_string(path.with_extension("md.legacy.bak"))?,
            original
        );
        assert!(fs::read_to_string(path)?.contains("## Legacy 1"));
        Ok(())
    }

    #[test]
    fn carry_moves_only_old_open_tasks() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        fs::write(
            &path,
            "# ZLS Tasks\n\n## 2020-01-01\n\n- [ ] old open <!-- id:old -->\n- [x] old done <!-- id:done done:2020-01-01T12:00:00+00:00 -->\n",
        )?;
        let mut store = Store::load(path)?;
        assert_eq!(store.carry()?, 1);
        assert_eq!(store.entries_today()[0].task.id, "old");
        assert_eq!(store.entries_all().len(), 2);
        assert_eq!(store.carry()?, 0);
        assert_eq!(store.entries_all().len(), 2);
        Ok(())
    }

    #[test]
    fn jira_links_round_trip_in_markdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let mut store = Store::load(path.clone())?;
        let task = store.add("investigate traces")?;
        store.link_jira(&task.id, "obs-482")?;

        let loaded = Store::load(path.clone())?;
        assert_eq!(
            loaded.entries_today()[0].task.jira.as_deref(),
            Some("OBS-482")
        );
        assert!(fs::read_to_string(path)?.contains("jira:OBS-482"));
        Ok(())
    }

    #[test]
    fn documentation_links_round_trip_in_markdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let mut store = Store::load(path.clone())?;
        let task = store.add("document this task")?;

        store.link_document(&task.id, "task-notes.md")?;

        let loaded = Store::load(path.clone())?;
        assert_eq!(
            loaded.entries_today()[0].task.doc.as_deref(),
            Some("task-notes.md")
        );
        assert!(fs::read_to_string(path)?.contains("doc:task-notes.md"));
        assert!(store.link_document(&task.id, "../outside.md").is_err());
        Ok(())
    }

    #[test]
    fn backlog_is_not_carried_and_can_be_promoted() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let mut store = Store::load(path)?;
        let task = store.add_backlog("future investigation")?;

        assert_eq!(store.carry()?, 0);
        assert!(store.entries_today().is_empty());
        assert_eq!(store.entries_backlog()[0].task.id, task.id);

        store.promote_backlog(&task.id)?;
        assert!(store.entries_backlog().is_empty());
        assert_eq!(store.entries_today()[0].task.text, "future investigation");
        Ok(())
    }

    #[test]
    fn tasks_can_move_to_backlog_and_tomorrow_is_not_carried() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        let mut store = Store::load(path)?;
        let backlog_task = store.add("move to backlog")?;
        let tomorrow_task = store.add("do tomorrow")?;

        store.move_task(&backlog_task.id, "backlog", false)?;
        store.move_task(&tomorrow_task.id, "tomorrow", false)?;
        assert_eq!(store.entries_backlog()[0].task.id, backlog_task.id);
        assert!(
            store
                .entries_all()
                .iter()
                .any(|entry| entry.date == tomorrow() && entry.task.id == tomorrow_task.id)
        );
        assert_eq!(store.carry()?, 0);
        assert!(store.entries_today().is_empty());
        Ok(())
    }
}
