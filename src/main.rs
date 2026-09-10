mod agent_guide;
mod config;
mod docs;
mod jira;
mod keybindings;
mod store;
mod ui;

use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use config::AppConfig;
use store::{Entry, Store, WeekReport, today};

#[derive(Parser)]
#[command(version, about = "A popup-first, Markdown-backed daily task list")]
struct Cli {
    /// Temporarily override the configured task file
    #[arg(long)]
    file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Print the version-matched agent contract as JSON without loading local state
    AgentGuide,
    /// Open the interactive interface
    Ui,
    /// Open the interface in a tmux popup
    Popup,
    /// Add a task for today
    Add { text: String },
    /// Complete a task by today's number or ID
    Done { task: String },
    /// Reopen a task by number or ID
    Reopen { task: String },
    /// Show today's tasks
    Today {
        #[arg(long)]
        json: bool,
    },
    /// Show task history
    History {
        #[arg(long)]
        json: bool,
    },
    /// Move older unfinished tasks into today
    Carry,
    /// Print a compact tmux-friendly status
    Status,
    /// Move a task to today, tomorrow, backlog, or a date
    Move { task: String, destination: String },
    /// Show task activity for an ISO week
    Week {
        /// Select the current week (0), previous week (1), and so on
        #[arg(long, default_value_t = 0)]
        weeks_ago: u32,
        #[arg(long)]
        json: bool,
    },
    /// Create, edit, or read task documentation
    Docs {
        #[command(subcommand)]
        command: DocsAction,
    },
    /// Emit task and documentation context for AI tools
    Context {
        task: String,
        #[arg(long, value_enum, default_value_t = ContextFormat::Json)]
        format: ContextFormat,
        /// Include current Jira issue details, excluding comments
        #[arg(long)]
        jira: bool,
    },
    /// Manage tasks without a scheduled day
    Backlog {
        #[command(subcommand)]
        command: BacklogAction,
    },
    /// Configure and use Jira Cloud
    Jira {
        #[command(subcommand)]
        command: JiraAction,
    },
}

#[derive(Subcommand)]
enum BacklogAction {
    /// List backlog tasks
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a backlog task
    Add { text: String },
    /// Move a backlog task into today
    Promote { task: String },
}

#[derive(Subcommand)]
enum DocsAction {
    /// Open task documentation in VISUAL or EDITOR
    Edit { task: String },
    /// Replace task documentation from a file or stdin
    Set {
        task: String,
        #[command(flatten)]
        input: DocumentInput,
    },
    /// Append to task documentation from a file or stdin
    Append {
        task: String,
        #[command(flatten)]
        input: DocumentInput,
    },
    /// Print task documentation
    Show { task: String },
    /// Print the path to existing task documentation
    Path { task: String },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct DocumentInput {
    /// Read documentation from a file
    #[arg(long = "file", value_name = "PATH")]
    input_file: Option<PathBuf>,
    /// Read documentation from stdin
    #[arg(long)]
    stdin: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ContextFormat {
    Json,
    Markdown,
}

#[derive(Subcommand)]
enum JiraAction {
    /// Configure Jira Cloud credentials
    Auth,
    /// Verify Jira authentication
    Test,
    /// Search Jira issues
    Search {
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List accessible Jira projects
    Projects,
    /// Show or set the single active Jira project
    Project { key: Option<String> },
    /// Display a Jira issue card
    Show { key: String },
    /// Post a Jira comment
    Comment {
        key: String,
        text: Option<String>,
        /// Read the comment body from a file; use - for stdin
        #[arg(long, visible_alias = "from-file", conflicts_with_all = ["text", "stdin"])]
        body_file: Option<PathBuf>,
        /// Read the comment body from stdin
        #[arg(long, conflicts_with_all = ["text", "body_file"])]
        stdin: bool,
    },
    /// List valid status transitions for a Jira issue
    Transitions { key: String },
    /// Apply a Jira status transition by ID
    Transition { key: String, id: String },
    /// Link today's local task to a Jira issue
    Link { task: String, key: String },
    /// Remove a Jira link from today's local task
    Unlink { task: String },
    /// Create today's local task from a Jira issue
    Import { key: String },
}

fn initial_task_file() -> Result<PathBuf> {
    let legacy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the project has a parent directory")
        .join("todo.md");
    if legacy.exists() {
        Ok(legacy)
    } else {
        config::default_data_path()
    }
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(suffix) = path
        .to_str()
        .and_then(|value| value.strip_prefix("~/"))
        .map(str::to_owned)
    else {
        return path;
    };
    env::var_os("HOME").map_or(path, |home| Path::new(&home).join(suffix))
}

fn print_entries(entries: &[Entry], as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(entries)?);
    } else if entries.is_empty() {
        println!("No tasks.");
    } else {
        for (index, entry) in entries.iter().enumerate() {
            let state = if entry.task.completed { 'x' } else { ' ' };
            println!(
                "{:>2}. [{state}] {}  ({}, {})",
                index + 1,
                entry.task.text,
                entry.date,
                entry.task.id
            );
        }
    }
    Ok(())
}

fn print_week_report(report: &WeekReport, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "ISO WEEK {}-W{:02} / {} to {}",
        report.iso_year, report.iso_week, report.start, report.end
    );
    println!(
        "{} touched tasks / {} touches / {} completed / {} Jira-linked\n",
        report.touched, report.touches, report.completed, report.jira_linked
    );
    for day in &report.days {
        let title = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
            .map(|date| date.format("%A / %Y-%m-%d").to_string())
            .unwrap_or_else(|_| day.date.clone());
        println!("{title}");
        if day.tasks.is_empty() {
            println!("  No task activity.");
        }
        for entry in &day.tasks {
            let jira = entry
                .task
                .jira
                .as_ref()
                .map_or_else(String::new, |key| format!(" [{key}]"));
            let docs = if entry.task.doc.is_some() {
                " [doc]"
            } else {
                ""
            };
            let state = if entry.task.completed { 'x' } else { ' ' };
            println!(
                "  [{state}] {}{jira}{docs} / {}",
                entry.task.text,
                entry.task.touch_summary()
            );
        }
        println!();
    }
    if !report.undated.is_empty() {
        println!("Undated");
        for entry in &report.undated {
            let jira = entry
                .task
                .jira
                .as_ref()
                .map_or_else(String::new, |key| format!(" [{key}]"));
            let docs = if entry.task.doc.is_some() {
                " [doc]"
            } else {
                ""
            };
            println!("  [x] {}{jira}{docs} / legacy completion", entry.task.text);
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Serialize)]
struct TaskContext {
    task: Entry,
    documentation: Option<String>,
    document_updated_at: Option<String>,
    issue: Option<jira::IssueCard>,
}

fn ensure_task_document(store: &mut Store, entry: &Entry, docs_path: &Path) -> Result<PathBuf> {
    let jira_config = jira::JiraConfig::load().ok();
    let (path, link) = docs::ensure_document(docs_path, entry, jira_config.as_ref())?;
    if entry.task.doc.is_none() {
        store.link_document(&entry.task.id, &link)?;
    }
    Ok(path)
}

fn read_comment_input(
    text: Option<String>,
    body_file: Option<PathBuf>,
    stdin: bool,
) -> Result<String> {
    let body = match (text, body_file, stdin) {
        (Some(text), None, false) => text,
        (None, Some(path), false) if path == Path::new("-") => {
            let mut body = String::new();
            io::stdin().read_to_string(&mut body)?;
            body
        }
        (None, Some(path), false) => {
            let path = expand_home(path);
            fs::read_to_string(&path)
                .with_context(|| format!("failed to read Jira comment from {}", path.display()))?
        }
        (None, None, true) => {
            let mut body = String::new();
            io::stdin().read_to_string(&mut body)?;
            body
        }
        _ => bail!("provide comment text, --body-file, or --stdin exactly once"),
    };
    if body.trim().is_empty() {
        bail!("Jira comment must not be empty");
    }
    Ok(body)
}

fn read_document_input(input: DocumentInput) -> Result<String> {
    let content = if let Some(path) = input.input_file {
        let path = expand_home(path);
        fs::read_to_string(&path)
            .with_context(|| format!("failed to read documentation from {}", path.display()))?
    } else if input.stdin {
        let mut content = String::new();
        io::stdin().read_to_string(&mut content)?;
        content
    } else {
        unreachable!("clap requires --file or --stdin")
    };
    if content.trim().is_empty() {
        bail!("documentation must not be empty");
    }
    Ok(content)
}

fn print_task_context(
    context: &TaskContext,
    format: ContextFormat,
    mut output: impl Write,
) -> Result<()> {
    match format {
        ContextFormat::Json => writeln!(output, "{}", serde_json::to_string_pretty(context)?)?,
        ContextFormat::Markdown => {
            writeln!(output, "# Task Context\n")?;
            writeln!(output, "- ID: {}", context.task.task.id)?;
            writeln!(output, "- Title: {}", context.task.task.text)?;
            writeln!(output, "- Date: {}", context.task.date)?;
            writeln!(output, "- Completed: {}", context.task.task.completed)?;
            if let Some(key) = context.task.task.jira.as_deref() {
                writeln!(output, "- Jira: {key}")?;
            }
            writeln!(output, "\n## Documentation\n")?;
            writeln!(
                output,
                "{}",
                context
                    .documentation
                    .as_deref()
                    .unwrap_or("Not documented.")
            )?;
            if let Some(issue) = context.issue.as_ref() {
                writeln!(output, "\n## Jira Issue\n")?;
                writeln!(output, "```json")?;
                writeln!(output, "{}", serde_json::to_string_pretty(issue)?)?;
                writeln!(output, "```")?;
            }
        }
    }
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Action::AgentGuide)) {
        println!("{}", serde_json::to_string_pretty(&agent_guide::guide())?);
        return Ok(());
    }
    let mut config = AppConfig::load_or_create(initial_task_file()?)?;
    let docs_path = expand_home(config.docs.path.clone());
    let file = cli
        .file
        .or_else(|| env::var_os("ZLS_FILE").map(PathBuf::from))
        .unwrap_or_else(|| config.tasks.path.clone());
    let mut store = Store::load(expand_home(file))?;
    match cli.command.unwrap_or(Action::Ui) {
        Action::AgentGuide => unreachable!("agent guide is dispatched before loading state"),
        Action::Ui => ui::run(&mut store, &docs_path, &mut config),
        Action::Popup => {
            if env::var_os("TMUX").is_none() {
                bail!("popup must be run inside tmux");
            }
            let executable = env::current_exe().context("failed to locate the zls executable")?;
            let invocation = format!(
                "{} --file {} ui",
                shell_quote(&executable.to_string_lossy()),
                shell_quote(&store.path().to_string_lossy())
            );
            let status = Command::new("tmux")
                .args([
                    "display-popup",
                    "-E",
                    "-w",
                    "80%",
                    "-h",
                    "70%",
                    "-s",
                    "fg=default,bg=default",
                    &invocation,
                ])
                .status()
                .context("failed to launch tmux")?;
            if !status.success() {
                bail!("tmux popup exited with {status}");
            }
            Ok(())
        }
        Action::Add { text } => {
            let task = store.add(&text)?;
            println!(
                "{}",
                serde_json::to_string(&Entry {
                    date: today(),
                    task
                })?
            );
            Ok(())
        }
        Action::Done { task } => {
            println!(
                "{}",
                serde_json::to_string(&store.set_completed(&task, true, true)?)?
            );
            Ok(())
        }
        Action::Reopen { task } => {
            println!(
                "{}",
                serde_json::to_string(&store.set_completed(&task, false, false)?)?
            );
            Ok(())
        }
        Action::Today { json } => print_entries(&store.entries_today(), json),
        Action::History { json } => print_entries(&store.entries_all(), json),
        Action::Carry => {
            println!("{}", store.carry()?);
            Ok(())
        }
        Action::Status => {
            let open = store
                .entries_today()
                .into_iter()
                .filter(|entry| !entry.task.completed)
                .collect::<Vec<_>>();
            if let Some(first) = open.first() {
                let preview = first.task.text.chars().take(40).collect::<String>();
                println!("todo: {} | {preview}", open.len());
            } else {
                println!("todo: clear");
            }
            Ok(())
        }
        Action::Move { task, destination } => {
            println!(
                "{}",
                serde_json::to_string(&store.move_task(&task, &destination, true)?)?
            );
            Ok(())
        }
        Action::Week { weeks_ago, json } => {
            print_week_report(&store.week_report_weeks_ago(weeks_ago)?, json)
        }
        Action::Docs { command } => match command {
            DocsAction::Edit { task } => {
                let entry = store.entry(&task, true)?;
                let path = ensure_task_document(&mut store, &entry, &docs_path)?;
                docs::edit_document(&path)?;
                store.record_touch(&entry.task.id, "docs-edited")?;
                Ok(())
            }
            DocsAction::Set { task, input } => {
                let content = read_document_input(input)?;
                let entry = store.entry(&task, true)?;
                let path = ensure_task_document(&mut store, &entry, &docs_path)?;
                let acknowledgement =
                    docs::write_task_document(&entry.task.id, &path, &content, false)?;
                store.record_touch(&entry.task.id, "docs-set")?;
                println!("{}", serde_json::to_string(&acknowledgement)?);
                Ok(())
            }
            DocsAction::Append { task, input } => {
                let content = read_document_input(input)?;
                let entry = store.entry(&task, true)?;
                let path = ensure_task_document(&mut store, &entry, &docs_path)?;
                let acknowledgement =
                    docs::write_task_document(&entry.task.id, &path, &content, true)?;
                store.record_touch(&entry.task.id, "docs-appended")?;
                println!("{}", serde_json::to_string(&acknowledgement)?);
                Ok(())
            }
            DocsAction::Show { task } => {
                let entry = store.entry(&task, true)?;
                let documentation =
                    docs::read_document(&docs_path, &entry.task)?.ok_or_else(|| {
                        anyhow::anyhow!("task has no documentation; run `zls docs edit {task}`")
                    })?;
                print!("{documentation}");
                Ok(())
            }
            DocsAction::Path { task } => {
                let entry = store.entry(&task, true)?;
                let path =
                    docs::linked_document_path(&docs_path, &entry.task)?.ok_or_else(|| {
                        anyhow::anyhow!("task has no documentation; run `zls docs edit {task}`")
                    })?;
                println!("{}", path.display());
                Ok(())
            }
        },
        Action::Context { task, format, jira } => {
            let entry = store.entry(&task, true)?;
            let documentation = docs::read_document(&docs_path, &entry.task)?;
            let document_updated_at = docs::linked_document_path(&docs_path, &entry.task)?
                .map(|path| docs::document_updated_at(&path))
                .transpose()?;
            let issue = if jira {
                let key = entry
                    .task
                    .jira
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("task is not linked to Jira"))?;
                Some(jira::JiraClient::from_config()?.issue_context(key)?)
            } else {
                None
            };
            print_task_context(
                &TaskContext {
                    task: entry,
                    documentation,
                    document_updated_at,
                    issue,
                },
                format,
                io::stdout().lock(),
            )
        }
        Action::Backlog { command } => match command {
            BacklogAction::List { json } => print_entries(&store.entries_backlog(), json),
            BacklogAction::Add { text } => {
                let task = store.add_backlog(&text)?;
                println!(
                    "{}",
                    serde_json::to_string(&Entry {
                        date: store::BACKLOG.to_owned(),
                        task
                    })?
                );
                Ok(())
            }
            BacklogAction::Promote { task } => {
                println!("{}", serde_json::to_string(&store.promote_backlog(&task)?)?);
                Ok(())
            }
        },
        Action::Jira { command } => match command {
            JiraAction::Auth => {
                let client = jira::interactive_auth_setup()?;
                println!("Authenticated as {}", client.test_auth()?);
                Ok(())
            }
            JiraAction::Test => {
                let client = jira::JiraClient::from_config()?;
                println!("Authenticated as {}", client.test_auth()?);
                Ok(())
            }
            JiraAction::Search { query, json } => {
                let issues = jira::JiraClient::from_config()?
                    .search_issues(query.as_deref().unwrap_or_default())?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issues)?);
                } else {
                    for issue in issues {
                        println!("{}  [{}]  {}", issue.key, issue.status, issue.summary);
                    }
                }
                Ok(())
            }
            JiraAction::Projects => {
                for project in jira::JiraClient::from_config()?.projects()? {
                    println!("{}  {}", project.key, project.name);
                }
                Ok(())
            }
            JiraAction::Project { key } => {
                let mut client = jira::JiraClient::from_config()?;
                if let Some(key) = key {
                    client.set_project(&key)?;
                    println!("Jira project set to {key}");
                } else if let Some(project) = client.config().project.as_deref() {
                    println!("{project}");
                } else {
                    println!("No Jira project selected.");
                }
                Ok(())
            }
            JiraAction::Show { key } => {
                let card = jira::JiraClient::from_config()?.issue_card(&key)?;
                println!("{}", serde_json::to_string_pretty(&card)?);
                Ok(())
            }
            JiraAction::Comment {
                key,
                text,
                body_file,
                stdin,
            } => {
                let text = read_comment_input(text, body_file, stdin)?;
                let comment = jira::JiraClient::from_config()?.post_comment(&key, &text)?;
                store.record_jira_touch(&key, "jira-commented")?;
                println!("{}", serde_json::to_string_pretty(&comment)?);
                Ok(())
            }
            JiraAction::Transitions { key } => {
                for transition in jira::JiraClient::from_config()?.transitions(&key)? {
                    println!(
                        "{}  {} -> {}",
                        transition.id, transition.name, transition.to_status
                    );
                }
                Ok(())
            }
            JiraAction::Transition { key, id } => {
                jira::JiraClient::from_config()?.transition_issue(&key, &id)?;
                store.record_jira_touch(&key, "jira-transitioned")?;
                println!("Updated Jira status for {key}");
                Ok(())
            }
            JiraAction::Link { task, key } => {
                let entry = store.entry(&task, true)?;
                let linked = store.link_jira(&task, &key)?;
                if let Err(error) = docs::update_jira_link(
                    &docs_path,
                    &entry.task,
                    linked.jira.as_deref(),
                    jira::JiraConfig::load().ok().as_ref(),
                ) {
                    eprintln!("zls: Jira linked, but documentation was not updated: {error:#}");
                }
                println!("{}", serde_json::to_string(&linked)?);
                Ok(())
            }
            JiraAction::Unlink { task } => {
                let entry = store.entry(&task, true)?;
                let unlinked = store.unlink_jira(&task)?;
                if let Err(error) = docs::update_jira_link(
                    &docs_path,
                    &entry.task,
                    None,
                    jira::JiraConfig::load().ok().as_ref(),
                ) {
                    eprintln!("zls: Jira unlinked, but documentation was not updated: {error:#}");
                }
                println!("{}", serde_json::to_string(&unlinked)?);
                Ok(())
            }
            JiraAction::Import { key } => {
                let card = jira::JiraClient::from_config()?.issue_card(&key)?;
                let task = store.add_linked(&card.summary, Some(&card.key))?;
                println!(
                    "{}",
                    serde_json::to_string(&Entry {
                        date: today(),
                        task
                    })?
                );
                Ok(())
            }
        },
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zls: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_distinguishes_linked_identity_from_fetched_issue() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("tasks.md"))?;
        let task = store.add("Investigate")?;
        for linked in [false, true] {
            if linked {
                store.link_jira(&task.id, "MP-467")?;
            }
            for fetched in [false, true] {
                if fetched && !linked {
                    continue;
                }
                let context = TaskContext {
                    task: store.entry(&task.id, true)?,
                    documentation: Some("Custom Jira: WRONG-1".to_owned()),
                    document_updated_at: None,
                    issue: fetched.then(|| {
                        serde_json::from_value(serde_json::json!({
                            "key": "MP-467", "summary": "Fetched summary", "status": "Open",
                            "description": "", "subtasks": [], "links": [], "comments": []
                        }))
                        .unwrap()
                    }),
                };
                let mut json = Vec::new();
                print_task_context(&context, ContextFormat::Json, &mut json)?;
                let json: serde_json::Value = serde_json::from_slice(&json)?;
                assert!(json.get("jira").is_none());
                assert_eq!(json["task"].get("jira").is_some(), linked);
                assert_eq!(json["issue"].is_null(), !fetched);
                if linked {
                    assert_eq!(json["task"]["jira"], "MP-467");
                }
                if fetched {
                    assert_eq!(json["issue"]["key"], "MP-467");
                }
                let mut markdown = Vec::new();
                print_task_context(&context, ContextFormat::Markdown, &mut markdown)?;
                let markdown = String::from_utf8(markdown)?;
                assert_eq!(markdown.contains("- Jira: MP-467"), linked);
                assert_eq!(markdown.contains("## Jira Issue"), fetched);
                assert_eq!(markdown.contains("Fetched summary"), fetched);
                assert!(markdown.contains("Custom Jira: WRONG-1"));
            }
        }
        Ok(())
    }

    #[test]
    fn reads_cli_input_from_text_or_file() -> Result<()> {
        assert_eq!(
            read_comment_input(Some("direct comment".to_owned()), None, false)?,
            "direct comment"
        );

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("comment.md");
        fs::write(&path, "multiline\ncomment\n")?;
        assert_eq!(
            read_comment_input(None, Some(path), false)?,
            "multiline\ncomment\n"
        );
        assert!(read_comment_input(None, None, false).is_err());
        assert!(read_comment_input(Some("text".to_owned()), None, true).is_err());

        let document = directory.path().join("document.md");
        fs::write(&document, "documentation\n")?;
        assert_eq!(
            read_document_input(DocumentInput {
                input_file: Some(document.clone()),
                stdin: false,
            })?,
            "documentation\n"
        );
        fs::write(&document, " \n")?;
        assert!(
            read_document_input(DocumentInput {
                input_file: Some(document),
                stdin: false,
            })
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn separates_task_and_document_file_arguments() {
        let cli = Cli::try_parse_from([
            "zls", "--file", "tasks.md", "docs", "set", "abc12345", "--file", "notes.md",
        ])
        .expect("task and document files should parse");
        assert_eq!(cli.file, Some(PathBuf::from("tasks.md")));
        assert!(matches!(
            cli.command,
            Some(Action::Docs {
                command: DocsAction::Set {
                    input: DocumentInput {
                        input_file: Some(path),
                        stdin: false,
                    },
                    ..
                }
            }) if path == Path::new("notes.md")
        ));
    }
}
