use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::jira as jira_api;

use super::{
    AsyncEvent,
    layout::{centered, centered_fixed},
};

pub(super) enum Event {
    Card(u64, String, Box<Result<jira_api::IssueCard, String>>),
    Search(u64, Result<Vec<jira_api::IssueSummary>, String>),
    Projects(u64, Result<Vec<jira_api::ProjectSummary>, String>),
    Transitions(u64, String, Result<Vec<jira_api::Transition>, String>),
    Transitioned(String, Result<(), String>),
    Comment(String, Result<jira_api::JiraComment, String>),
    MoreComments(u64, String, Result<Vec<jira_api::JiraComment>, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModeIntent {
    Normal,
    Configuration,
    Search,
    Project,
    Transition,
    Comment,
    ConfirmComment,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum StoreEffect {
    Link {
        task_id: String,
        issue_key: String,
    },
    Import {
        summary: String,
        issue_key: String,
    },
    Unlink {
        task_id: String,
    },
    TouchJira {
        issue_key: String,
        action: &'static str,
    },
}

#[derive(Default)]
pub(super) struct Action {
    pub(super) mode: Option<ModeIntent>,
    pub(super) notice: Option<String>,
    pub(super) store: Option<StoreEffect>,
}

impl Action {
    fn mode(mode: ModeIntent) -> Self {
        Self {
            mode: Some(mode),
            ..Self::default()
        }
    }

    fn notice(message: impl Into<String>) -> Self {
        Self {
            notice: Some(message.into()),
            ..Self::default()
        }
    }
}

pub(super) struct State {
    config: Option<jira_api::JiraConfig>,
    client: Option<jira_api::JiraClient>,
    startup_error: Option<String>,
    sender: Sender<AsyncEvent>,
    card: Option<jira_api::IssueCard>,
    requested_key: Option<String>,
    card_generation: u64,
    card_loading: bool,
    card_scroll: u16,
    search_input: String,
    search_results: Vec<jira_api::IssueSummary>,
    search_selected: usize,
    search_generation: u64,
    search_token: Arc<AtomicU64>,
    search_loading: bool,
    search_task_id: Option<String>,
    project_results: Vec<jira_api::ProjectSummary>,
    project_selected: usize,
    project_loading: bool,
    project_generation: u64,
    project_error: bool,
    transition_results: Vec<jira_api::Transition>,
    transition_selected: usize,
    transition_generation: u64,
    transition_loading: bool,
    more_comments_generation: u64,
    comment_input: String,
}

impl State {
    pub(super) fn new(sender: Sender<AsyncEvent>) -> Self {
        match jira_api::JiraConfig::load() {
            Ok(config) => match jira_api::JiraClient::from_config_snapshot(config.clone()) {
                Ok(client) => Self::with_client(Some(config), Some(client), None, sender),
                Err(error) => {
                    Self::with_client(Some(config), None, Some(error.to_string()), sender)
                }
            },
            Err(error) => Self::with_client(None, None, Some(error.to_string()), sender),
        }
    }

    fn with_client(
        config: Option<jira_api::JiraConfig>,
        client: Option<jira_api::JiraClient>,
        startup_error: Option<String>,
        sender: Sender<AsyncEvent>,
    ) -> Self {
        Self {
            config,
            client,
            startup_error,
            sender,
            card: None,
            requested_key: None,
            card_generation: 0,
            card_loading: false,
            card_scroll: 0,
            search_input: String::new(),
            search_results: Vec::new(),
            search_selected: 0,
            search_generation: 0,
            search_token: Arc::new(AtomicU64::new(0)),
            search_loading: false,
            search_task_id: None,
            project_results: Vec::new(),
            project_selected: 0,
            project_loading: false,
            project_generation: 0,
            project_error: false,
            transition_results: Vec::new(),
            transition_selected: 0,
            transition_generation: 0,
            transition_loading: false,
            more_comments_generation: 0,
            comment_input: String::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn test_default(sender: Sender<AsyncEvent>) -> Self {
        Self::with_client(None, None, None, sender)
    }

    #[cfg(test)]
    pub(super) fn test_with_config(
        config: jira_api::JiraConfig,
        sender: Sender<AsyncEvent>,
    ) -> Self {
        Self::with_client(Some(config), None, Some("unavailable".to_owned()), sender)
    }

    pub(super) fn client(&self) -> Option<&jira_api::JiraClient> {
        self.client.as_ref()
    }

    pub(super) fn config(&self) -> Option<&jira_api::JiraConfig> {
        self.config.as_ref()
    }

    pub(super) fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub(super) fn reset_scroll(&mut self) {
        self.card_scroll = 0;
    }

    pub(super) fn scroll_down(&mut self) {
        self.card_scroll = self.card_scroll.saturating_add(8);
    }

    pub(super) fn scroll_up(&mut self) {
        self.card_scroll = self.card_scroll.saturating_sub(8);
    }

    pub(super) fn refresh(&mut self) {
        self.requested_key = None;
        self.card_loading = false;
    }

    pub(super) fn handle_normal_key(
        &mut self,
        key: KeyCode,
        selected_task_id: Option<&str>,
    ) -> Option<Action> {
        match key {
            KeyCode::Char('l') => selected_task_id
                .map(|task_id| self.open_search(task_id.to_owned()))
                .or_else(|| Some(Action::default())),
            KeyCode::Char('u') => {
                Some(
                    selected_task_id.map_or_else(Action::default, |task_id| Action {
                        notice: Some("Jira link removed".to_owned()),
                        store: Some(StoreEffect::Unlink {
                            task_id: task_id.to_owned(),
                        }),
                        ..Action::default()
                    }),
                )
            }
            KeyCode::Char('c') => Some(self.open_comment()),
            KeyCode::Char('t') => Some(self.open_transition()),
            KeyCode::Char('r') => {
                self.refresh();
                Some(Action::default())
            }
            KeyCode::Char('m') => Some(self.load_more_comments()),
            _ => None,
        }
    }

    pub(super) fn open_search(&mut self, task_id: String) -> Action {
        if self.client.is_none() {
            return Action::notice(
                self.startup_error
                    .as_deref()
                    .unwrap_or("Jira is unavailable"),
            );
        }
        if self
            .config()
            .and_then(|config| config.project.as_ref())
            .is_none()
        {
            return Action::notice("Select JIRA.Project in Configuration first (g)");
        }
        self.search_task_id = Some(task_id);
        self.search_input.clear();
        self.search_results.clear();
        self.search_selected = 0;
        self.request_search(false);
        Action::mode(ModeIntent::Search)
    }

    pub(super) fn open_comment(&mut self) -> Action {
        if self.card.is_some() && self.client.is_some() {
            self.comment_input.clear();
            Action::mode(ModeIntent::Comment)
        } else {
            Action::notice("Select a linked Jira task first")
        }
    }

    pub(super) fn open_transition(&mut self) -> Action {
        let Some(key) = self.card.as_ref().map(|card| card.key.clone()) else {
            return Action::notice("Select a linked Jira task first");
        };
        let Some(client) = self.client.as_ref().cloned() else {
            return Action::notice("Select a linked Jira task first");
        };
        self.transition_results.clear();
        self.transition_selected = 0;
        self.transition_loading = true;
        self.transition_generation = self.transition_generation.wrapping_add(1);
        let generation = self.transition_generation;
        let event_key = key.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client.transitions(&key).map_err(|error| error.to_string());
            let _ = sender.send(AsyncEvent::Jira(Event::Transitions(
                generation, event_key, result,
            )));
        });
        Action::mode(ModeIntent::Transition)
    }

    pub(super) fn load_more_comments(&mut self) -> Action {
        let (Some(client), Some(card)) = (self.client.as_ref(), self.card.as_ref()) else {
            return Action::default();
        };
        self.more_comments_generation = self.more_comments_generation.wrapping_add(1);
        let generation = self.more_comments_generation;
        let client = client.clone();
        let key = card.key.clone();
        let event_key = key.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client
                .issue_comments(&key, 50)
                .map_err(|error| error.to_string());
            let _ = sender.send(AsyncEvent::Jira(Event::MoreComments(
                generation, event_key, result,
            )));
        });
        Action::notice("Loading more comments...")
    }

    pub(super) fn open_project(&mut self) -> Action {
        let Some(client) = self.client.as_ref().cloned() else {
            return Action::notice(
                self.startup_error
                    .as_deref()
                    .unwrap_or("Jira is unavailable"),
            );
        };
        self.project_results.clear();
        self.project_selected = 0;
        self.project_loading = true;
        self.project_error = false;
        self.project_generation = self.project_generation.wrapping_add(1);
        let generation = self.project_generation;
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client.projects().map_err(|error| error.to_string());
            let _ = sender.send(AsyncEvent::Jira(Event::Projects(generation, result)));
        });
        Action::mode(ModeIntent::Project)
    }

    pub(super) fn handle_search_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Esc => {
                self.search_input.clear();
                Action::mode(ModeIntent::Normal)
            }
            KeyCode::Up => {
                self.search_selected = self.search_selected.saturating_sub(1);
                Action::default()
            }
            KeyCode::Down => {
                self.search_selected =
                    (self.search_selected + 1).min(self.search_results.len().saturating_sub(1));
                Action::default()
            }
            KeyCode::Enter if !self.search_results.is_empty() => {
                let issue = &self.search_results[self.search_selected];
                let Some(task_id) = self.search_task_id.clone() else {
                    return Action::default();
                };
                self.search_input.clear();
                Action {
                    mode: Some(ModeIntent::Normal),
                    notice: Some(format!("Linked to {}", issue.key)),
                    store: Some(StoreEffect::Link {
                        task_id,
                        issue_key: issue.key.clone(),
                    }),
                }
            }
            KeyCode::Char('i') if !self.search_results.is_empty() => {
                let issue = &self.search_results[self.search_selected];
                self.search_input.clear();
                Action {
                    mode: Some(ModeIntent::Normal),
                    notice: Some(format!("Imported {}", issue.key)),
                    store: Some(StoreEffect::Import {
                        summary: issue.summary.clone(),
                        issue_key: issue.key.clone(),
                    }),
                }
            }
            KeyCode::Backspace => {
                self.search_input.pop();
                self.request_search(true);
                Action::default()
            }
            KeyCode::Char(character) => {
                self.search_input.push(character);
                self.request_search(true);
                Action::default()
            }
            _ => Action::default(),
        }
    }

    pub(super) fn handle_project_key(&mut self, key: KeyCode) -> Result<Action> {
        let action = match key {
            KeyCode::Esc => {
                self.project_generation = self.project_generation.wrapping_add(1);
                self.project_loading = false;
                Action::mode(ModeIntent::Configuration)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.project_selected = self.project_selected.saturating_sub(1);
                Action::default()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.project_selected =
                    (self.project_selected + 1).min(self.project_results.len().saturating_sub(1));
                Action::default()
            }
            KeyCode::Enter if !self.project_results.is_empty() => {
                let project = &self.project_results[self.project_selected];
                let Some(client) = self.client.as_mut() else {
                    return Ok(Action::default());
                };
                client.set_project(&project.key)?;
                self.config = Some(client.config().clone());
                let notice = format!("Jira project set to {} ({})", project.key, project.name);
                self.search_results.clear();
                Action {
                    mode: Some(ModeIntent::Configuration),
                    notice: Some(notice),
                    store: None,
                }
            }
            _ => Action::default(),
        };
        Ok(action)
    }

    pub(super) fn handle_transition_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Esc => Action::mode(ModeIntent::Normal),
            KeyCode::Up | KeyCode::Char('k') => {
                self.transition_selected = self.transition_selected.saturating_sub(1);
                Action::default()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.transition_selected = (self.transition_selected + 1)
                    .min(self.transition_results.len().saturating_sub(1));
                Action::default()
            }
            KeyCode::Enter if !self.transition_results.is_empty() => {
                let (Some(client), Some(card)) = (self.client.as_ref(), self.card.as_ref()) else {
                    return Action::default();
                };
                let transition = &self.transition_results[self.transition_selected];
                let client = client.clone();
                let key = card.key.clone();
                let event_key = key.clone();
                let transition_id = transition.id.clone();
                let transition_name = transition.name.clone();
                let sender = self.sender.clone();
                thread::spawn(move || {
                    let result = client
                        .transition_issue(&key, &transition_id)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(AsyncEvent::Jira(Event::Transitioned(event_key, result)));
                });
                Action {
                    mode: Some(ModeIntent::Normal),
                    notice: Some(format!("Applying Jira transition: {transition_name}...")),
                    ..Action::default()
                }
            }
            _ => Action::default(),
        }
    }

    pub(super) fn handle_comment_key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Action {
        match key {
            KeyCode::Esc => {
                self.comment_input.clear();
                Action::mode(ModeIntent::Normal)
            }
            KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.comment_input.trim().is_empty() {
                    Action::default()
                } else {
                    Action::mode(ModeIntent::ConfirmComment)
                }
            }
            KeyCode::Enter => {
                self.comment_input.push('\n');
                Action::default()
            }
            KeyCode::Backspace => {
                self.comment_input.pop();
                Action::default()
            }
            KeyCode::Char(character) => {
                self.comment_input.push(character);
                Action::default()
            }
            _ => Action::default(),
        }
    }

    pub(super) fn handle_confirmation_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Char('y') | KeyCode::Enter => {
                let (Some(client), Some(card)) = (self.client.as_ref(), self.card.as_ref()) else {
                    return Action::default();
                };
                let client = client.clone();
                let key = card.key.clone();
                let event_key = key.clone();
                let text = self.comment_input.clone();
                let sender = self.sender.clone();
                thread::spawn(move || {
                    let result = client
                        .post_comment(&key, &text)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(AsyncEvent::Jira(Event::Comment(event_key, result)));
                });
                self.comment_input.clear();
                Action {
                    mode: Some(ModeIntent::Normal),
                    notice: Some("Posting Jira comment...".to_owned()),
                    ..Action::default()
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => Action::mode(ModeIntent::Comment),
            _ => Action::default(),
        }
    }

    pub(super) fn select_issue(&mut self, key: Option<String>) {
        if key == self.requested_key {
            return;
        }
        self.requested_key = key.clone();
        self.card = None;
        self.card_scroll = 0;
        self.card_loading = false;
        self.card_generation = self.card_generation.wrapping_add(1);
        let generation = self.card_generation;
        let (Some(key), Some(client)) = (key, self.client.as_ref()) else {
            return;
        };
        self.card_loading = true;
        let event_key = key.clone();
        let client = client.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client.issue_card(&key).map_err(|error| error.to_string());
            let _ = sender.send(AsyncEvent::Jira(Event::Card(
                generation,
                event_key,
                Box::new(result),
            )));
        });
    }

    fn request_search(&mut self, debounce: bool) {
        let Some(client) = self.client.as_ref().cloned() else {
            return;
        };
        self.search_generation += 1;
        let generation = self.search_generation;
        self.search_token.store(generation, Ordering::Relaxed);
        let search_token = Arc::clone(&self.search_token);
        let query = self.search_input.clone();
        let sender = self.sender.clone();
        self.search_loading = true;
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
            let _ = sender.send(AsyncEvent::Jira(Event::Search(generation, result)));
        });
    }

    pub(super) fn apply_event(&mut self, event: Event) -> Option<Action> {
        let notice = match event {
            Event::Card(generation, key, result)
                if generation == self.card_generation
                    && self.requested_key.as_deref() == Some(&key) =>
            {
                self.card_loading = false;
                match *result {
                    Ok(card) => {
                        self.card = Some(card);
                        None
                    }
                    Err(error) => Some(error),
                }
            }
            Event::Search(generation, result) if generation == self.search_generation => {
                self.search_loading = false;
                match result {
                    Ok(issues) => {
                        self.search_results = issues;
                        self.search_selected = 0;
                        None
                    }
                    Err(error) => Some(error),
                }
            }
            Event::Projects(generation, result) if generation == self.project_generation => {
                self.project_loading = false;
                match result {
                    Ok(projects) => {
                        self.project_error = false;
                        self.project_results = projects;
                        self.project_selected = self
                            .config()
                            .and_then(|config| config.project.as_deref())
                            .and_then(|selected| {
                                self.project_results
                                    .iter()
                                    .position(|project| project.key == selected)
                            })
                            .unwrap_or(0);
                        self.project_results
                            .is_empty()
                            .then(|| "No Jira projects are available for this account".to_owned())
                    }
                    Err(error) => {
                        self.project_error = true;
                        Some(error)
                    }
                }
            }
            Event::Transitions(generation, key, result)
                if generation == self.transition_generation
                    && self.card.as_ref().is_some_and(|card| card.key == key) =>
            {
                self.transition_loading = false;
                match result {
                    Ok(transitions) => {
                        self.transition_results = transitions;
                        self.transition_selected = 0;
                        None
                    }
                    Err(error) => Some(error),
                }
            }
            Event::Transitioned(key, result) => match result {
                Ok(()) => {
                    self.refresh();
                    return Some(Action {
                        notice: Some(format!("Updated Jira status for {key}")),
                        store: Some(StoreEffect::TouchJira {
                            issue_key: key,
                            action: "jira-transitioned",
                        }),
                        ..Action::default()
                    });
                }
                Err(error) => Some(error),
            },
            Event::Comment(key, result) => match result {
                Ok(_) => {
                    self.refresh();
                    return Some(Action {
                        notice: Some(format!("Comment posted to {key}")),
                        store: Some(StoreEffect::TouchJira {
                            issue_key: key,
                            action: "jira-commented",
                        }),
                        ..Action::default()
                    });
                }
                Err(error) => Some(error),
            },
            Event::MoreComments(generation, key, result)
                if generation == self.more_comments_generation
                    && self.card.as_ref().is_some_and(|card| card.key == key) =>
            {
                match result {
                    Ok(comments) => {
                        if let Some(card) = self.card.as_mut() {
                            card.comments = comments;
                        }
                        Some("Loaded comment history".to_owned())
                    }
                    Err(error) => Some(error),
                }
            }
            _ => None,
        };
        notice.map(Action::notice)
    }
}

pub(super) fn draw_card(frame: &mut Frame, area: Rect, state: &State, accent: Color) {
    let project = state.config().and_then(|config| config.project.as_deref());
    let title = project.map_or_else(|| " JIRA ".to_owned(), |key| format!(" JIRA / {key} "));
    let block = Block::default().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(error) = state.startup_error() {
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
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
        .map(jira_api::format_jira_datetime)
        .unwrap_or_else(|| "Unknown".to_owned());
    let updated = card
        .updated
        .as_deref()
        .map(jira_api::format_jira_datetime)
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
                jira_api::format_jira_datetime(&comment.created)
            ),
            Style::default().fg(accent),
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

pub(super) fn draw_search(frame: &mut Frame, state: &State, accent: Color) {
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
        Paragraph::new(state.search_input.as_str())
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
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
        rows[0].x + state.search_input.chars().count() as u16,
        rows[0].y + 1,
    ));
}

pub(super) fn draw_project(frame: &mut Frame, state: &State, accent: Color) {
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
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
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

pub(super) fn draw_transition(frame: &mut Frame, state: &State) {
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

pub(super) fn draw_comment(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 75, 60);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(state.comment_input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" JIRA COMMENT / Ctrl-S review / Esc cancel "),
            ),
        area,
    );
    let line_count = state.comment_input.lines().count().max(1) as u16;
    let column = state
        .comment_input
        .lines()
        .last()
        .map_or(0, |line| line.chars().count()) as u16;
    frame.set_cursor_position((
        area.x + column + 1,
        area.y + line_count.min(area.height - 2),
    ));
}

pub(super) fn draw_comment_confirmation(frame: &mut Frame, state: &State) {
    let area = centered(frame.area(), 65, 45);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(state.comment_input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" POST THIS COMMENT? / y yes / n edit "),
            ),
        area,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let (sender, _receiver) = std::sync::mpsc::channel();
        State::with_client(None, None, Some("unavailable".to_owned()), sender)
    }

    fn card(key: &str, summary: &str, comments: Vec<jira_api::JiraComment>) -> jira_api::IssueCard {
        jira_api::IssueCard {
            key: key.to_owned(),
            summary: summary.to_owned(),
            status: "Open".to_owned(),
            priority: None,
            assignee: None,
            description: String::new(),
            subtasks: Vec::new(),
            links: Vec::new(),
            comments,
            creator: None,
            reporter: None,
            parent: None,
            labels: Vec::new(),
            sprints: Vec::new(),
            issue_type: None,
            components: Vec::new(),
            fix_versions: Vec::new(),
            created: None,
            updated: None,
            due_date: None,
            resolution: None,
            stale: false,
            stale_reason: None,
        }
    }

    fn comment(id: &str) -> jira_api::JiraComment {
        jira_api::JiraComment {
            id: id.to_owned(),
            author: "User".to_owned(),
            created: String::new(),
            updated: String::new(),
            body: id.to_owned(),
        }
    }

    #[test]
    fn loaded_config_is_retained_without_a_client() {
        let config = jira_api::JiraConfig {
            site: "https://example.atlassian.net".to_owned(),
            email: "user@example.com".to_owned(),
            cloud_id: "cloud-id".to_owned(),
            project: Some("APP".to_owned()),
        };

        let state = State::with_client(
            Some(config.clone()),
            None,
            Some("missing Jira token".to_owned()),
            std::sync::mpsc::channel().0,
        );

        assert_eq!(state.config(), Some(&config));
        assert!(state.client().is_none());
        assert_eq!(state.startup_error(), Some("missing Jira token"));
    }

    #[test]
    fn project_selector_returns_to_configuration() -> Result<()> {
        let mut state = state();
        state.project_generation = 4;

        let action = state.handle_project_key(KeyCode::Esc)?;

        assert_eq!(action.mode, Some(ModeIntent::Configuration));
        assert_eq!(state.project_generation, 5);
        Ok(())
    }

    #[test]
    fn stale_project_responses_are_ignored() {
        let mut state = state();
        state.project_generation = 2;
        let action = state.apply_event(Event::Projects(1, Err("stale error".to_owned())));

        assert!(action.is_none());
        assert!(!state.project_error);
    }

    #[test]
    fn stale_card_responses_do_not_replace_the_selected_card() {
        let mut state = state();
        state.requested_key = Some("NEW-2".to_owned());
        let action = state.apply_event(Event::Card(
            0,
            "OLD-1".to_owned(),
            Box::new(Err("stale card error".to_owned())),
        ));

        assert!(action.is_none());
        assert_eq!(state.requested_key.as_deref(), Some("NEW-2"));
    }

    #[test]
    fn stale_same_key_card_response_does_not_replace_newer_response() {
        let mut state = state();
        state.requested_key = Some("APP-1".to_owned());
        state.card_generation = 2;

        assert!(
            state
                .apply_event(Event::Card(
                    2,
                    "APP-1".to_owned(),
                    Box::new(Ok(card("APP-1", "new", Vec::new()))),
                ))
                .is_none()
        );
        let action = state.apply_event(Event::Card(
            1,
            "APP-1".to_owned(),
            Box::new(Ok(card("APP-1", "old", Vec::new()))),
        ));

        assert!(action.is_none());
        assert_eq!(
            state.card.as_ref().map(|card| card.summary.as_str()),
            Some("new")
        );
    }

    #[test]
    fn stale_same_key_transition_response_does_not_replace_newer_response() {
        let mut state = state();
        state.card = Some(card("APP-1", "Issue", Vec::new()));
        state.transition_generation = 2;
        let new = jira_api::Transition {
            id: "2".to_owned(),
            name: "Finish".to_owned(),
            to_status: "Done".to_owned(),
        };
        let old = jira_api::Transition {
            id: "1".to_owned(),
            name: "Start".to_owned(),
            to_status: "In Progress".to_owned(),
        };

        assert!(
            state
                .apply_event(Event::Transitions(
                    2,
                    "APP-1".to_owned(),
                    Ok(vec![new.clone()]),
                ))
                .is_none()
        );
        let action = state.apply_event(Event::Transitions(1, "APP-1".to_owned(), Ok(vec![old])));

        assert!(action.is_none());
        assert_eq!(state.transition_results, vec![new]);
    }

    #[test]
    fn stale_same_key_comment_history_does_not_replace_newer_response() {
        let mut state = state();
        state.card = Some(card("APP-1", "Issue", Vec::new()));
        state.more_comments_generation = 2;
        let new = comment("new");

        let new_action = state.apply_event(Event::MoreComments(
            2,
            "APP-1".to_owned(),
            Ok(vec![new.clone()]),
        ));
        let stale_action = state.apply_event(Event::MoreComments(
            1,
            "APP-1".to_owned(),
            Ok(vec![comment("old")]),
        ));

        assert_eq!(
            new_action.and_then(|action| action.notice),
            Some("Loaded comment history".to_owned())
        );
        assert!(stale_action.is_none());
        assert_eq!(
            state.card.as_ref().map(|card| card.comments.as_slice()),
            Some([new].as_slice())
        );
    }

    #[test]
    fn linking_uses_task_captured_when_search_opened() {
        let mut state = state();
        state.search_task_id = Some("captured-task".to_owned());
        state.search_results.push(jira_api::IssueSummary {
            key: "APP-7".to_owned(),
            summary: "Issue".to_owned(),
            status: "Open".to_owned(),
            priority: None,
            assignee: None,
        });

        let action = state.handle_search_key(KeyCode::Enter);

        assert_eq!(
            action.store,
            Some(StoreEffect::Link {
                task_id: "captured-task".to_owned(),
                issue_key: "APP-7".to_owned(),
            })
        );
    }
}
