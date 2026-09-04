use std::{
    io::{self, stdout},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use chrono::NaiveDate;
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
    config::{AppConfig, DEFAULT_ACCENT, config_path, parse_hex_color},
    docs,
    jira::{
        IssueCard, IssueSummary, JiraClient, JiraComment, JiraConfig, ProjectSummary, Transition,
        format_jira_datetime,
    },
    keybindings::{KEYBINDINGS, Section, compact_hint},
    store::{BACKLOG, Entry, Store, WeekReport, today, tomorrow},
};

struct AccentPreset {
    name: &'static str,
    hex: &'static str,
}

const ACCENT_PRESETS: &[AccentPreset] = &[
    AccentPreset {
        name: "Cyan",
        hex: "#00FFFF",
    },
    AccentPreset {
        name: "Ocean",
        hex: "#5FAFFF",
    },
    AccentPreset {
        name: "Sky",
        hex: "#89B4FA",
    },
    AccentPreset {
        name: "Periwinkle",
        hex: "#8CAAEE",
    },
    AccentPreset {
        name: "Teal",
        hex: "#94E2D5",
    },
    AccentPreset {
        name: "Mint",
        hex: "#A6E3A1",
    },
    AccentPreset {
        name: "Sage",
        hex: "#B8C0A0",
    },
    AccentPreset {
        name: "Lavender",
        hex: "#CBA6F7",
    },
    AccentPreset {
        name: "Lilac",
        hex: "#B4BEFE",
    },
    AccentPreset {
        name: "Mauve",
        hex: "#C6A0F6",
    },
    AccentPreset {
        name: "Pink",
        hex: "#F5C2E7",
    },
    AccentPreset {
        name: "Rose",
        hex: "#F38BA8",
    },
    AccentPreset {
        name: "Coral",
        hex: "#EA999C",
    },
    AccentPreset {
        name: "Peach",
        hex: "#FAB387",
    },
    AccentPreset {
        name: "Amber",
        hex: "#F9E2AF",
    },
    AccentPreset {
        name: "Silver",
        hex: "#BAC2DE",
    },
];

enum Mode {
    Normal,
    Add,
    AddBacklog,
    Move,
    Help,
    Configuration,
    AccentPalette,
    AccentCustom,
    TaskSearch,
    Search,
    Project,
    Transition,
    Comment,
    ConfirmComment,
}

enum JiraEvent {
    Card(String, Box<Result<IssueCard, String>>),
    Search(u64, Result<Vec<IssueSummary>, String>),
    Projects(u64, Result<Vec<ProjectSummary>, String>),
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
    project_generation: u64,
    project_error: bool,
    transition_results: Vec<Transition>,
    transition_selected: usize,
    transition_loading: bool,
    move_selected: usize,
    help_scroll: u16,
    config_selected: usize,
    config_path: String,
    task_path: String,
    jira_config: Option<JiraConfig>,
    task_query: String,
    docs_path: String,
    edit_document: bool,
    weekly: bool,
    weeks_ago: u32,
    accent: Color,
    accent_hex: String,
    accent_original: String,
    accent_selected: Option<usize>,
    accent_columns: usize,
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
            project_generation: 0,
            project_error: false,
            transition_results: Vec::new(),
            transition_selected: 0,
            transition_loading: false,
            move_selected: 0,
            help_scroll: 0,
            config_selected: 0,
            config_path: String::new(),
            task_path: String::new(),
            jira_config: None,
            task_query: String::new(),
            docs_path: String::new(),
            edit_document: false,
            weekly: false,
            weeks_ago: 0,
            accent: Color::Rgb(0, 255, 255),
            accent_hex: DEFAULT_ACCENT.to_owned(),
            accent_original: DEFAULT_ACCENT.to_owned(),
            accent_selected: Some(0),
            accent_columns: 4,
        }
    }
}

pub fn run(store: &mut Store, docs_path: &Path, config: &mut AppConfig) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_loop(&mut terminal, store, docs_path, config);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &mut Store,
    docs_path: &Path,
    config: &mut AppConfig,
) -> Result<()> {
    let (mut jira, jira_error) = match JiraClient::from_config() {
        Ok(client) => (Some(client), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let (sender, receiver) = mpsc::channel();
    let mut state = State {
        config_path: config_path()?.display().to_string(),
        task_path: store.path().display().to_string(),
        docs_path: docs_path.display().to_string(),
        accent: accent_color(&config.ui.accent)?,
        accent_hex: config.ui.accent.clone(),
        accent_original: config.ui.accent.clone(),
        accent_selected: ACCENT_PRESETS
            .iter()
            .position(|preset| preset.hex.eq_ignore_ascii_case(&config.ui.accent)),
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
        state.accent_columns = if terminal.size()?.width >= 92 { 4 } else { 2 };
        handle_jira_events(&mut state, &receiver, &sender, jira.as_ref());
        let week_report = state
            .weekly
            .then(|| store.week_report_weeks_ago(state.weeks_ago))
            .transpose()?;
        let entries = week_report.as_ref().map_or_else(
            || {
                entries(
                    store,
                    state.show_history,
                    state.show_backlog,
                    &state.task_query,
                )
            },
            WeekReport::entries,
        );
        state.selected = state.selected.min(entries.len().saturating_sub(1));
        request_selected_card(&entries, &mut state, jira.as_ref(), &sender);

        terminal.draw(|frame| {
            draw(
                frame,
                &entries,
                &state,
                jira.as_ref(),
                jira_error.as_deref(),
                week_report.as_ref(),
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
            Mode::Configuration => handle_configuration_key(
                key.code,
                &mut state,
                jira.as_ref(),
                jira_error.as_deref(),
                &sender,
            ),
            Mode::AccentPalette => {
                handle_accent_palette_key(key.code, &mut state, config)?;
            }
            Mode::AccentCustom => handle_accent_custom_key(key.code, &mut state, config)?,
            Mode::TaskSearch => handle_task_search_key(key.code, &mut state),
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
            Mode::Normal if state.weekly => {
                if handle_week_key(key.code, &entries, &mut state)? {
                    return Ok(());
                }
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
        if state.edit_document {
            state.edit_document = false;
            edit_selected_document(terminal, &entries, &mut state, store, docs_path)?;
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
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Esc if !state.task_query.is_empty() => {
            state.task_query.clear();
            state.selected = 0;
        }
        KeyCode::Esc => return Ok(true),
        KeyCode::Char('?') => {
            state.mode = Mode::Help;
            state.help_scroll = 0;
        }
        KeyCode::Char('g') => {
            state.config_selected = 0;
            state.mode = Mode::Configuration;
        }
        KeyCode::Char('W') => {
            state.weekly = true;
            state.weeks_ago = 0;
            state.selected = 0;
            state.jira_tab = false;
            state.task_query.clear();
        }
        KeyCode::Char('/') => {
            state.mode = Mode::TaskSearch;
            state.selected = 0;
        }
        KeyCode::Char('e') if !entries.is_empty() => state.edit_document = true,
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
                state.message = "Select JIRA.Project in Configuration first (g)".to_owned();
                return Ok(false);
            }
            state.mode = Mode::Search;
            state.input.clear();
            state.search_results.clear();
            state.search_selected = 0;
            request_search(state, client, sender, false);
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

fn handle_week_key(key: KeyCode, entries: &[Entry], state: &mut State) -> Result<bool> {
    match key {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Esc | KeyCode::Char('W') => {
            state.weekly = false;
            state.selected = 0;
            state.jira_tab = false;
        }
        KeyCode::Char('?') => {
            state.mode = Mode::Help;
            state.help_scroll = 0;
        }
        KeyCode::Left => {
            state.weeks_ago = state.weeks_ago.saturating_add(1);
            state.selected = 0;
        }
        KeyCode::Right => {
            state.weeks_ago = state.weeks_ago.saturating_sub(1);
            state.selected = 0;
        }
        KeyCode::Char('0') => {
            state.weeks_ago = 0;
            state.selected = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.selected = state.selected.saturating_sub(1);
            state.card_scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.selected = (state.selected + 1).min(entries.len().saturating_sub(1));
            state.card_scroll = 0;
        }
        KeyCode::Char('e') if !entries.is_empty() => state.edit_document = true,
        KeyCode::Enter if !entries.is_empty() => state.jira_tab = true,
        KeyCode::Tab => state.jira_tab = !state.jira_tab,
        KeyCode::PageDown => state.card_scroll = state.card_scroll.saturating_add(8),
        KeyCode::PageUp => state.card_scroll = state.card_scroll.saturating_sub(8),
        KeyCode::Char('r') => {
            state.requested_key = None;
            state.card_loading = false;
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

fn handle_configuration_key(
    key: KeyCode,
    state: &mut State,
    jira: Option<&JiraClient>,
    jira_error: Option<&str>,
    sender: &Sender<JiraEvent>,
) {
    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            state.config_selected = state.config_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.config_selected = (state.config_selected + 1).min(1);
        }
        KeyCode::Enter => match state.config_selected {
            0 => {
                state.accent_original = state.accent_hex.clone();
                state.accent_selected = ACCENT_PRESETS
                    .iter()
                    .position(|preset| preset.hex.eq_ignore_ascii_case(&state.accent_hex));
                state.mode = Mode::AccentPalette;
            }
            _ => {
                if let Some(client) = jira {
                    open_project_selector(state, client, sender);
                } else {
                    state.message = jira_error.unwrap_or("Jira is unavailable").to_owned();
                }
            }
        },
        KeyCode::Char('g' | 'q') | KeyCode::Esc => state.mode = Mode::Normal,
        _ => {}
    }
}

fn handle_accent_palette_key(
    key: KeyCode,
    state: &mut State,
    config: &mut AppConfig,
) -> Result<()> {
    match key {
        KeyCode::Esc => {
            state.accent_hex = state.accent_original.clone();
            state.accent = accent_color(&state.accent_hex)?;
            state.mode = Mode::Configuration;
        }
        KeyCode::Char('c') => {
            state.input.clear();
            state.mode = Mode::AccentCustom;
        }
        KeyCode::Char('d') => save_accent(state, config, DEFAULT_ACCENT)?,
        KeyCode::Enter => {
            let accent = state.accent_selected.map_or_else(
                || state.accent_hex.clone(),
                |index| ACCENT_PRESETS[index].hex.to_owned(),
            );
            save_accent(state, config, &accent)?;
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
            let mut selected = state.accent_selected.unwrap_or(0);
            let columns = state.accent_columns;
            selected = match key {
                KeyCode::Left if !selected.is_multiple_of(columns) => selected - 1,
                KeyCode::Right
                    if selected % columns < columns - 1 && selected + 1 < ACCENT_PRESETS.len() =>
                {
                    selected + 1
                }
                KeyCode::Up if selected >= columns => selected - columns,
                KeyCode::Down if selected + columns < ACCENT_PRESETS.len() => selected + columns,
                _ => selected,
            };
            state.accent_selected = Some(selected);
            state.accent_hex = ACCENT_PRESETS[selected].hex.to_owned();
            state.accent = accent_color(&state.accent_hex)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_accent_custom_key(key: KeyCode, state: &mut State, config: &mut AppConfig) -> Result<()> {
    match key {
        KeyCode::Enter => {
            let accent = state.input.to_ascii_uppercase();
            match accent_color(&accent) {
                Ok(color) => {
                    config.set_accent(&accent)?;
                    state.accent = color;
                    state.accent_hex = accent.clone();
                    state.accent_original = accent;
                    state.accent_selected = ACCENT_PRESETS
                        .iter()
                        .position(|preset| preset.hex == state.accent_hex);
                    state.input.clear();
                    state.message = format!("Accent saved as {}", state.accent_hex);
                    state.mode = Mode::Configuration;
                }
                Err(error) => state.message = error.to_string(),
            }
        }
        KeyCode::Esc => {
            state.input.clear();
            state.accent_hex = state.accent_selected.map_or_else(
                || state.accent_original.clone(),
                |index| ACCENT_PRESETS[index].hex.to_owned(),
            );
            state.accent = accent_color(&state.accent_hex)?;
            state.mode = Mode::AccentPalette;
        }
        KeyCode::Backspace => {
            state.input.pop();
            preview_custom_accent(state);
        }
        KeyCode::Char(character) => {
            state.input.push(character);
            preview_custom_accent(state);
        }
        _ => {}
    }
    Ok(())
}

fn preview_custom_accent(state: &mut State) {
    let accent = state.input.to_ascii_uppercase();
    if let Ok(color) = accent_color(&accent) {
        state.accent = color;
        state.accent_hex = accent;
    }
}

fn save_accent(state: &mut State, config: &mut AppConfig, accent: &str) -> Result<()> {
    config.set_accent(accent)?;
    state.accent = accent_color(accent)?;
    state.accent_hex = accent.to_owned();
    state.accent_original = accent.to_owned();
    state.accent_selected = ACCENT_PRESETS
        .iter()
        .position(|preset| preset.hex == accent);
    state.message = format!("Accent saved as {accent}");
    state.mode = Mode::Configuration;
    Ok(())
}

fn accent_color(hex: &str) -> Result<Color> {
    let (red, green, blue) = parse_hex_color(hex)?;
    Ok(Color::Rgb(red, green, blue))
}

fn handle_task_search_key(key: KeyCode, state: &mut State) {
    match key {
        KeyCode::Enter => state.mode = Mode::Normal,
        KeyCode::Esc => {
            state.task_query.clear();
            state.selected = 0;
            state.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            state.task_query.pop();
            state.selected = 0;
        }
        KeyCode::Char(character) => {
            state.task_query.push(character);
            state.selected = 0;
        }
        _ => {}
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
            let entries = entries(
                store,
                state.show_history,
                state.show_backlog,
                &state.task_query,
            );
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
        KeyCode::Esc => {
            state.project_generation = state.project_generation.wrapping_add(1);
            state.project_loading = false;
            state.mode = Mode::Configuration;
        }
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
                state.mode = Mode::Configuration;
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

fn edit_selected_document(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    entries: &[Entry],
    state: &mut State,
    store: &mut Store,
    docs_path: &Path,
) -> Result<()> {
    let Some(entry) = entries.get(state.selected) else {
        state.message = "Select a task to document".to_owned();
        return Ok(());
    };
    let prepared = (|| -> Result<_> {
        let (path, link) = docs::ensure_document(docs_path, entry, state.jira_config.as_ref())?;
        if entry.task.doc.is_none() {
            store.link_document(&entry.task.id, &link)?;
        }
        Ok(path)
    })();
    let path = match prepared {
        Ok(path) => path,
        Err(error) => {
            state.message = format!("Documentation error: {error:#}");
            return Ok(());
        }
    };

    disable_raw_mode()?;
    if let Err(error) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        let _ = enable_raw_mode();
        return Err(error.into());
    }
    let _ = terminal.show_cursor();
    let edit_result = docs::edit_document(&path);
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.clear()?;
    state.message = match edit_result {
        Ok(()) => format!("Documentation saved for {}", entry.task.id),
        Err(error) => format!("Documentation error: {error:#}"),
    };
    Ok(())
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
    state.project_error = false;
    state.project_generation = state.project_generation.wrapping_add(1);
    let generation = state.project_generation;
    let client = client.clone();
    let sender = sender.clone();
    thread::spawn(move || {
        let result = client.projects().map_err(|error| error.to_string());
        let _ = sender.send(JiraEvent::Projects(generation, result));
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
            JiraEvent::Projects(generation, result) if generation == state.project_generation => {
                state.project_loading = false;
                match result {
                    Ok(projects) => {
                        state.project_error = false;
                        state.project_results = projects;
                        if state.project_results.is_empty() {
                            state.message =
                                "No Jira projects are available for this account".to_owned();
                        }
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
                    Err(error) => {
                        state.project_error = true;
                        state.message = error;
                    }
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

fn entries(store: &Store, show_history: bool, show_backlog: bool, query: &str) -> Vec<Entry> {
    let entries = if show_history {
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
    };
    entries
        .into_iter()
        .filter(|entry| task_matches(entry, query))
        .collect()
}

fn task_matches(entry: &Entry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || entry.task.text.to_lowercase().contains(&query)
        || entry.task.id.to_lowercase().contains(&query)
        || entry.date.to_lowercase().contains(&query)
        || entry
            .task
            .jira
            .as_ref()
            .is_some_and(|key| key.to_lowercase().contains(&query))
}

fn draw(
    frame: &mut Frame,
    entries: &[Entry],
    state: &State,
    jira: Option<&JiraClient>,
    jira_error: Option<&str>,
    week_report: Option<&WeekReport>,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    draw_header(frame, areas[0], state, week_report);

    if areas[1].width >= 100 {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(areas[1]);
        if let Some(report) = week_report {
            draw_week_tasks(frame, panes[0], report, state.selected, state.accent);
        } else {
            draw_tasks(frame, panes[0], entries, state);
        }
        draw_jira_card(frame, panes[1], state, jira, jira_error);
    } else if state.jira_tab {
        draw_jira_card(frame, areas[1], state, jira, jira_error);
    } else {
        if let Some(report) = week_report {
            draw_week_tasks(frame, areas[1], report, state.selected, state.accent);
        } else {
            draw_tasks(frame, areas[1], entries, state);
        }
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
        Mode::AccentPalette => draw_accent_palette(frame, state),
        Mode::AccentCustom => draw_single_input(
            frame,
            "Custom accent / #RRGGBB / Enter save / Esc back",
            &state.input,
        ),
        Mode::TaskSearch => draw_single_input(
            frame,
            "Search tasks / Enter keep / Esc clear",
            &state.task_query,
        ),
        Mode::Search => draw_search(frame, state),
        Mode::Project => draw_project_selector(frame, state),
        Mode::Transition => draw_transition_selector(frame, state),
        Mode::Comment => draw_comment_editor(frame, state),
        Mode::ConfirmComment => draw_comment_confirmation(frame, state),
        Mode::Normal => {}
    }
}

fn draw_header(frame: &mut Frame, area: Rect, state: &State, week_report: Option<&WeekReport>) {
    let mut title = if let Some(report) = week_report {
        format!(
            "ZLS / WEEK / {}-W{:02} / {} TO {} / {} DONE / {} JIRA",
            report.iso_year,
            report.iso_week,
            report.start,
            report.end,
            report.completed,
            report.jira_linked,
        )
    } else if state.show_history {
        "ZLS / HISTORY".to_owned()
    } else {
        format!("ZLS / TODAY / {}", today())
    };
    if !state.task_query.is_empty() {
        title.push_str(&format!(" / SEARCH: {}", state.task_query));
    }
    let hint = if week_report.is_some() {
        "j/k select  Left/Right week  0 current  e docs  Enter Jira  W/Esc close  q quit".to_owned()
    } else {
        compact_hint()
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Line::styled(hint, Style::default().add_modifier(Modifier::DIM)),
        ]),
        area,
    );
}

fn draw_week_tasks(
    frame: &mut Frame,
    area: Rect,
    report: &WeekReport,
    selected: usize,
    accent: Color,
) {
    let mut items = Vec::new();
    let mut task_index = 0;
    let mut selected_row = None;
    for day in &report.days {
        let title = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
            .map(|date| date.format("%A / %Y-%m-%d").to_string())
            .unwrap_or_else(|_| day.date.clone());
        items.push(ListItem::new(Line::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )));
        if day.tasks.is_empty() {
            items.push(ListItem::new(Line::styled(
                "  No completed tasks.",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        for entry in &day.tasks {
            if task_index == selected {
                selected_row = Some(items.len());
            }
            items.push(weekly_task_item(entry));
            task_index += 1;
        }
    }
    if !report.undated.is_empty() {
        items.push(ListItem::new(Line::styled(
            "Undated",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in &report.undated {
            if task_index == selected {
                selected_row = Some(items.len());
            }
            items.push(weekly_task_item(entry));
            task_index += 1;
        }
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::RIGHT)
                .title(" WEEKLY / <- older / newer -> / 0 current / Esc close "),
        )
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default().with_selected(selected_row);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn weekly_task_item(entry: &Entry) -> ListItem<'static> {
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
    ListItem::new(format!("  [x] {}{jira}{docs}", entry.task.text))
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

    let empty_message = if state.task_query.is_empty() {
        "Nothing here. Press a to add a task.".to_owned()
    } else {
        format!("No tasks match /{}", state.task_query)
    };

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
            if state.task_query.is_empty() {
                "Nothing scheduled for today."
            } else {
                &empty_message
            },
            state.show_history,
        );
        draw_task_list(
            frame,
            panes[1],
            &backlog,
            backlog_selected,
            " BACKLOG ",
            if state.task_query.is_empty() {
                "Backlog is empty. Press B to add."
            } else {
                &empty_message
            },
            false,
        );
    } else {
        draw_task_list(
            frame,
            area,
            &regular,
            regular_selected,
            " TASKS ",
            &empty_message,
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
                let docs = if entry.task.doc.is_some() {
                    " [doc]"
                } else {
                    ""
                };
                let date = if show_dates {
                    format!("  {}", entry.date)
                } else {
                    String::new()
                };
                ListItem::new(format!("[{checked}] {}{jira}{docs}{date}", entry.task.text))
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
            Paragraph::new(
                "No Jira project selected.\n\nOpen Configuration with g to choose your project.",
            )
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
                    .fg(state.accent)
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
            Style::default().fg(state.accent),
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

fn selectable_detail_line(
    label: &str,
    value: &str,
    selected: bool,
    accent: Color,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} {label:<10}", if selected { ">" } else { " " }),
            Style::default().fg(if selected { accent } else { Color::DarkGray }),
        ),
        Span::styled(
            value.to_owned(),
            Style::default().add_modifier(if selected {
                Modifier::REVERSED
            } else {
                Modifier::empty()
            }),
        ),
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
    let area = centered_fixed(frame.area(), 70, 3);
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
                        .fg(state.accent)
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
                .fg(state.accent)
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
    let area = centered_fixed(frame.area(), 70, 19);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" CONFIGURATION / arrows select / Enter edit / g, q, or Esc close ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        section_separator("ZLS", inner.width),
        detail_line("Config file", &state.config_path),
        detail_line("Tasks file", &state.task_path),
        detail_line("Docs path", &state.docs_path),
        Line::raw(""),
        section_separator("APPEARANCE", inner.width),
        Line::from(vec![
            Span::styled(
                format!(
                    "{} {:<10}",
                    if state.config_selected == 0 { ">" } else { " " },
                    "Accent"
                ),
                Style::default()
                    .fg(state.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}  Enter to edit", state.accent_hex),
                Style::default().add_modifier(if state.config_selected == 0 {
                    Modifier::REVERSED
                } else {
                    Modifier::empty()
                }),
            ),
        ]),
        Line::raw(""),
        section_separator("JIRA", inner.width),
    ];
    if let Some(config) = state.jira_config.as_ref() {
        lines.extend([
            selectable_detail_line(
                "Project",
                config.project.as_deref().unwrap_or("Not selected"),
                state.config_selected == 1,
                state.accent,
            ),
            detail_line("Site", &config.site),
            detail_line("Email", &config.email),
            detail_line("Cloud ID", &config.cloud_id),
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
        lines.push(selectable_detail_line(
            "Project",
            "Jira is not configured",
            state.config_selected == 1,
            state.accent,
        ));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_accent_palette(frame: &mut Frame, state: &State) {
    let area = centered_fixed(frame.area(), 92, 14);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ACCENT / arrows preview / Enter save / c custom / d default / Esc cancel ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let is_custom = !ACCENT_PRESETS
        .iter()
        .any(|preset| preset.hex.eq_ignore_ascii_case(&state.accent_original));
    let mut lines = vec![
        Line::from(vec![
            Span::raw("Preview  "),
            Span::styled(
                format!("[ {} ]", state.accent_hex),
                Style::default()
                    .fg(state.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        if is_custom {
            Line::styled(
                format!("Current custom  {}", state.accent_original),
                Style::default().add_modifier(Modifier::DIM),
            )
        } else {
            Line::raw("")
        },
        Line::raw(""),
    ];
    let rows = ACCENT_PRESETS.len().div_ceil(state.accent_columns);
    let cell_width = usize::from(inner.width) / state.accent_columns;
    for row in 0..rows {
        let mut spans = Vec::new();
        for column in 0..state.accent_columns {
            let index = row * state.accent_columns + column;
            if index >= ACCENT_PRESETS.len() {
                break;
            }
            let preset = &ACCENT_PRESETS[index];
            let mut style = Style::default().fg(accent_color(preset.hex).unwrap_or(Color::Reset));
            if state.accent_selected == Some(index) {
                style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
            }
            let label = format!("{} {}", preset.name, preset.hex)
                .chars()
                .take(cell_width)
                .collect::<String>();
            spans.push(Span::styled(
                format!("{label:<width$}", width = cell_width),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::styled(
        "Custom values must use strict #RRGGBB format.",
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_project_selector(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 65, 65);
    frame.render_widget(Clear, area);
    let title = if state.project_loading {
        " JIRA PROJECT / loading... / Esc cancel "
    } else if state.project_error {
        " JIRA PROJECT / load failed / Esc back "
    } else if state.project_results.is_empty() {
        " JIRA PROJECT / no projects / Esc back "
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
                        .fg(state.accent)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DocsConfig, TasksConfig, UiConfig};
    use crate::store::Task;

    fn test_config() -> AppConfig {
        AppConfig {
            tasks: TasksConfig {
                path: "todo.md".into(),
            },
            docs: DocsConfig {
                path: "docs".into(),
            },
            ui: UiConfig::default(),
        }
    }

    #[test]
    fn task_search_matches_all_visible_identifiers_case_insensitively() {
        let entry = Entry {
            date: "2026-09-02".to_owned(),
            task: Task {
                id: "a1B2c3D4".to_owned(),
                text: "Prepare Smart Factory presentation".to_owned(),
                completed: false,
                done: None,
                jira: Some("MP-396".to_owned()),
                doc: None,
            },
        };

        assert!(task_matches(&entry, "smart factory"));
        assert!(task_matches(&entry, "mp-396"));
        assert!(task_matches(&entry, "A1b2"));
        assert!(task_matches(&entry, "09-02"));
        assert!(!task_matches(&entry, "unrelated"));
    }

    #[test]
    fn accent_palette_has_sixteen_valid_unique_colors_with_cyan_default() {
        assert_eq!(ACCENT_PRESETS.len(), 16);
        assert_eq!(ACCENT_PRESETS[0].hex, DEFAULT_ACCENT);
        for (index, preset) in ACCENT_PRESETS.iter().enumerate() {
            assert!(parse_hex_color(preset.hex).is_ok());
            assert!(
                ACCENT_PRESETS[index + 1..]
                    .iter()
                    .all(|other| other.hex != preset.hex)
            );
        }
    }

    #[test]
    fn accent_grid_stays_in_column_at_edges() -> Result<()> {
        let mut state = State {
            accent_selected: Some(1),
            accent_columns: 4,
            ..State::default()
        };
        let mut config = test_config();
        handle_accent_palette_key(KeyCode::Up, &mut state, &mut config)?;
        assert_eq!(state.accent_selected, Some(1));

        state.accent_selected = Some(13);
        handle_accent_palette_key(KeyCode::Down, &mut state, &mut config)?;
        assert_eq!(state.accent_selected, Some(13));

        state.accent_columns = 2;
        state.accent_selected = Some(1);
        handle_accent_palette_key(KeyCode::Up, &mut state, &mut config)?;
        assert_eq!(state.accent_selected, Some(1));
        Ok(())
    }

    #[test]
    fn cancelling_custom_input_restores_the_palette_preview() -> Result<()> {
        let mut state = State {
            mode: Mode::AccentCustom,
            accent_original: DEFAULT_ACCENT.to_owned(),
            accent_hex: ACCENT_PRESETS[1].hex.to_owned(),
            accent: accent_color(ACCENT_PRESETS[1].hex)?,
            accent_selected: Some(1),
            input: "#112233".to_owned(),
            ..State::default()
        };
        preview_custom_accent(&mut state);
        assert_eq!(state.accent_hex, "#112233");

        handle_accent_custom_key(KeyCode::Esc, &mut state, &mut test_config())?;

        assert_eq!(state.accent_hex, ACCENT_PRESETS[1].hex);
        assert!(matches!(state.mode, Mode::AccentPalette));
        Ok(())
    }

    #[test]
    fn configuration_navigates_between_accent_and_jira_project() {
        let (sender, _receiver) = mpsc::channel();
        let mut state = State {
            mode: Mode::Configuration,
            ..State::default()
        };

        handle_configuration_key(KeyCode::Down, &mut state, None, None, &sender);
        assert_eq!(state.config_selected, 1);
        handle_configuration_key(KeyCode::Down, &mut state, None, None, &sender);
        assert_eq!(state.config_selected, 1);
        handle_configuration_key(KeyCode::Up, &mut state, None, None, &sender);
        assert_eq!(state.config_selected, 0);
    }

    #[test]
    fn jira_project_selector_returns_to_configuration() -> Result<()> {
        let mut state = State {
            mode: Mode::Project,
            ..State::default()
        };

        handle_project_key(KeyCode::Esc, &mut state, &mut None)?;

        assert!(matches!(state.mode, Mode::Configuration));
        Ok(())
    }

    #[test]
    fn jira_project_row_is_visible_in_a_short_popup() -> Result<()> {
        let backend = ratatui::backend::TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend)?;
        let state = State {
            config_selected: 1,
            config_path: "/home/user/a/very/long/configuration/path/zls/config.toml".to_owned(),
            task_path: "/home/user/a/very/long/task/storage/path/todo.md".to_owned(),
            docs_path: "/home/user/a/very/long/documentation/storage/path".to_owned(),
            jira_config: Some(JiraConfig {
                site: "https://example.atlassian.net".to_owned(),
                email: "user@example.com".to_owned(),
                cloud_id: "cloud-id".to_owned(),
                project: Some("OBS".to_owned()),
            }),
            ..State::default()
        };

        terminal.draw(|frame| draw_configuration(frame, &state, None))?;
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Project"));
        assert!(rendered.contains("OBS"));
        Ok(())
    }

    #[test]
    fn stale_project_responses_are_ignored() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(JiraEvent::Projects(1, Err("stale error".to_owned())))
            .unwrap();
        let mut state = State {
            project_generation: 2,
            message: "Current message".to_owned(),
            ..State::default()
        };

        handle_jira_events(&mut state, &receiver, &sender, None);

        assert_eq!(state.message, "Current message");
    }
}
