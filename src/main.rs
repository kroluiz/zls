mod config;
mod jira;
mod keybindings;
mod store;
mod ui;

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use config::AppConfig;
use store::{Entry, Store, today};

#[derive(Parser)]
#[command(about = "A popup-first, Markdown-backed daily task list")]
struct Cli {
    /// Temporarily override the configured task file
    #[arg(long, global = true)]
    file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
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
    Comment { key: String, text: String },
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = AppConfig::load_or_create(initial_task_file()?)?;
    let file = cli
        .file
        .or_else(|| env::var_os("ZLS_FILE").map(PathBuf::from))
        .unwrap_or(config.tasks.path);
    let mut store = Store::load(expand_home(file))?;
    match cli.command.unwrap_or(Action::Ui) {
        Action::Ui => ui::run(&mut store),
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
                let user = client.test_auth()?;
                println!("Authenticated as {}", user.display_name);
                Ok(())
            }
            JiraAction::Test => {
                let client = jira::JiraClient::from_config()?;
                let user = client.test_auth()?;
                println!("Authenticated as {}", user.display_name);
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
            JiraAction::Comment { key, text } => {
                let comment = jira::JiraClient::from_config()?.post_comment(&key, &text)?;
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
                println!("Updated Jira status for {key}");
                Ok(())
            }
            JiraAction::Link { task, key } => {
                println!("{}", serde_json::to_string(&store.link_jira(&task, &key)?)?);
                Ok(())
            }
            JiraAction::Unlink { task } => {
                println!("{}", serde_json::to_string(&store.unlink_jira(&task)?)?);
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
