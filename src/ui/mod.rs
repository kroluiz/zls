use std::{
    io::{self, stdout},
    path::Path,
    time::Duration,
};

mod configuration;
mod jira;

use anyhow::Result;
use chrono::NaiveDate;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    config::{AppConfig, config_path},
    docs,
    keybindings::{KEYBINDINGS, Section, compact_hint},
    store::{BACKLOG, Entry, Store, WeekReport, today, tomorrow},
};

#[derive(Clone, Copy, PartialEq, Eq)]
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

struct State {
    selected: usize,
    show_history: bool,
    show_backlog: bool,
    jira_tab: bool,
    mode: Mode,
    overlay_return: Mode,
    input: String,
    message: String,
    jira: jira::State,
    move_selected: usize,
    help_scroll: u16,
    configuration: configuration::State,
    task_query: String,
    edit_document: bool,
    weekly: bool,
    weeks_ago: u32,
}

impl State {
    fn new(configuration: configuration::State, jira: jira::State) -> Self {
        Self {
            selected: 0,
            show_history: false,
            show_backlog: false,
            jira_tab: false,
            mode: Mode::Normal,
            overlay_return: Mode::Normal,
            input: String::new(),
            message: String::new(),
            jira,
            move_selected: 0,
            help_scroll: 0,
            configuration,
            task_query: String::new(),
            edit_document: false,
            weekly: false,
            weeks_ago: 0,
        }
    }
}

#[cfg(test)]
impl Default for State {
    fn default() -> Self {
        Self::new(configuration::State::default(), jira::State::test_default())
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
    let jira = jira::State::new();
    let configuration = configuration::State::new(
        config_path()?.display().to_string(),
        store.path().display().to_string(),
        docs_path.display().to_string(),
        config.ui.accent.clone(),
        jira.loaded_config(),
    )?;
    let mut state = State::new(configuration, jira);
    let carried = store.carry()?;
    if carried > 0 {
        state.message = format!(
            "Carried {carried} unfinished task{} into today",
            if carried == 1 { "" } else { "s" }
        );
    }

    loop {
        state.configuration.set_width(terminal.size()?.width);
        for action in state.jira.poll() {
            apply_jira_action(action, &mut state, store)?;
        }
        if let Some(message) = state.configuration.poll() {
            state.message = message;
        }
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
        let selected_issue = entries
            .get(state.selected)
            .and_then(|entry| entry.task.jira.clone());
        state.jira.select_issue(selected_issue);

        terminal.draw(|frame| draw(frame, &entries, &state, week_report.as_ref()))?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if handle_global_overlay_key(key.code, &mut state) {
            continue;
        }

        match state.mode {
            Mode::Add => handle_add_key(key.code, &mut state, store, false)?,
            Mode::AddBacklog => handle_add_key(key.code, &mut state, store, true)?,
            Mode::Move => handle_move_key(key.code, &entries, &mut state, store)?,
            Mode::Help => handle_help_key(key.code, &mut state),
            Mode::Configuration => apply_configuration_action(
                configuration::State::handle_overview(
                    &mut state.configuration,
                    key.code,
                    state.jira.client(),
                    state.jira.startup_error(),
                ),
                &mut state,
                store,
            )?,
            Mode::AccentPalette => apply_configuration_action(
                state
                    .configuration
                    .handle_accent_palette(key.code, config)?,
                &mut state,
                store,
            )?,
            Mode::AccentCustom => apply_configuration_action(
                state.configuration.handle_custom_accent(key.code, config)?,
                &mut state,
                store,
            )?,
            Mode::TaskSearch => handle_task_search_key(key.code, &mut state),
            Mode::Search => {
                let action = state.jira.handle_search_key(key.code);
                apply_jira_action(action, &mut state, store)?;
            }
            Mode::Project => {
                let action = state.jira.handle_project_key(key.code)?;
                apply_jira_action(action, &mut state, store)?;
            }
            Mode::Transition => {
                let action = state.jira.handle_transition_key(key.code);
                apply_jira_action(action, &mut state, store)?;
            }
            Mode::Comment => {
                let action = state.jira.handle_comment_key(key.code, key.modifiers);
                apply_jira_action(action, &mut state, store)?;
            }
            Mode::ConfirmComment => {
                let action = state.jira.handle_confirmation_key(key.code);
                apply_jira_action(action, &mut state, store)?;
            }
            Mode::Normal if state.weekly => {
                if handle_week_key(key.code, &entries, &mut state)? {
                    return Ok(());
                }
            }
            Mode::Normal => {
                if handle_normal_key(key.code, &entries, &mut state, store)? {
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
) -> Result<bool> {
    let selected_task_id = entries
        .get(state.selected)
        .map(|entry| entry.task.id.as_str());
    if let Some(action) = state.jira.handle_normal_key(key, selected_task_id) {
        apply_jira_action(action, state, store)?;
        return Ok(false);
    }
    match key {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Esc if !state.task_query.is_empty() => {
            state.task_query.clear();
            state.selected = 0;
        }
        KeyCode::Esc => return Ok(true),
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
            state.jira.reset_scroll();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.selected = (state.selected + 1).min(entries.len().saturating_sub(1));
            state.jira.reset_scroll();
        }
        KeyCode::Tab => state.jira_tab = !state.jira_tab,
        KeyCode::PageDown => state.jira.scroll_down(),
        KeyCode::PageUp => state.jira.scroll_up(),
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
            state.jira.reset_scroll();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.selected = (state.selected + 1).min(entries.len().saturating_sub(1));
            state.jira.reset_scroll();
        }
        KeyCode::Char('e') if !entries.is_empty() => state.edit_document = true,
        KeyCode::Enter if !entries.is_empty() => state.jira_tab = true,
        KeyCode::Tab => state.jira_tab = !state.jira_tab,
        KeyCode::PageDown => state.jira.scroll_down(),
        KeyCode::PageUp => state.jira.scroll_up(),
        KeyCode::Char('r') => state.jira.refresh(),
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
        KeyCode::Char('q') | KeyCode::Esc => state.mode = state.overlay_return,
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

fn handle_global_overlay_key(key: KeyCode, state: &mut State) -> bool {
    match key {
        KeyCode::Char('?') if state.mode == Mode::Help => {
            state.mode = state.overlay_return;
            true
        }
        KeyCode::Char('?')
            if !matches!(
                state.mode,
                Mode::Add
                    | Mode::AddBacklog
                    | Mode::AccentCustom
                    | Mode::TaskSearch
                    | Mode::Search
                    | Mode::Comment
            ) =>
        {
            state.overlay_return = state.mode;
            state.help_scroll = 0;
            state.mode = Mode::Help;
            true
        }
        KeyCode::Char('g') if state.mode == Mode::Normal => {
            state.mode = Mode::Configuration;
            if let Some(message) = state
                .configuration
                .open(state.jira.client(), state.jira.startup_error())
            {
                state.message = message;
            }
            true
        }
        _ => false,
    }
}

fn apply_configuration_action(
    action: configuration::Action,
    state: &mut State,
    store: &mut Store,
) -> Result<()> {
    match action {
        configuration::Action::None => {}
        configuration::Action::Close => {
            state.mode = match state.mode {
                Mode::AccentCustom => Mode::AccentPalette,
                Mode::AccentPalette => Mode::Configuration,
                _ => Mode::Normal,
            };
        }
        configuration::Action::OpenAccent => state.mode = Mode::AccentPalette,
        configuration::Action::OpenCustomAccent => state.mode = Mode::AccentCustom,
        configuration::Action::OpenProject => {
            let action = state.jira.open_project();
            apply_jira_action(action, state, store)?;
        }
        configuration::Action::Message(message) => {
            state.message = message;
        }
        configuration::Action::AccentSaved(message) => {
            state.message = message;
            state.mode = Mode::Configuration;
        }
    }
    Ok(())
}

fn apply_jira_action(action: jira::Action, state: &mut State, store: &mut Store) -> Result<()> {
    if let Some(effect) = action.store {
        match effect {
            jira::StoreEffect::Link { task_id, issue_key } => {
                store.link_jira(&task_id, &issue_key)?;
            }
            jira::StoreEffect::Import { summary, issue_key } => {
                store.add_linked(&summary, Some(&issue_key))?;
                state.show_history = false;
            }
            jira::StoreEffect::Unlink { task_id } => {
                store.unlink_jira(&task_id)?;
            }
        }
    }
    if let Some(config) = action.config {
        state.configuration.set_jira_config(config);
    }
    if let Some(message) = action.notice {
        state.message = message;
    }
    if let Some(mode) = action.mode {
        state.mode = match mode {
            jira::ModeIntent::Normal => Mode::Normal,
            jira::ModeIntent::Configuration => Mode::Configuration,
            jira::ModeIntent::Search => Mode::Search,
            jira::ModeIntent::Project => Mode::Project,
            jira::ModeIntent::Transition => Mode::Transition,
            jira::ModeIntent::Comment => Mode::Comment,
            jira::ModeIntent::ConfirmComment => Mode::ConfirmComment,
        };
    }
    Ok(())
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
        let (path, link) =
            docs::ensure_document(docs_path, entry, state.configuration.jira_config())?;
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

fn draw(frame: &mut Frame, entries: &[Entry], state: &State, week_report: Option<&WeekReport>) {
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
            draw_week_tasks(
                frame,
                panes[0],
                report,
                state.selected,
                state.configuration.accent(),
            );
        } else {
            draw_tasks(frame, panes[0], entries, state);
        }
        jira::draw_card(frame, panes[1], &state.jira, state.configuration.accent());
    } else if state.jira_tab {
        jira::draw_card(frame, areas[1], &state.jira, state.configuration.accent());
    } else {
        if let Some(report) = week_report {
            draw_week_tasks(
                frame,
                areas[1],
                report,
                state.selected,
                state.configuration.accent(),
            );
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
        Mode::Configuration => {
            configuration::draw_overview(frame, &state.configuration, state.jira.startup_error())
        }
        Mode::AccentPalette => configuration::draw_accent(frame, &state.configuration),
        Mode::AccentCustom => configuration::draw_custom_accent(frame, &state.configuration),
        Mode::TaskSearch => draw_single_input(
            frame,
            "Search tasks / Enter keep / Esc clear",
            &state.task_query,
        ),
        Mode::Search => jira::draw_search(frame, &state.jira, state.configuration.accent()),
        Mode::Project => jira::draw_project(frame, &state.jira, state.configuration.accent()),
        Mode::Transition => jira::draw_transition(frame, &state.jira),
        Mode::Comment => jira::draw_comment(frame, &state.jira),
        Mode::ConfirmComment => jira::draw_comment_confirmation(frame, &state.jira),
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

fn draw_single_input(frame: &mut Frame, title: &str, input: &str) {
    let area = centered_fixed(frame.area(), 70, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(input).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
    frame.set_cursor_position((area.x + input.chars().count() as u16 + 1, area.y + 1));
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
                .fg(state.configuration.accent())
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
    use crate::store::Task;

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
    fn configuration_opens_and_closes_without_leaving_week_view() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let mut state = State {
            weekly: true,
            mode: Mode::Normal,
            ..State::default()
        };

        assert!(handle_global_overlay_key(KeyCode::Char('g'), &mut state));
        assert!(matches!(state.mode, Mode::Configuration));
        let action = state
            .configuration
            .handle_overview(KeyCode::Esc, None, None);
        apply_configuration_action(action, &mut state, &mut store)?;

        assert!(matches!(state.mode, Mode::Normal));
        assert!(state.weekly);
        Ok(())
    }

    #[test]
    fn help_returns_to_the_overlay_that_opened_it() {
        let mut state = State {
            mode: Mode::Configuration,
            ..State::default()
        };

        assert!(handle_global_overlay_key(KeyCode::Char('?'), &mut state));
        assert!(matches!(state.mode, Mode::Help));
        handle_help_key(KeyCode::Esc, &mut state);

        assert!(matches!(state.mode, Mode::Configuration));
    }

    #[test]
    fn global_overlay_keys_do_not_capture_text_input() {
        let mut state = State {
            mode: Mode::Add,
            ..State::default()
        };

        assert!(!handle_global_overlay_key(KeyCode::Char('?'), &mut state));
    }

    #[test]
    fn configuration_actions_map_to_global_modes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let mut state = State::default();

        apply_configuration_action(configuration::Action::OpenAccent, &mut state, &mut store)?;
        assert!(matches!(state.mode, Mode::AccentPalette));

        apply_configuration_action(
            configuration::Action::OpenCustomAccent,
            &mut state,
            &mut store,
        )?;
        assert!(matches!(state.mode, Mode::AccentCustom));

        apply_configuration_action(configuration::Action::Close, &mut state, &mut store)?;
        assert!(matches!(state.mode, Mode::AccentPalette));

        apply_configuration_action(
            configuration::Action::AccentSaved("saved".to_owned()),
            &mut state,
            &mut store,
        )?;
        assert!(matches!(state.mode, Mode::Configuration));
        assert_eq!(state.message, "saved");
        Ok(())
    }

    #[test]
    fn jira_effects_are_applied_to_root_and_store() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let task = store.add("Existing task")?;
        let mut state = State::default();
        let action = jira::Action {
            mode: Some(jira::ModeIntent::Normal),
            notice: Some("Linked to APP-9".to_owned()),
            store: Some(jira::StoreEffect::Link {
                task_id: task.id,
                issue_key: "APP-9".to_owned(),
            }),
            config: None,
        };

        apply_jira_action(action, &mut state, &mut store)?;

        assert_eq!(store.entries_today()[0].task.jira.as_deref(), Some("APP-9"));
        assert_eq!(state.message, "Linked to APP-9");
        assert!(matches!(state.mode, Mode::Normal));

        apply_jira_action(
            jira::Action {
                mode: Some(jira::ModeIntent::Normal),
                notice: Some("Imported APP-10".to_owned()),
                store: Some(jira::StoreEffect::Import {
                    summary: "Imported task".to_owned(),
                    issue_key: "APP-10".to_owned(),
                }),
                config: None,
            },
            &mut state,
            &mut store,
        )?;

        assert!(store.entries_today().iter().any(|entry| {
            entry.task.text == "Imported task" && entry.task.jira.as_deref() == Some("APP-10")
        }));
        assert_eq!(state.message, "Imported APP-10");
        Ok(())
    }
}
