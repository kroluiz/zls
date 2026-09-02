use std::{
    io::{self, stdout},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    config::config_path,
    jira::{
        IssueCard, IssueSummary, JiraClient, JiraComment, JiraConfig, ProjectSummary, Transition,
        format_jira_datetime,
    },
    keybindings::{KEYBINDINGS, Section, compact_hint},
    store::{BACKLOG, Entry, Store, today, tomorrow},
};

enum Mode {
    Normal,
    Add,
    AddBacklog,
    Move,
    Help,
    Configuration,
    Search,
    Project,
    Transition,
    Comment,
    ConfirmComment,
}

enum JiraEvent {
    Card(String, Box<Result<IssueCard, String>>),
    Search(u64, Result<Vec<IssueSummary>, String>),
    Projects(Result<Vec<ProjectSummary>, String>),
    Transitions(String, Result<Vec<Transition>, String>),
    Transitioned(String, Result<(), String>),
    Comment(String, Result<JiraComment, String>),
    MoreComments(String, Result<Vec<JiraComment>, String>),
}

struct State {
    selected: usize,
    show_history: bool,
    show_backlog: bool,
    jira_tab: bool,
    mode: Mode,
    input: String,
    message: String,
    card: Option<IssueCard>,
    requested_key: Option<String>,
    card_loading: bool,
    card_scroll: u16,
    search_results: Vec<IssueSummary>,
    search_selected: usize,
    search_generation: u64,
    search_token: Arc<AtomicU64>,
    search_loading: bool,
    project_results: Vec<ProjectSummary>,
    project_selected: usize,
    project_loading: bool,
    transition_results: Vec<Transition>,
    transition_selected: usize,
    transition_loading: bool,
    move_selected: usize,
    help_scroll: u16,
    config_path: String,
    task_path: String,
    jira_config: Option<JiraConfig>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            selected: 0,
            show_history: false,
            show_backlog: false,
            jira_tab: false,
            mode: Mode::Normal,
            input: String::new(),
            message: String::new(),
            card: None,
            requested_key: None,
            card_loading: false,
            card_scroll: 0,
            search_results: Vec::new(),
            search_selected: 0,
            search_generation: 0,
            search_token: Arc::new(AtomicU64::new(0)),
            search_loading: false,
            project_results: Vec::new(),
            project_selected: 0,
            project_loading: false,
            transition_results: Vec::new(),
            transition_selected: 0,
            transition_loading: false,
            move_selected: 0,
            help_scroll: 0,
            config_path: String::new(),
            task_path: String::new(),
            jira_config: None,
        }
    }
}

pub fn run(store: &mut Store) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_loop(&mut terminal, store);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &mut Store,
) -> Result<()> {
    let (mut jira, jira_error) = match JiraClient::from_config() {
        Ok(client) => (Some(client), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let (sender, receiver) = mpsc::channel();
    let mut state = State {
        config_path: config_path()?.display().to_string(),
        task_path: store.path().display().to_string(),
        jira_config: jira
            .as_ref()
            .map(|client| client.config().clone())
            .or_else(|| JiraConfig::load().ok()),
        ..State::default()
    };
    let carried = store.carry()?;
    if carried > 0 {
        state.message = format!(
            "Carried {carried} unfinished task{} into today",
            if carried == 1 { "" } else { "s" }
        );
    }

    loop {
        handle_jira_events(&mut state, &receiver, &sender, jira.as_ref());
        let entries = entries(store, state.show_history, state.show_backlog);
        state.selected = state.selected.min(entries.len().saturating_sub(1));
        request_selected_card(&entries, &mut state, jira.as_ref(), &sender);

        terminal.draw(|frame| {
            draw(
                frame,
                &entries,
                &state,
                jira.as_ref(),
                jira_error.as_deref(),
            )
        })?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        match state.mode {
            Mode::Add => handle_add_key(key.code, &mut state, store, false)?,
            Mode::AddBacklog => handle_add_key(key.code, &mut state, store, true)?,
            Mode::Move => handle_move_key(key.code, &entries, &mut state, store)?,
            Mode::Help => handle_help_key(key.code, &mut state),
            Mode::Configuration => handle_configuration_key(key.code, &mut state),
            Mode::Search => handle_search_key(key.code, &mut state, store, jira.as_ref(), &sender)?,
            Mode::Project => {
                handle_project_key(key.code, &mut state, &mut jira)?;
            }
            Mode::Transition => {
                handle_transition_key(key.code, &mut state, jira.as_ref(), &sender);
            }
            Mode::Comment => handle_comment_key(key.code, key.modifiers, &mut state),
            Mode::ConfirmComment => {
                handle_confirmation_key(key.code, &mut state, jira.as_ref(), &sender)
            }
            Mode::Normal => {
                if handle_normal_key(
                    key.code,
                    &entries,
                    &mut state,
                    store,
                    jira.as_ref(),
                    jira_error.as_deref(),
                    &sender,
                )? {
                    return Ok(());
                }
            }
        }
    }
}

fn handle_normal_key(
    key: KeyCode,
    entries: &[Entry],
    state: &mut State,
    store: &mut Store,
    jira: Option<&JiraClient>,
    jira_error: Option<&str>,
    sender: &Sender<JiraEvent>,
) -> Result<bool> {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Char('?') => {
            state.mode = Mode::Help;
            state.help_scroll = 0;
        }
        KeyCode::Char('g') => state.mode = Mode::Configuration,
        KeyCode::Up | KeyCode::Char('k') => {
            state.selected = state.selected.saturating_sub(1);
            state.card_scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.selected = (state.selected + 1).min(entries.len().saturating_sub(1));
            state.card_scroll = 0;
        }
        KeyCode::Tab => state.jira_tab = !state.jira_tab,
        KeyCode::PageDown => state.card_scroll = state.card_scroll.saturating_add(8),
        KeyCode::PageUp => state.card_scroll = state.card_scroll.saturating_sub(8),
        KeyCode::Char('a') => {
            state.mode = Mode::Add;
            state.input.clear();
        }
        KeyCode::Char('B') => {
            state.mode = Mode::AddBacklog;
            state.input.clear();
        }
        KeyCode::Char('b') => {
            state.show_backlog = !state.show_backlog;
            state.message = if state.show_backlog {
                "Backlog visible".to_owned()
            } else {
                "Backlog hidden".to_owned()
            };
        }
        KeyCode::Char('p') if !entries.is_empty() => {
            let entry = &entries[state.selected];
            if entry.date == BACKLOG {
                store.promote_backlog(&entry.task.id)?;
                state.message = "Moved backlog task into today".to_owned();
            } else {
                state.message = "Select a backlog task to promote".to_owned();
            }
        }
        KeyCode::Char('M') if !entries.is_empty() => {
            state.mode = Mode::Move;
            state.move_selected = 0;
        }
        KeyCode::Char(' ') | KeyCode::Char('d') if !entries.is_empty() => {
            let task = &entries[state.selected].task;
            let completed = !task.completed;
            store.set_completed(&task.id, false, completed)?;
            state.message = if completed { "Completed" } else { "Reopened" }.to_owned();
        }
        KeyCode::Char('h') => {
            state.show_history = !state.show_history;
            state.selected = 0;
            state.message.clear();
        }
        KeyCode::Char('C') => {
            let count = store.carry()?;
            state.show_history = false;
            state.selected = 0;
            state.message = format!("Carried {count} task{}", if count == 1 { "" } else { "s" });
        }
        KeyCode::Char('l') if !entries.is_empty() => {
            let Some(client) = jira else {
                state.message = jira_error.unwrap_or("Jira is unavailable").to_owned();
                return Ok(false);
            };
            if client.config().project.is_none() {
                open_project_selector(state, client, sender);
                return Ok(false);
            }
            state.mode = Mode::Search;
            state.input.clear();
            state.search_results.clear();
            state.search_selected = 0;
            request_search(state, client, sender, false);
        }
        KeyCode::Char('s') => {
            let Some(client) = jira else {
                state.message = jira_error.unwrap_or("Jira is unavailable").to_owned();
                return Ok(false);
            };
            open_project_selector(state, client, sender);
        }
        KeyCode::Char('u') if !entries.is_empty() => {
            store.unlink_jira(&entries[state.selected].task.id)?;
            state.message = "Jira link removed".to_owned();
        }
        KeyCode::Char('c') => {
            if state.card.is_some() && jira.is_some() {
                state.mode = Mode::Comment;
                state.input.clear();
            } else {
                state.message = "Select a linked Jira task first".to_owned();
            }
        }
        KeyCode::Char('t') => {
            if let (Some(client), Some(card)) = (jira, state.card.as_ref()) {
                let key = card.key.clone();
                open_transition_selector(state, client, &key, sender);
            } else {
                state.message = "Select a linked Jira task first".to_owned();
            }
        }
        KeyCode::Char('r') => {
            state.requested_key = None;
            state.card_loading = false;
        }
        KeyCode::Char('m') => {
            if let (Some(client), Some(card)) = (jira, state.card.as_ref()) {
                let client = client.clone();
                let key = card.key.clone();
                let event_key = key.clone();
                let sender = sender.clone();
                thread::spawn(move || {
                    let result = client
                        .issue_comments(&key, 50)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(JiraEvent::MoreComments(event_key, result));
                });
                state.message = "Loading more comments...".to_owned();
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_add_key(key: KeyCode, state: &mut State, store: &mut Store, backlog: bool) -> Result<()> {
    match key {
        KeyCode::Enter => {
            if !state.input.trim().is_empty() {
                if backlog {
                    store.add_backlog(&state.input)?;
                    state.message = "Backlog task added".to_owned();
                    state.show_backlog = true;
                } else {
                    store.add(&state.input)?;
                    state.message = "Task added".to_owned();
                    state.show_history = false;
                }
            }
            state.input.clear();
            state.mode = Mode::Normal;
        }
        KeyCode::Esc => {
            state.input.clear();
            state.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            state.input.pop();
        }
        KeyCode::Char(character) => state.input.push(character),
        _ => {}
    }
    Ok(())
}

fn handle_move_key(
    key: KeyCode,
    entries: &[Entry],
    state: &mut State,
    store: &mut Store,
) -> Result<()> {
    match key {
        KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Up | KeyCode::Char('k') => {
            state.move_selected = state.move_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.move_selected = (state.move_selected + 1).min(2);
        }
        KeyCode::Enter => {
            if let Some(entry) = entries.get(state.selected) {
                let destination = match state.move_selected {
                    0 => "today",
                    1 => "tomorrow",
                    _ => "backlog",
                };
                store.move_task(&entry.task.id, destination, false)?;
                state.message = format!("Moved task to {destination}");
                state.mode = Mode::Normal;
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_help_key(key: KeyCode, state: &mut State) {
    match key {
        KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Up | KeyCode::Char('k') => {
            state.help_scroll = state.help_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.help_scroll = state.help_scroll.saturating_add(1);
        }
        KeyCode::PageUp => state.help_scroll = state.help_scroll.saturating_sub(8),
        KeyCode::PageDown => state.help_scroll = state.help_scroll.saturating_add(8),
        _ => {}
    }
}

fn handle_configuration_key(key: KeyCode, state: &mut State) {
    if matches!(key, KeyCode::Char('g' | 'q') | KeyCode::Esc) {
        state.mode = Mode::Normal;
    }
}

fn handle_search_key(
    key: KeyCode,
    state: &mut State,
    store: &mut Store,
    jira: Option<&JiraClient>,
    sender: &Sender<JiraEvent>,
) -> Result<()> {
    match key {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            state.input.clear();
        }
        KeyCode::Up => state.search_selected = state.search_selected.saturating_sub(1),
        KeyCode::Down => {
            state.search_selected =
                (state.search_selected + 1).min(state.search_results.len().saturating_sub(1));
        }
        KeyCode::Enter if !state.search_results.is_empty() => {
            let entries = entries(store, state.show_history, state.show_backlog);
            if let Some(entry) = entries.get(state.selected) {
                let issue = &state.search_results[state.search_selected];
                store.link_jira(&entry.task.id, &issue.key)?;
                state.message = format!("Linked to {}", issue.key);
                state.mode = Mode::Normal;
                state.input.clear();
            }
        }
        KeyCode::Char('i') if !state.search_results.is_empty() => {
            let issue = &state.search_results[state.search_selected];
            store.add_linked(&issue.summary, Some(&issue.key))?;
            state.message = format!("Imported {}", issue.key);
            state.show_history = false;
            state.mode = Mode::Normal;
            state.input.clear();
        }
        KeyCode::Backspace => {
            state.input.pop();
            if let Some(client) = jira {
                request_search(state, client, sender, true);
            }
        }
        KeyCode::Char(character) => {
            state.input.push(character);
            if let Some(client) = jira {
                request_search(state, client, sender, true);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_project_key(
    key: KeyCode,
    state: &mut State,
    jira: &mut Option<JiraClient>,
) -> Result<()> {
    match key {
        KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Up | KeyCode::Char('k') => {
            state.project_selected = state.project_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.project_selected =
                (state.project_selected + 1).min(state.project_results.len().saturating_sub(1));
        }
        KeyCode::Enter if !state.project_results.is_empty() => {
            let project = &state.project_results[state.project_selected];
            if let Some(client) = jira.as_mut() {
                client.set_project(&project.key)?;
                state.jira_config = Some(client.config().clone());
                state.message = format!("Jira project set to {} ({})", project.key, project.name);
                state.mode = Mode::Normal;
                state.search_results.clear();
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_transition_key(
    key: KeyCode,
    state: &mut State,
    jira: Option<&JiraClient>,
    sender: &Sender<JiraEvent>,
) {
    match key {
        KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Up | KeyCode::Char('k') => {
            state.transition_selected = state.transition_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.transition_selected = (state.transition_selected + 1)
                .min(state.transition_results.len().saturating_sub(1));
        }
        KeyCode::Enter if !state.transition_results.is_empty() => {
            if let (Some(client), Some(card)) = (jira, state.card.as_ref()) {
                let transition = &state.transition_results[state.transition_selected];
                let client = client.clone();
                let key = card.key.clone();
                let event_key = key.clone();
                let transition_id = transition.id.clone();
                let transition_name = transition.name.clone();
                let sender = sender.clone();
                thread::spawn(move || {
                    let result = client
                        .transition_issue(&key, &transition_id)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(JiraEvent::Transitioned(event_key, result));
                });
                state.message = format!("Applying Jira transition: {transition_name}...");
                state.mode = Mode::Normal;
            }
        }
        _ => {}
    }
}

fn handle_comment_key(key: KeyCode, modifiers: KeyModifiers, state: &mut State) {
    match key {
        KeyCode::Esc => {
            state.input.clear();
            state.mode = Mode::Normal;
        }
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            if !state.input.trim().is_empty() {
                state.mode = Mode::ConfirmComment;
            }
        }
        KeyCode::Enter => state.input.push('\n'),
        KeyCode::Backspace => {
            state.input.pop();
        }
        KeyCode::Char(character) => state.input.push(character),
        _ => {}
    }
}

fn handle_confirmation_key(
    key: KeyCode,
    state: &mut State,
    jira: Option<&JiraClient>,
    sender: &Sender<JiraEvent>,
) {
    match key {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let (Some(client), Some(card)) = (jira, state.card.as_ref()) {
                let client = client.clone();
                let key = card.key.clone();
                let event_key = key.clone();
                let text = state.input.clone();
                let sender = sender.clone();
                thread::spawn(move || {
                    let result = client
                        .post_comment(&key, &text)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(JiraEvent::Comment(event_key, result));
                });
                state.message = "Posting Jira comment...".to_owned();
                state.input.clear();
                state.mode = Mode::Normal;
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => state.mode = Mode::Comment,
        _ => {}
    }
}

fn request_selected_card(
    entries: &[Entry],
    state: &mut State,
    jira: Option<&JiraClient>,
    sender: &Sender<JiraEvent>,
) {
    let key = entries
        .get(state.selected)
        .and_then(|entry| entry.task.jira.clone());
    if key == state.requested_key {
        return;
    }
    state.requested_key = key.clone();
    state.card = None;
    state.card_scroll = 0;
    state.card_loading = false;
    let (Some(key), Some(client)) = (key, jira) else {
        return;
    };
    state.card_loading = true;
    let event_key = key.clone();
    let client = client.clone();
    let sender = sender.clone();
    thread::spawn(move || {
        let result = client.issue_card(&key).map_err(|error| error.to_string());
        let _ = sender.send(JiraEvent::Card(event_key, Box::new(result)));
    });
}

fn request_search(
    state: &mut State,
    client: &JiraClient,
    sender: &Sender<JiraEvent>,
    debounce: bool,
) {
    state.search_generation += 1;
    let generation = state.search_generation;
    state.search_token.store(generation, Ordering::Relaxed);
    let search_token = Arc::clone(&state.search_token);
    let query = state.input.clone();
    let client = client.clone();
    let sender = sender.clone();
    state.search_loading = true;
    thread::spawn(move || {
        if debounce {
            thread::sleep(Duration::from_millis(300));
        }
        if search_token.load(Ordering::Relaxed) != generation {
            return;
        }
        let result = client
            .search_issues(&query)
            .map_err(|error| error.to_string());
        let _ = sender.send(JiraEvent::Search(generation, result));
    });
}

fn open_project_selector(state: &mut State, client: &JiraClient, sender: &Sender<JiraEvent>) {
    state.mode = Mode::Project;
    state.project_results.clear();
    state.project_selected = 0;
    state.project_loading = true;
    let client = client.clone();
    let sender = sender.clone();
    thread::spawn(move || {
        let result = client.projects().map_err(|error| error.to_string());
        let _ = sender.send(JiraEvent::Projects(result));
    });
}

fn open_transition_selector(
    state: &mut State,
    client: &JiraClient,
    key: &str,
    sender: &Sender<JiraEvent>,
) {
    state.mode = Mode::Transition;
    state.transition_results.clear();
    state.transition_selected = 0;
    state.transition_loading = true;
    let client = client.clone();
    let key = key.to_owned();
    let event_key = key.clone();
    let sender = sender.clone();
    thread::spawn(move || {
        let result = client.transitions(&key).map_err(|error| error.to_string());
        let _ = sender.send(JiraEvent::Transitions(event_key, result));
    });
}

fn handle_jira_events(
    state: &mut State,
    receiver: &Receiver<JiraEvent>,
    sender: &Sender<JiraEvent>,
    jira: Option<&JiraClient>,
) {
    while let Ok(event) = receiver.try_recv() {
        match event {
            JiraEvent::Card(key, result) if state.requested_key.as_deref() == Some(&key) => {
                state.card_loading = false;
                match *result {
                    Ok(card) => state.card = Some(card),
                    Err(error) => state.message = error,
                }
            }
            JiraEvent::Search(generation, result) if generation == state.search_generation => {
                state.search_loading = false;
                match result {
                    Ok(issues) => {
                        state.search_results = issues;
                        state.search_selected = 0;
                    }
                    Err(error) => state.message = error,
                }
            }
            JiraEvent::Projects(result) => {
                state.project_loading = false;
                match result {
                    Ok(projects) => {
                        state.project_results = projects;
                        state.project_selected = jira
                            .and_then(|client| client.config().project.as_deref())
                            .and_then(|selected| {
                                state
                                    .project_results
                                    .iter()
                                    .position(|project| project.key == selected)
                            })
                            .unwrap_or(0);
                    }
                    Err(error) => state.message = error,
                }
            }
            JiraEvent::Transitions(key, result)
                if state.card.as_ref().is_some_and(|card| card.key == key) =>
            {
                state.transition_loading = false;
                match result {
                    Ok(transitions) => {
                        state.transition_results = transitions;
                        state.transition_selected = 0;
                    }
                    Err(error) => state.message = error,
                }
            }
            JiraEvent::Transitioned(key, result) => match result {
                Ok(()) => {
                    state.message = format!("Updated Jira status for {key}");
                    state.requested_key = None;
                    state.card_loading = false;
                }
                Err(error) => state.message = error,
            },
            JiraEvent::Comment(key, result) => match result {
                Ok(_) => {
                    state.message = format!("Comment posted to {key}");
                    state.requested_key = None;
                    state.card_loading = false;
                }
                Err(error) => state.message = error,
            },
            JiraEvent::MoreComments(key, result)
                if state.card.as_ref().is_some_and(|card| card.key == key) =>
            {
                match result {
                    Ok(comments) => {
                        if let Some(card) = state.card.as_mut() {
                            card.comments = comments;
                        }
                        state.message = "Loaded comment history".to_owned();
                    }
                    Err(error) => state.message = error,
                }
            }
            _ => {}
        }
    }

    if state.requested_key.is_none() && jira.is_some() && !state.card_loading {
        let _ = sender;
    }
}

fn entries(store: &Store, show_history: bool, show_backlog: bool) -> Vec<Entry> {
    if show_history {
        let all = store.entries_all().into_iter().collect::<Vec<_>>();
        let mut entries = all
            .iter()
            .filter(|entry| entry.date != BACKLOG)
            .cloned()
            .collect::<Vec<_>>();
        if show_backlog {
            entries.extend(all.into_iter().filter(|entry| entry.date == BACKLOG));
        }
        entries
    } else {
        let mut entries = store.entries_today();
        if show_backlog {
            entries.extend(store.entries_backlog());
        }
        entries
    }
}

fn draw(
    frame: &mut Frame,
    entries: &[Entry],
    state: &State,
    jira: Option<&JiraClient>,
    jira_error: Option<&str>,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    draw_header(frame, areas[0], state);

    if areas[1].width >= 100 {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(areas[1]);
        draw_tasks(frame, panes[0], entries, state);
        draw_jira_card(frame, panes[1], state, jira, jira_error);
    } else if state.jira_tab {
        draw_jira_card(frame, areas[1], state, jira, jira_error);
    } else {
        draw_tasks(frame, areas[1], entries, state);
    }

    frame.render_widget(
        Paragraph::new(state.message.as_str()).style(Style::default().add_modifier(Modifier::DIM)),
        areas[2],
    );

    match state.mode {
        Mode::Add => draw_single_input(frame, "New task", &state.input),
        Mode::AddBacklog => draw_single_input(frame, "New backlog task", &state.input),
        Mode::Move => draw_move_selector(frame, state),
        Mode::Help => draw_help(frame, state),
        Mode::Configuration => draw_configuration(frame, state, jira_error),
        Mode::Search => draw_search(frame, state),
        Mode::Project => draw_project_selector(frame, state),
        Mode::Transition => draw_transition_selector(frame, state),
        Mode::Comment => draw_comment_editor(frame, state),
        Mode::ConfirmComment => draw_comment_confirmation(frame, state),
        Mode::Normal => {}
    }
}

fn draw_header(frame: &mut Frame, area: Rect, state: &State) {
    let title = if state.show_history {
        "ZLS / HISTORY".to_owned()
    } else {
        format!("ZLS / TODAY / {}", today())
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Line::styled(compact_hint(), Style::default().add_modifier(Modifier::DIM)),
        ]),
        area,
    );
}

fn draw_tasks(frame: &mut Frame, area: Rect, entries: &[Entry], state: &State) {
    let regular = entries
        .iter()
        .filter(|entry| entry.date != BACKLOG)
        .collect::<Vec<_>>();
    let backlog = entries
        .iter()
        .filter(|entry| entry.date == BACKLOG)
        .collect::<Vec<_>>();
    let selected_id = entries
        .get(state.selected)
        .map(|entry| entry.task.id.as_str());
    let regular_selected =
        selected_id.and_then(|id| regular.iter().position(|entry| entry.task.id == id));
    let backlog_selected =
        selected_id.and_then(|id| backlog.iter().position(|entry| entry.task.id == id));

    if state.show_backlog {
        let backlog_height = (backlog.len() as u16 + 2)
            .max(3)
            .min(area.height.saturating_sub(3).max(1));
        let panes = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(backlog_height)])
            .split(area);
        draw_task_list(
            frame,
            panes[0],
            &regular,
            regular_selected,
            " TASKS ",
            "Nothing scheduled for today.",
            state.show_history,
        );
        draw_task_list(
            frame,
            panes[1],
            &backlog,
            backlog_selected,
            " BACKLOG ",
            "Backlog is empty. Press B to add.",
            false,
        );
    } else {
        draw_task_list(
            frame,
            area,
            &regular,
            regular_selected,
            " TASKS ",
            "Nothing here. Press a to add a task.",
            state.show_history,
        );
    }
}

fn draw_task_list(
    frame: &mut Frame,
    area: Rect,
    entries: &[&Entry],
    selected: Option<usize>,
    title: &str,
    empty_message: &str,
    show_dates: bool,
) {
    let items = if entries.is_empty() {
        vec![ListItem::new(empty_message)]
    } else {
        entries
            .iter()
            .map(|entry| {
                let checked = if entry.task.completed { 'x' } else { ' ' };
                let jira = entry
                    .task
                    .jira
                    .as_ref()
                    .map_or_else(String::new, |key| format!(" [{key}]"));
                let date = if show_dates {
                    format!("  {}", entry.date)
                } else {
                    String::new()
                };
                ListItem::new(format!("[{checked}] {}{jira}{date}", entry.task.text))
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::RIGHT)
                .title(title),
        )
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_jira_card(
    frame: &mut Frame,
    area: Rect,
    state: &State,
    jira: Option<&JiraClient>,
    jira_error: Option<&str>,
) {
    let project = jira.and_then(|client| client.config().project.as_deref());
    let title = project.map_or_else(|| " JIRA ".to_owned(), |key| format!(" JIRA / {key} "));
    let block = Block::default().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(error) = jira_error {
        frame.render_widget(
            Paragraph::new(format!(
                "Jira unavailable\n\n{error}\n\nRun `zls jira auth` or provide ZLS_JIRA_TOKEN to tmux."
            ))
            .wrap(Wrap { trim: false })
            .alignment(Alignment::Center),
            inner,
        );
        return;
    }
    if project.is_none() && state.card.is_none() {
        frame.render_widget(
            Paragraph::new("No Jira project selected.\n\nPress s to choose your project.")
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }
    if state.card_loading {
        frame.render_widget(Paragraph::new("Loading Jira card..."), inner);
        return;
    }
    let Some(card) = state.card.as_ref() else {
        frame.render_widget(
            Paragraph::new("Link a task with l to see its Jira card here.")
                .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                &card.key,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(format!("[{}]", card.status), status_style(&card.status)),
            if card.stale {
                Span::styled("  CACHED", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            },
        ]),
        Line::styled(&card.summary, Style::default().add_modifier(Modifier::BOLD)),
        Line::raw(""),
        section_separator("DETAILS", inner.width),
    ];
    lines.push(detail_pair(
        "Type",
        card.issue_type.as_deref().unwrap_or("Unknown"),
        "Priority",
        card.priority.as_deref().unwrap_or("None"),
    ));
    lines.push(detail_pair(
        "Assignee",
        card.assignee.as_deref().unwrap_or("Unassigned"),
        "Reporter",
        card.reporter.as_deref().unwrap_or("Unknown"),
    ));
    lines.push(detail_pair(
        "Creator",
        card.creator.as_deref().unwrap_or("Unknown"),
        "Resolution",
        card.resolution.as_deref().unwrap_or("Unresolved"),
    ));
    if let Some(parent) = card.parent.as_ref() {
        lines.push(detail_line(
            "Parent",
            &format!("{} [{}] {}", parent.key, parent.status, parent.summary),
        ));
    }
    if !card.sprints.is_empty() {
        lines.push(detail_line("Sprint", &card.sprints.join(", ")));
    }
    if !card.labels.is_empty() {
        lines.push(detail_line("Labels", &card.labels.join(", ")));
    }
    if !card.components.is_empty() {
        lines.push(detail_line("Components", &card.components.join(", ")));
    }
    if !card.fix_versions.is_empty() {
        lines.push(detail_line("Fix versions", &card.fix_versions.join(", ")));
    }
    let created = card
        .created
        .as_deref()
        .map(format_jira_datetime)
        .unwrap_or_else(|| "Unknown".to_owned());
    let updated = card
        .updated
        .as_deref()
        .map(format_jira_datetime)
        .unwrap_or_else(|| "Unknown".to_owned());
    lines.push(detail_pair("Created", &created, "Updated", &updated));
    if let Some(due_date) = card.due_date.as_deref() {
        lines.push(detail_line("Due", due_date));
    }

    lines.push(Line::raw(""));
    lines.push(section_separator("DESCRIPTION", inner.width));
    append_text(&mut lines, &card.description, "No description.");

    if !card.subtasks.is_empty() || !card.links.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section_separator("RELATED WORK", inner.width));
        for subtask in &card.subtasks {
            lines.push(Line::raw(format!(
                "  {} [{}] {}",
                subtask.key, subtask.status, subtask.summary
            )));
        }
        for link in &card.links {
            lines.push(Line::raw(format!(
                "  {} {} [{}] {}",
                link.relationship, link.key, link.status, link.summary
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(section_separator(
        &format!("COMMENTS ({})", card.comments.len()),
        inner.width,
    ));
    if card.comments.is_empty() {
        lines.push(Line::raw("No comments."));
    }
    for comment in &card.comments {
        lines.push(Line::styled(
            format!(
                "{}  /  {}",
                comment.author,
                format_jira_datetime(&comment.created)
            ),
            Style::default().fg(Color::Cyan),
        ));
        append_text(&mut lines, &comment.body, "");
        lines.push(Line::raw(""));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((state.card_scroll, 0)),
        inner,
    );
}

fn section_separator(title: &str, width: u16) -> Line<'static> {
    let prefix = format!("-- {title} ");
    let remaining = usize::from(width).saturating_sub(prefix.chars().count() + 1);
    Line::styled(
        format!("{prefix}{}", "-".repeat(remaining)),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_owned()),
    ])
}

fn detail_pair(left_label: &str, left: &str, right_label: &str, right: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{left_label:<11}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(format!("{left:<22}")),
        Span::styled(
            format!("{right_label:<11}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(right.to_owned()),
    ])
}

fn status_style(status: &str) -> Style {
    let status = status.to_ascii_lowercase();
    let color =
        if status.contains("done") || status.contains("closed") || status.contains("resolved") {
            Color::Green
        } else if status.contains("progress") || status.contains("review") {
            Color::Yellow
        } else if status.contains("block") {
            Color::Red
        } else {
            Color::Cyan
        };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn append_text<'a>(lines: &mut Vec<Line<'a>>, text: &'a str, fallback: &'a str) {
    let text = if text.trim().is_empty() {
        fallback
    } else {
        text
    };
    lines.extend(text.lines().map(Line::raw));
}

fn draw_single_input(frame: &mut Frame, title: &str, input: &str) {
    let area = centered(frame.area(), 70, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(input).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
    frame.set_cursor_position((area.x + input.chars().count() as u16 + 1, area.y + 1));
}

fn draw_search(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 80, 70);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" LINK JIRA / Enter link / i import / Esc cancel ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);
    let title = if state.search_loading {
        "Search Jira (loading...)"
    } else {
        "Search Jira"
    };
    frame.render_widget(
        Paragraph::new(state.input.as_str())
            .block(Block::default().borders(Borders::BOTTOM).title(title)),
        rows[0],
    );
    let items = state
        .search_results
        .iter()
        .map(|issue| {
            ListItem::new(vec![
                Line::styled(
                    format!("{}  [{}]", issue.key, issue.status),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(&issue.summary),
            ])
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default()
        .with_selected((!state.search_results.is_empty()).then_some(state.search_selected));
    frame.render_stateful_widget(list, rows[1], &mut list_state);
    frame.set_cursor_position((
        rows[0].x + state.input.chars().count() as u16,
        rows[0].y + 1,
    ));
}

fn draw_move_selector(frame: &mut Frame, state: &State) {
    let area = centered_fixed(frame.area(), 45, 5);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" MOVE TASK / Enter select / Esc cancel ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let options = [
        format!("Today     {}", today()),
        format!("Tomorrow  {}", tomorrow()),
        "Backlog    no scheduled date".to_owned(),
    ];
    let list = List::new(options.into_iter().map(ListItem::new).collect::<Vec<_>>())
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default().with_selected(Some(state.move_selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn draw_help(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 74, 86);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" KEYBINDINGS / j-k scroll / ? or Esc close ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    for section in [
        Section::Tasks,
        Section::Jira,
        Section::Configuration,
        Section::Navigation,
        Section::Dialogs,
    ] {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            section.title(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        for binding in KEYBINDINGS
            .iter()
            .filter(|binding| binding.section == section)
        {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<16}", binding.keys),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(binding.description),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((state.help_scroll, 0)),
        inner,
    );
}

fn draw_configuration(frame: &mut Frame, state: &State, jira_error: Option<&str>) {
    let area = centered(frame.area(), 70, 62);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" CONFIGURATION / g, q, or Esc close ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        section_separator("ZLS", inner.width),
        detail_line("Config file", &state.config_path),
        detail_line("Tasks file", &state.task_path),
        Line::raw(""),
        section_separator("JIRA", inner.width),
    ];
    if let Some(config) = state.jira_config.as_ref() {
        lines.extend([
            detail_line("Site", &config.site),
            detail_line("Email", &config.email),
            detail_line("Cloud ID", &config.cloud_id),
            detail_line(
                "Project",
                config.project.as_deref().unwrap_or("Not selected"),
            ),
            detail_line(
                "Auth",
                if jira_error.is_none() {
                    "Available"
                } else {
                    "Credential unavailable"
                },
            ),
            detail_line("API token", "Hidden"),
        ]);
        if let Some(error) = jira_error {
            lines.push(Line::raw(""));
            lines.push(Line::styled(error, Style::default().fg(Color::Yellow)));
        }
    } else {
        lines.push(Line::raw("Jira is not configured."));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_project_selector(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 65, 65);
    frame.render_widget(Clear, area);
    let title = if state.project_loading {
        " JIRA PROJECT / loading... / Esc cancel "
    } else {
        " JIRA PROJECT / Enter select / Esc cancel "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = state
        .project_results
        .iter()
        .map(|project| {
            ListItem::new(vec![
                Line::styled(
                    &project.key,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(&project.name),
            ])
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default()
        .with_selected((!state.project_results.is_empty()).then_some(state.project_selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn draw_transition_selector(frame: &mut Frame, state: &State) {
    let height = (state.transition_results.len() as u16 + 2).clamp(5, 14);
    let area = centered_fixed(frame.area(), 48, height);
    frame.render_widget(Clear, area);
    let title = if state.transition_loading {
        " CHANGE STATUS / loading... / Esc cancel "
    } else {
        " CHANGE STATUS / Enter apply / Esc cancel "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = state
        .transition_results
        .iter()
        .map(|transition| {
            if transition.name.eq_ignore_ascii_case(&transition.to_status) {
                ListItem::new(Line::styled(
                    &transition.to_status,
                    status_style(&transition.to_status),
                ))
            } else {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        &transition.name,
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  ->  "),
                    Span::styled(&transition.to_status, status_style(&transition.to_status)),
                ]))
            }
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default()
        .with_selected((!state.transition_results.is_empty()).then_some(state.transition_selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn draw_comment_editor(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 75, 60);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(state.input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" JIRA COMMENT / Ctrl-S review / Esc cancel "),
            ),
        area,
    );
    let line_count = state.input.lines().count().max(1) as u16;
    let column = state
        .input
        .lines()
        .last()
        .map_or(0, |line| line.chars().count()) as u16;
    frame.set_cursor_position((
        area.x + column + 1,
        area.y + line_count.min(area.height - 2),
    ));
}

fn draw_comment_confirmation(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 65, 45);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(state.input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" POST THIS COMMENT? / y yes / n edit "),
            ),
        area,
    );
}

fn centered(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn centered_fixed(area: Rect, width_percent: u16, height: u16) -> Rect {
    let width = area.width.saturating_mul(width_percent) / 100;
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}
