use anyhow::Result;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::store::{BACKLOG, Entry, Store, today, tomorrow};

use super::layout::centered_fixed;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Quit,
    OpenWeek,
    JiraTogglePane,
    JiraResetScroll,
    JiraScrollDown,
    JiraScrollUp,
    EditDocument,
    Notice(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Dialog {
    #[default]
    None,
    Add,
    AddBacklog,
    Move,
    Search,
}

#[derive(Default)]
pub(super) struct State {
    selected: usize,
    show_history: bool,
    show_backlog: bool,
    query: String,
    dialog: Dialog,
    input: String,
    move_selected: usize,
}

impl State {
    pub(super) fn entries(&self, store: &Store) -> Vec<Entry> {
        let entries = if self.show_history {
            let all = store.entries_all().into_iter().collect::<Vec<_>>();
            let mut entries = all
                .iter()
                .filter(|entry| entry.date != BACKLOG)
                .cloned()
                .collect::<Vec<_>>();
            if self.show_backlog {
                entries.extend(all.into_iter().filter(|entry| entry.date == BACKLOG));
            }
            entries
        } else {
            let mut entries = store.entries_today();
            if self.show_backlog {
                entries.extend(store.entries_backlog());
            }
            entries
        };
        entries
            .into_iter()
            .filter(|entry| task_matches(entry, &self.query))
            .collect()
    }

    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    pub(super) fn selected_task_id<'a>(&self, entries: &'a [Entry]) -> Option<&'a str> {
        entries
            .get(self.selected)
            .map(|entry| entry.task.id.as_str())
    }

    pub(super) fn clamp_selection(&mut self, entry_count: usize) {
        self.selected = self.selected.min(entry_count.saturating_sub(1));
    }

    pub(super) fn reset_selection(&mut self) {
        self.selected = 0;
    }

    pub(super) fn captures_text(&self) -> bool {
        matches!(
            self.dialog,
            Dialog::Add | Dialog::AddBacklog | Dialog::Search
        )
    }

    pub(super) fn is_normal(&self) -> bool {
        self.dialog == Dialog::None
    }

    pub(super) fn handle_key(
        &mut self,
        key: KeyCode,
        entries: &[Entry],
        store: &mut Store,
    ) -> Result<Action> {
        match self.dialog {
            Dialog::None => self.handle_normal_key(key, entries, store),
            Dialog::Add => self.handle_add_key(key, store, false),
            Dialog::AddBacklog => self.handle_add_key(key, store, true),
            Dialog::Move => self.handle_move_key(key, entries, store),
            Dialog::Search => Ok(self.handle_search_key(key)),
        }
    }

    fn handle_normal_key(
        &mut self,
        key: KeyCode,
        entries: &[Entry],
        store: &mut Store,
    ) -> Result<Action> {
        let action = match key {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.selected = 0;
                Action::None
            }
            KeyCode::Esc => Action::Quit,
            KeyCode::Char('W') => {
                self.selected = 0;
                self.query.clear();
                Action::OpenWeek
            }
            KeyCode::Char('/') => {
                self.dialog = Dialog::Search;
                self.selected = 0;
                Action::None
            }
            KeyCode::Char('e') if !entries.is_empty() => Action::EditDocument,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::JiraResetScroll
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(entries.len().saturating_sub(1));
                Action::JiraResetScroll
            }
            KeyCode::Tab => Action::JiraTogglePane,
            KeyCode::PageDown => Action::JiraScrollDown,
            KeyCode::PageUp => Action::JiraScrollUp,
            KeyCode::Char('a') => {
                self.dialog = Dialog::Add;
                self.input.clear();
                Action::None
            }
            KeyCode::Char('B') => {
                self.dialog = Dialog::AddBacklog;
                self.input.clear();
                Action::None
            }
            KeyCode::Char('b') => {
                self.show_backlog = !self.show_backlog;
                Action::Notice(if self.show_backlog {
                    "Backlog visible".to_owned()
                } else {
                    "Backlog hidden".to_owned()
                })
            }
            KeyCode::Char('p') if !entries.is_empty() => {
                let entry = &entries[self.selected];
                if entry.date == BACKLOG {
                    store.promote_backlog(&entry.task.id)?;
                    Action::Notice("Moved backlog task into today".to_owned())
                } else {
                    Action::Notice("Select a backlog task to promote".to_owned())
                }
            }
            KeyCode::Char('M') if !entries.is_empty() => {
                self.dialog = Dialog::Move;
                self.move_selected = 0;
                Action::None
            }
            KeyCode::Char(' ') | KeyCode::Char('d') if !entries.is_empty() => {
                let task = &entries[self.selected].task;
                let completed = !task.completed;
                store.set_completed(&task.id, false, completed)?;
                Action::Notice(if completed { "Completed" } else { "Reopened" }.to_owned())
            }
            KeyCode::Char('h') => {
                self.show_history = !self.show_history;
                self.selected = 0;
                Action::Notice(String::new())
            }
            KeyCode::Char('C') => {
                let count = store.carry()?;
                self.show_history = false;
                self.selected = 0;
                Action::Notice(format!(
                    "Carried {count} task{}",
                    if count == 1 { "" } else { "s" }
                ))
            }
            _ => Action::None,
        };
        Ok(action)
    }

    fn handle_add_key(&mut self, key: KeyCode, store: &mut Store, backlog: bool) -> Result<Action> {
        match key {
            KeyCode::Enter => {
                let notice = if self.input.trim().is_empty() {
                    None
                } else if backlog {
                    store.add_backlog(&self.input)?;
                    self.show_backlog = true;
                    Some("Backlog task added")
                } else {
                    store.add(&self.input)?;
                    self.show_history = false;
                    Some("Task added")
                };
                self.input.clear();
                self.dialog = Dialog::None;
                Ok(notice.map_or(Action::None, |message| Action::Notice(message.to_owned())))
            }
            KeyCode::Esc => {
                self.input.clear();
                self.dialog = Dialog::None;
                Ok(Action::None)
            }
            KeyCode::Backspace => {
                self.input.pop();
                Ok(Action::None)
            }
            KeyCode::Char(character) => {
                self.input.push(character);
                Ok(Action::None)
            }
            _ => Ok(Action::None),
        }
    }

    fn handle_move_key(
        &mut self,
        key: KeyCode,
        entries: &[Entry],
        store: &mut Store,
    ) -> Result<Action> {
        match key {
            KeyCode::Esc => self.dialog = Dialog::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selected = self.move_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selected = (self.move_selected + 1).min(2);
            }
            KeyCode::Enter => {
                if let Some(entry) = entries.get(self.selected) {
                    let destination = match self.move_selected {
                        0 => "today",
                        1 => "tomorrow",
                        _ => "backlog",
                    };
                    store.move_task(&entry.task.id, destination, false)?;
                    self.dialog = Dialog::None;
                    return Ok(Action::Notice(format!("Moved task to {destination}")));
                }
            }
            _ => {}
        }
        Ok(Action::None)
    }

    fn handle_search_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Enter => self.dialog = Dialog::None,
            KeyCode::Esc => {
                self.query.clear();
                self.selected = 0;
                self.dialog = Dialog::None;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(character) => {
                self.query.push(character);
                self.selected = 0;
            }
            _ => {}
        }
        Action::None
    }

    pub(super) fn show_today(&mut self) {
        self.show_history = false;
    }

    pub(super) fn title(&self) -> String {
        let mut title = if self.show_history {
            "ZLS / HISTORY".to_owned()
        } else {
            format!("ZLS / TODAY / {}", today())
        };
        if !self.query.is_empty() {
            title.push_str(&format!(" / SEARCH: {}", self.query));
        }
        title
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, entries: &[Entry]) {
        let regular = entries
            .iter()
            .filter(|entry| entry.date != BACKLOG)
            .collect::<Vec<_>>();
        let backlog = entries
            .iter()
            .filter(|entry| entry.date == BACKLOG)
            .collect::<Vec<_>>();
        let selected_id = entries
            .get(self.selected)
            .map(|entry| entry.task.id.as_str());
        let regular_selected =
            selected_id.and_then(|id| regular.iter().position(|entry| entry.task.id == id));
        let backlog_selected =
            selected_id.and_then(|id| backlog.iter().position(|entry| entry.task.id == id));

        let empty_message = if self.query.is_empty() {
            "Nothing here. Press a to add a task.".to_owned()
        } else {
            format!("No tasks match /{}", self.query)
        };

        if self.show_backlog {
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
                if self.query.is_empty() {
                    "Nothing scheduled for today."
                } else {
                    &empty_message
                },
                self.show_history,
            );
            draw_task_list(
                frame,
                panes[1],
                &backlog,
                backlog_selected,
                " BACKLOG ",
                if self.query.is_empty() {
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
                self.show_history,
            );
        }
    }

    pub(super) fn draw_dialog(&self, frame: &mut Frame) {
        match self.dialog {
            Dialog::None => {}
            Dialog::Add => draw_single_input(frame, "New task", &self.input),
            Dialog::AddBacklog => draw_single_input(frame, "New backlog task", &self.input),
            Dialog::Move => self.draw_move_selector(frame),
            Dialog::Search => {
                draw_single_input(frame, "Search tasks / Enter keep / Esc clear", &self.query)
            }
        }
    }

    fn draw_move_selector(&self, frame: &mut Frame) {
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
        let mut list_state = ListState::default().with_selected(Some(self.move_selected));
        frame.render_stateful_widget(list, inner, &mut list_state);
    }
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
    fn dialogs_and_mutations_are_owned_by_task_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::load(directory.path().join("todo.md"))?;
        let mut state = State::default();

        assert_eq!(
            state.handle_key(KeyCode::Char('a'), &[], &mut store)?,
            Action::None
        );
        assert!(state.captures_text());
        state.handle_key(KeyCode::Char('n'), &[], &mut store)?;
        assert_eq!(
            state.handle_key(KeyCode::Enter, &[], &mut store)?,
            Action::Notice("Task added".to_owned())
        );
        assert!(state.is_normal());
        assert_eq!(store.entries_today()[0].task.text, "n");
        Ok(())
    }
}
