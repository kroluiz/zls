use std::{
    io::{self, stdout},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

mod configuration;
mod help;
mod jira;
mod layout;
mod tasks;
mod week;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::{
    config::{AppConfig, config_path},
    docs,
    keybindings::compact_hint,
    store::{Entry, Store, WeekReport},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Help,
    Configuration,
    AccentPalette,
    AccentCustom,
    Search,
    Project,
    Transition,
    Comment,
    ConfirmComment,
}

enum AsyncEvent {
    Jira(jira::Event),
    Configuration(configuration::Event),
}

struct State {
    jira_tab: bool,
    mode: Mode,
    overlay_return: Mode,
    message: String,
    jira: jira::State,
    help: help::State,
    configuration: configuration::State,
    edit_document: bool,
    tasks: tasks::State,
    week: week::State,
    docs_path: PathBuf,
    async_receiver: Receiver<AsyncEvent>,
}

impl State {
    fn new(
        configuration: configuration::State,
        jira: jira::State,
        docs_path: PathBuf,
        async_receiver: Receiver<AsyncEvent>,
    ) -> Self {
        Self {
            jira_tab: false,
            mode: Mode::Normal,
            overlay_return: Mode::Normal,
            message: String::new(),
            jira,
            help: help::State::default(),
            configuration,
            edit_document: false,
            tasks: tasks::State::default(),
            week: week::State::default(),
            docs_path,
            async_receiver,
        }
    }

    #[cfg(test)]
    fn test_default() -> (Self, mpsc::Sender<AsyncEvent>) {
        let (sender, receiver) = mpsc::channel();
        let configuration = configuration::State::test_default(sender.clone());
        let jira = jira::State::test_default(sender.clone());
        (
            Self::new(configuration, jira, PathBuf::new(), receiver),
            sender,
        )
    }
}

#[cfg(test)]
impl Default for State {
    fn default() -> Self {
        Self::test_default().0
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
    let (async_sender, async_receiver) = mpsc::channel();
    let jira = jira::State::new(async_sender.clone());
    let configuration = configuration::State::new(
        config_path()?.display().to_string(),
        store.path().display().to_string(),
        docs_path.display().to_string(),
        config.ui.accent.clone(),
        async_sender,
    )?;
    let mut state = State::new(configuration, jira, docs_path.to_owned(), async_receiver);
    let carried = store.carry()?;
    if carried > 0 {
        state.message = format!(
            "Carried {carried} unfinished task{} into today",
            if carried == 1 { "" } else { "s" }
        );
    }

    loop {
        state.configuration.set_width(terminal.size()?.width);
        drain_async_events(&mut state, store)?;
        let week_report = state.week.report(store)?;
        let entries = week_report
            .as_ref()
            .map_or_else(|| state.tasks.entries(store), WeekReport::entries);
        if state.week.is_active() {
            state.week.clamp_selection(entries.len());
        } else {
            state.tasks.clamp_selection(entries.len());
        }
        let selected = if state.week.is_active() {
            state.week.selected()
        } else {
            state.tasks.selected()
        };
        let selected_issue = entries
            .get(selected)
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
        if handle_jira_scroll_key(key.code, key.modifiers, &mut state) {
            continue;
        }

        match state.mode {
            Mode::Help => {
                let action = state.help.handle_key(key.code);
                apply_help_action(action, &mut state);
            }
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
            Mode::Normal if state.week.is_active() => {
                let action = state.week.handle_key(key.code, entries.len());
                if apply_week_action(action, &mut state) {
                    return Ok(());
                }
            }
            Mode::Normal => {
                if handle_task_key(key.code, &entries, &mut state, store)? {
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

fn drain_async_events(state: &mut State, store: &mut Store) -> Result<()> {
    while let Ok(event) = state.async_receiver.try_recv() {
        match event {
            AsyncEvent::Jira(event) => {
                if let Some(action) = state.jira.apply_event(event) {
                    apply_jira_action(action, state, store)?;
                }
            }
            AsyncEvent::Configuration(event) => {
                if let Some(message) = state.configuration.apply_event(event) {
                    state.message = message;
                }
            }
        }
    }
    Ok(())
}

fn handle_task_key(
    key: KeyCode,
    entries: &[Entry],
    state: &mut State,
    store: &mut Store,
) -> Result<bool> {
    if state.tasks.is_normal() {
        let selected_task_id = state.tasks.selected_task_id(entries);
        if let Some(action) = state.jira.handle_normal_key(key, selected_task_id) {
            apply_jira_action(action, state, store)?;
            return Ok(false);
        }
    }
    let action = state.tasks.handle_key(key, entries, store)?;
    Ok(match action {
        tasks::Action::None => false,
        tasks::Action::Quit => true,
        tasks::Action::OpenWeek => {
            state.week.open();
            state.jira_tab = false;
            false
        }
        tasks::Action::JiraTogglePane => {
            state.jira_tab = !state.jira_tab;
            false
        }
        tasks::Action::JiraResetScroll => {
            state.jira.reset_scroll();
            false
        }
        tasks::Action::EditDocument => {
            state.edit_document = true;
            false
        }
        tasks::Action::Notice(message) => {
            state.message = message;
            false
        }
    })
}

fn apply_week_action(action: week::Action, state: &mut State) -> bool {
    match action {
        week::Action::None => {}
        week::Action::Quit => return true,
        week::Action::Close => {
            state.tasks.reset_selection();
            state.jira_tab = false;
        }
        week::Action::JiraFocus => state.jira_tab = true,
        week::Action::JiraToggleFocus => state.jira_tab = !state.jira_tab,
        week::Action::JiraResetScroll => state.jira.reset_scroll(),
        week::Action::JiraRefresh => state.jira.refresh(),
        week::Action::EditDocument => state.edit_document = true,
    }
    false
}

fn apply_help_action(action: help::Action, state: &mut State) {
    if action == help::Action::Close {
        state.mode = state.overlay_return;
    }
}

fn handle_global_overlay_key(key: KeyCode, state: &mut State) -> bool {
    match key {
        KeyCode::Char('?') if state.mode == Mode::Help => {
            let action = state.help.handle_key(key);
            apply_help_action(action, state);
            true
        }
        KeyCode::Char('?')
            if !matches!(
                state.mode,
                Mode::AccentCustom | Mode::Search | Mode::Comment
            ) =>
        {
            if state.tasks.captures_text() {
                return false;
            }
            state.overlay_return = state.mode;
            state.help.open();
            state.mode = Mode::Help;
            true
        }
        KeyCode::Char('g') if state.mode == Mode::Normal && state.tasks.is_normal() => {
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

fn handle_jira_scroll_key(key: KeyCode, modifiers: KeyModifiers, state: &mut State) -> bool {
    if state.mode != Mode::Normal
        || !state.tasks.is_normal()
        || !modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    match key {
        KeyCode::Char('n') => state.jira.scroll_down(),
        KeyCode::Char('p') => state.jira.scroll_up(),
        _ => return false,
    }
    true
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
    let mut documentation_error = None;
    if let Some(effect) = action.store {
        match effect {
            jira::StoreEffect::Link { task_id, issue_key } => {
                let entry = store.entry(&task_id, false)?;
                let linked = store.link_jira(&task_id, &issue_key)?;
                if let Err(error) = docs::update_jira_link(
                    &state.docs_path,
                    &entry.task,
                    linked.jira.as_deref(),
                    state.jira.config(),
                ) {
                    documentation_error = Some(error);
                }
            }
            jira::StoreEffect::Import { summary, issue_key } => {
                store.add_linked(&summary, Some(&issue_key))?;
                state.tasks.show_today();
            }
            jira::StoreEffect::Unlink { task_id } => {
                let entry = store.entry(&task_id, false)?;
                store.unlink_jira(&task_id)?;
                if let Err(error) =
                    docs::update_jira_link(&state.docs_path, &entry.task, None, state.jira.config())
                {
                    documentation_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = documentation_error {
        state.message = format!("Jira updated, but documentation was not: {error:#}");
    } else if let Some(message) = action.notice {
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

fn edit_selected_document(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    entries: &[Entry],
    state: &mut State,
    store: &mut Store,
    docs_path: &Path,
) -> Result<()> {
    let selected = if state.week.is_active() {
        state.week.selected()
    } else {
        state.tasks.selected()
    };
    let Some(entry) = entries.get(selected) else {
        state.message = "Select a task to document".to_owned();
        return Ok(());
    };
    let prepared = (|| -> Result<_> {
        let (path, link) = docs::ensure_document(docs_path, entry, state.jira.config())?;
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
            week::draw(
                frame,
                panes[0],
                report,
                state.week.selected(),
                state.configuration.accent(),
            );
        } else {
            state.tasks.draw(frame, panes[0], entries);
        }
        jira::draw_card(frame, panes[1], &state.jira, state.configuration.accent());
    } else if state.jira_tab {
        jira::draw_card(frame, areas[1], &state.jira, state.configuration.accent());
    } else {
        if let Some(report) = week_report {
            week::draw(
                frame,
                areas[1],
                report,
                state.week.selected(),
                state.configuration.accent(),
            );
        } else {
            state.tasks.draw(frame, areas[1], entries);
        }
    }

    frame.render_widget(
        Paragraph::new(state.message.as_str()).style(Style::default().add_modifier(Modifier::DIM)),
        areas[2],
    );

    match state.mode {
        Mode::Help => state.help.draw(frame, state.configuration.accent()),
        Mode::Configuration => configuration::draw_overview(
            frame,
            &state.configuration,
            state.jira.config(),
            state.jira.startup_error(),
        ),
        Mode::AccentPalette => configuration::draw_accent(frame, &state.configuration),
        Mode::AccentCustom => configuration::draw_custom_accent(frame, &state.configuration),
        Mode::Search => jira::draw_search(frame, &state.jira, state.configuration.accent()),
        Mode::Project => jira::draw_project(frame, &state.jira, state.configuration.accent()),
        Mode::Transition => jira::draw_transition(frame, &state.jira),
        Mode::Comment => jira::draw_comment(frame, &state.jira),
        Mode::ConfirmComment => jira::draw_comment_confirmation(frame, &state.jira),
        Mode::Normal => state.tasks.draw_dialog(frame),
    }
}

fn draw_header(frame: &mut Frame, area: Rect, state: &State, week_report: Option<&WeekReport>) {
    let title = if let Some(report) = week_report {
        week::title(report)
    } else {
        state.tasks.title()
    };
    let hint = if week_report.is_some() {
        week::HINT.to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn configuration_opens_and_closes_without_leaving_week_view() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let mut state = State::default();
        state.week.open();

        assert!(handle_global_overlay_key(KeyCode::Char('g'), &mut state));
        assert!(matches!(state.mode, Mode::Configuration));
        let action = state
            .configuration
            .handle_overview(KeyCode::Esc, None, None);
        apply_configuration_action(action, &mut state, &mut store)?;

        assert!(matches!(state.mode, Mode::Normal));
        assert!(state.week.is_active());
        Ok(())
    }

    #[test]
    fn week_selection_is_independent_from_normal_task_selection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        store.add("first")?;
        store.add("second")?;
        let entries = store.entries_today();
        let mut state = State::default();

        handle_task_key(KeyCode::Down, &entries, &mut state, &mut store)?;
        assert_eq!(state.tasks.selected(), 1);
        handle_task_key(KeyCode::Char('W'), &entries, &mut state, &mut store)?;
        state.week.handle_key(KeyCode::Down, 3);
        assert_eq!(state.tasks.selected(), 0);
        assert_eq!(state.week.selected(), 1);
        let action = state.week.handle_key(KeyCode::Esc, 3);
        apply_week_action(action, &mut state);

        assert_eq!(state.tasks.selected(), 0);
        assert!(!state.week.is_active());
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
        assert!(handle_global_overlay_key(KeyCode::Char('?'), &mut state));

        assert!(matches!(state.mode, Mode::Configuration));
    }

    #[test]
    fn global_overlay_keys_do_not_capture_text_input() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let mut state = State::default();
        handle_task_key(KeyCode::Char('a'), &[], &mut state, &mut store)?;

        assert!(!handle_global_overlay_key(KeyCode::Char('?'), &mut state));
        assert!(!handle_jira_scroll_key(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            &mut state,
        ));
        Ok(())
    }

    #[test]
    fn jira_scroll_uses_control_n_and_p() {
        let mut state = State::default();

        assert!(handle_jira_scroll_key(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            &mut state,
        ));
        assert!(handle_jira_scroll_key(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            &mut state,
        ));
        assert!(!handle_jira_scroll_key(
            KeyCode::PageDown,
            KeyModifiers::NONE,
            &mut state,
        ));
    }

    #[test]
    fn task_intents_update_root_jira_pane_and_document_effects() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        store.add("task")?;
        let entries = store.entries_today();
        let mut state = State::default();

        handle_task_key(KeyCode::Tab, &entries, &mut state, &mut store)?;
        handle_task_key(KeyCode::Char('e'), &entries, &mut state, &mut store)?;

        assert!(state.jira_tab);
        assert!(state.edit_document);
        Ok(())
    }

    #[test]
    fn jira_keys_take_precedence_and_use_the_selected_task_id() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let first = store.add("first")?;
        let second = store.add("second")?;
        store.link_jira(&second.id, "APP-2")?;
        let entries = store.entries_today();
        let mut state = State::default();
        handle_task_key(KeyCode::Down, &entries, &mut state, &mut store)?;

        handle_task_key(KeyCode::Char('u'), &entries, &mut state, &mut store)?;

        assert_eq!(store.entry(&first.id, false)?.task.jira, None);
        assert_eq!(store.entry(&second.id, false)?.task.jira, None);
        assert_eq!(state.message, "Jira link removed");
        Ok(())
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
    fn jira_project_reaches_configuration_without_action_propagation() -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        let jira = jira::State::test_with_config(
            crate::jira::JiraConfig {
                site: "https://example.atlassian.net".to_owned(),
                email: "user@example.com".to_owned(),
                cloud_id: "cloud-id".to_owned(),
                project: Some("ROOT".to_owned()),
            },
            sender.clone(),
        );
        let mut state = State::new(
            configuration::State::test_default(sender),
            jira,
            PathBuf::new(),
            receiver,
        );
        state.mode = Mode::Configuration;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        terminal.draw(|frame| draw(frame, &[], &state, None))?;
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("ROOT"));
        Ok(())
    }

    #[test]
    fn jira_effects_are_applied_to_root_and_store() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let task = store.add("Existing task")?;
        let mut state = State {
            docs_path: directory.path().join("docs"),
            ..State::default()
        };
        let entry = store.entry(&task.id, false)?;
        let (document, link) = docs::ensure_document(&state.docs_path, &entry, None)?;
        store.link_document(&task.id, &link)?;
        let action = jira::Action {
            mode: Some(jira::ModeIntent::Normal),
            notice: Some("Linked to APP-9".to_owned()),
            store: Some(jira::StoreEffect::Link {
                task_id: task.id.clone(),
                issue_key: "APP-9".to_owned(),
            }),
        };

        apply_jira_action(action, &mut state, &mut store)?;

        assert_eq!(store.entries_today()[0].task.jira.as_deref(), Some("APP-9"));
        assert!(std::fs::read_to_string(&document)?.contains("Jira: APP-9\nCreated:"));
        assert_eq!(state.message, "Linked to APP-9");
        assert!(matches!(state.mode, Mode::Normal));

        apply_jira_action(
            jira::Action {
                notice: Some("Jira link removed".to_owned()),
                store: Some(jira::StoreEffect::Unlink {
                    task_id: task.id.clone(),
                }),
                ..jira::Action::default()
            },
            &mut state,
            &mut store,
        )?;
        assert_eq!(store.entries_today()[0].task.jira, None);
        assert!(std::fs::read_to_string(document)?.contains("Jira: Not linked\nCreated:"));

        apply_jira_action(
            jira::Action {
                mode: Some(jira::ModeIntent::Normal),
                notice: Some("Imported APP-10".to_owned()),
                store: Some(jira::StoreEffect::Import {
                    summary: "Imported task".to_owned(),
                    issue_key: "APP-10".to_owned(),
                }),
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

    #[test]
    fn mixed_async_events_are_applied_in_channel_order() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let (mut state, sender) = State::test_default();

        sender.send(AsyncEvent::Configuration(configuration::Event::Result(
            0,
            std::time::Instant::now(),
            Err("configuration event".to_owned()),
        )))?;
        sender.send(AsyncEvent::Jira(jira::Event::Transitioned(
            "APP-1".to_owned(),
            Err("jira event".to_owned()),
        )))?;

        drain_async_events(&mut state, &mut store)?;

        assert_eq!(state.message, "jira event");
        Ok(())
    }
}
