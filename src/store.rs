use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, SecondsFormat, Weekday};
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<Touch>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Touch {
    pub at: String,
    pub action: String,
}

impl Task {
    pub fn touch_summary(&self) -> String {
        let times = self
            .touches
            .iter()
            .filter_map(|touch| DateTime::parse_from_rfc3339(&touch.at).ok())
            .map(|at| at.format("%H:%M").to_string())
            .collect::<Vec<_>>();
        let range = match (times.first(), times.last()) {
            (Some(first), Some(last)) if first != last => format!(" {first}-{last}"),
            (Some(first), _) => format!(" {first}"),
            _ => String::new(),
        };
        let mut actions = Vec::new();
        for touch in &self.touches {
            let action = touch.action.replace('-', " ");
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
        format!(
            "{} touch{}{} / {}",
            self.touches.len(),
            if self.touches.len() == 1 { "" } else { "es" },
            range,
            actions.join(", ")
        )
    }

    fn touch(&mut self, action: &str) {
        self.touches.push(Touch {
            at: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
            action: action.to_owned(),
        });
    }
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

#[derive(Clone, Debug, Serialize)]
pub struct WeekDay {
    pub date: String,
    pub tasks: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WeekReport {
    pub iso_year: i32,
    pub iso_week: u32,
    pub start: String,
    pub end: String,
    pub touched: usize,
    pub touches: usize,
    pub completed: usize,
    pub jira_linked: usize,
    pub days: Vec<WeekDay>,
    pub undated: Vec<Entry>,
}

impl WeekReport {
    pub fn entries(&self) -> Vec<Entry> {
        self.days
            .iter()
            .flat_map(|day| day.tasks.iter().cloned())
            .chain(self.undated.iter().cloned())
            .collect()
    }
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
        touches: Vec::new(),
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
            let completed = captures[1].eq_ignore_ascii_case("x");
            let done = value("done").map(str::to_owned);
            let mut touches = metadata
                .split_whitespace()
                .filter_map(|field| field.strip_prefix("touch:"))
                .filter_map(|value| value.split_once('/'))
                .filter(|(at, action)| {
                    DateTime::parse_from_rfc3339(at).is_ok() && !action.is_empty()
                })
                .map(|(at, action)| Touch {
                    at: at.to_owned(),
                    action: action.to_owned(),
                })
                .collect::<Vec<_>>();
            if completed
                && let Some(at) = done.as_deref()
                && DateTime::parse_from_rfc3339(at).is_ok()
                && !touches
                    .iter()
                    .any(|touch| touch.at == at && touch.action == "completed")
            {
                touches.push(Touch {
                    at: at.to_owned(),
                    action: "completed".to_owned(),
                });
            }
            touches.sort_by_key(|touch| {
                DateTime::parse_from_rfc3339(&touch.at)
                    .expect("touch timestamps were validated")
                    .timestamp()
            });
            self.sections[index].tasks.push(Task {
                id: value("id").map_or_else(|| new_task(&captures[2]).id, str::to_owned),
                text: captures[2].to_owned(),
                completed,
                done,
                jira: value("jira").map(str::to_owned),
                doc: value("doc").map(str::to_owned),
                touches,
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

    pub fn week_report_weeks_ago(&self, weeks_ago: u32) -> Result<WeekReport> {
        let current = Local::now().date_naive();
        let current_monday =
            current - Duration::days(i64::from(current.weekday().num_days_from_monday()));
        let start = current_monday
            .checked_sub_signed(Duration::weeks(i64::from(weeks_ago)))
            .ok_or_else(|| {
                anyhow::anyhow!("weeks-ago value is outside the supported date range")
            })?;
        self.week_report(start.iso_week().year(), start.iso_week().week())
    }

    pub fn week_report(&self, iso_year: i32, iso_week: u32) -> Result<WeekReport> {
        let start = NaiveDate::from_isoywd_opt(iso_year, iso_week, Weekday::Mon)
            .ok_or_else(|| anyhow::anyhow!("invalid ISO week {iso_year}-W{iso_week:02}"))?;
        let end = start
            .checked_add_signed(Duration::days(6))
            .ok_or_else(|| anyhow::anyhow!("ISO week end is outside the supported date range"))?;
        let mut days = (0..7)
            .map(|offset| WeekDay {
                date: (start + Duration::days(offset)).to_string(),
                tasks: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut undated = Vec::new();

        for entry in self.entries_all() {
            for (offset, day) in days.iter_mut().enumerate() {
                let date = start + Duration::days(offset as i64);
                let mut activity = entry.clone();
                activity.task.touches.retain(|touch| {
                    DateTime::parse_from_rfc3339(&touch.at).is_ok_and(|at| at.date_naive() == date)
                });
                if !activity.task.touches.is_empty() {
                    day.tasks.push(activity);
                }
            }
            if entry.task.completed
                && entry
                    .task
                    .done
                    .as_deref()
                    .is_none_or(|done| DateTime::parse_from_rfc3339(done).is_err())
                && NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
                    .is_ok_and(|date| date >= start && date <= end)
            {
                undated.push(entry);
            }
        }

        for day in &mut days {
            day.tasks.sort_by_key(|entry| {
                entry
                    .task
                    .touches
                    .first()
                    .and_then(|touch| DateTime::parse_from_rfc3339(&touch.at).ok())
                    .map(|at| at.timestamp())
            });
        }
        let touched = days.iter().map(|day| day.tasks.len()).sum::<usize>();
        let touches = days
            .iter()
            .flat_map(|day| &day.tasks)
            .map(|entry| entry.task.touches.len())
            .sum();
        let completed = days
            .iter()
            .flat_map(|day| &day.tasks)
            .flat_map(|entry| &entry.task.touches)
            .filter(|touch| touch.action == "completed")
            .count()
            + undated.len();
        let jira_linked = days
            .iter()
            .flat_map(|day| &day.tasks)
            .chain(&undated)
            .filter(|entry| entry.task.jira.is_some())
            .count();
        Ok(WeekReport {
            iso_year,
            iso_week,
            start: start.to_string(),
            end: end.to_string(),
            touched,
            touches,
            completed,
            jira_linked,
            days,
            undated,
        })
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
        task.touch("added");
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
        let mut task = new_task(&text);
        task.touch("added");
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
        let task_index = if let Ok(number) = reference.parse::<usize>()
            && number > 0
            && number <= tasks.len()
        {
            Some(number - 1)
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

        let mut task = self.sections[section_index].tasks.remove(task_index);
        task.touch("promoted");
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

        let mut task = self.sections[section_index].tasks.remove(task_index);
        task.touch("moved");
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
        task.touch("jira-linked");
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn unlink_jira(&mut self, reference: &str) -> Result<Task> {
        let (section_index, task_index) = self.find(reference, true)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.jira = None;
        task.touch("jira-unlinked");
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
        task.touch("docs-linked");
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
        task.touch(if completed { "completed" } else { "reopened" });
        task.done = completed.then(|| {
            task.touches
                .last()
                .expect("completion touch exists")
                .at
                .clone()
        });
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn record_touch(&mut self, reference: &str, action: &str) -> Result<Task> {
        let (section_index, task_index) = self.find(reference, false)?;
        let task = &mut self.sections[section_index].tasks[task_index];
        task.touch(action);
        let result = task.clone();
        self.save()?;
        Ok(result)
    }

    pub fn record_jira_touch(&mut self, key: &str, action: &str) -> Result<usize> {
        let key = normalize_jira_key(key)?;
        let mut count = 0;
        for section in &mut self.sections {
            for task in &mut section.tasks {
                if task.jira.as_deref() == Some(&key) {
                    task.touch(action);
                    count += 1;
                }
            }
        }
        if count > 0 {
            self.save()?;
        }
        Ok(count)
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
                let touches = task.touches.iter().fold(String::new(), |mut value, touch| {
                    value.push_str(&format!(" touch:{}/{}", touch.at, touch.action));
                    value
                });
                output.push_str(&format!(
                    "- [{state}] {} <!-- id:{}{jira}{doc}{done}{touches} -->\n",
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
        let mut store = Store::load(path.clone())?;
        let added = store.add("write   the report")?;
        assert_eq!(store.entries_today()[0].task.text, "write the report");

        store.set_completed(&added.id, true, true)?;
        assert!(store.entries_all()[0].task.completed);
        assert!(store.entries_all()[0].task.done.is_some());
        assert_eq!(
            store.entries_all()[0].task.done,
            store.entries_all()[0]
                .task
                .touches
                .last()
                .map(|touch| touch.at.clone())
        );

        store.set_completed(&added.id, false, false)?;
        assert!(!store.entries_today()[0].task.completed);
        let loaded = Store::load(path)?;
        assert_eq!(
            loaded.entries_today()[0]
                .task
                .touches
                .iter()
                .map(|touch| touch.action.as_str())
                .collect::<Vec<_>>(),
            ["added", "completed", "reopened"]
        );
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
    fn weekly_report_groups_all_activity_and_keeps_undated_completions() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("todo.md");
        fs::write(
            &path,
            "# ZLS Tasks\n\n## 2026-08-20\n\n- [x] finished Tuesday <!-- id:tue jira:MP-1 done:2026-09-01T00:30:00+14:00 -->\n\n## 2026-09-02\n\n- [x] old completion <!-- id:old done:2026-08-20T10:00:00-03:00 -->\n- [x] missing timestamp <!-- id:undated -->\n- [ ] still open <!-- id:open touch:2026-09-02T09:00:00-03:00/added touch:2026-09-02T15:12:00-03:00/docs-appended -->\n",
        )?;
        let store = Store::load(path)?;

        let report = store.week_report(2026, 36)?;

        assert_eq!(report.start, "2026-08-31");
        assert_eq!(report.end, "2026-09-06");
        assert_eq!(report.touched, 2);
        assert_eq!(report.touches, 3);
        assert_eq!(report.completed, 2);
        assert_eq!(report.jira_linked, 1);
        assert_eq!(report.days.len(), 7);
        assert_eq!(report.days[1].tasks[0].task.id, "tue");
        assert_eq!(report.days[2].tasks[0].task.id, "open");
        assert_eq!(
            report.days[2].tasks[0].task.touch_summary(),
            "2 touches 09:00-15:12 / added, docs appended"
        );
        assert_eq!(report.undated[0].task.id, "undated");
        assert_eq!(report.entries().len(), 3);
        assert!(store.week_report_weeks_ago(u32::MAX).is_err());

        let year_boundary = store.week_report(2025, 1)?;
        assert_eq!(year_boundary.start, "2024-12-30");
        assert_eq!(year_boundary.end, "2025-01-05");
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
    fn promotes_backlog_tasks_with_numeric_ids() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        store.add_backlog("numeric id")?;
        store
            .sections
            .iter_mut()
            .find(|section| section.date == BACKLOG)
            .expect("backlog section exists")
            .tasks[0]
            .id = "94129824".to_owned();
        store.save()?;

        store.promote_backlog("94129824")?;

        assert_eq!(store.entries_today()[0].task.id, "94129824");
        assert!(store.entries_backlog().is_empty());
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
