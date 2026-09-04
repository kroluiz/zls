use anyhow::Result;
use chrono::NaiveDate;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState},
};

use crate::store::{Entry, Store, WeekReport};

pub(super) const HINT: &str =
    "j/k select  Left/Right week  0 current  e docs  Enter Jira  W/Esc close  q quit";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Quit,
    Close,
    JiraFocus,
    JiraToggleFocus,
    JiraResetScroll,
    JiraScrollDown,
    JiraScrollUp,
    JiraRefresh,
    EditDocument,
}

#[derive(Default)]
pub(super) struct State {
    active: bool,
    weeks_ago: u32,
    selected: usize,
}

impl State {
    pub(super) fn open(&mut self) {
        self.active = true;
        self.weeks_ago = 0;
        self.selected = 0;
    }

    pub(super) fn is_active(&self) -> bool {
        self.active
    }

    pub(super) fn report(&self, store: &Store) -> Result<Option<WeekReport>> {
        self.active
            .then(|| store.week_report_weeks_ago(self.weeks_ago))
            .transpose()
    }

    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    pub(super) fn clamp_selection(&mut self, entry_count: usize) {
        self.selected = self.selected.min(entry_count.saturating_sub(1));
    }

    pub(super) fn handle_key(&mut self, key: KeyCode, entry_count: usize) -> Action {
        match key {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc | KeyCode::Char('W') => {
                self.active = false;
                self.selected = 0;
                Action::Close
            }
            KeyCode::Left => {
                self.weeks_ago = self.weeks_ago.saturating_add(1);
                self.selected = 0;
                Action::None
            }
            KeyCode::Right => {
                self.weeks_ago = self.weeks_ago.saturating_sub(1);
                self.selected = 0;
                Action::None
            }
            KeyCode::Char('0') => {
                self.weeks_ago = 0;
                self.selected = 0;
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::JiraResetScroll
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(entry_count.saturating_sub(1));
                Action::JiraResetScroll
            }
            KeyCode::Char('e') if entry_count > 0 => Action::EditDocument,
            KeyCode::Enter if entry_count > 0 => Action::JiraFocus,
            KeyCode::Tab => Action::JiraToggleFocus,
            KeyCode::PageDown => Action::JiraScrollDown,
            KeyCode::PageUp => Action::JiraScrollUp,
            KeyCode::Char('r') => Action::JiraRefresh,
            _ => Action::None,
        }
    }
}

pub(super) fn title(report: &WeekReport) -> String {
    format!(
        "ZLS / WEEK / {}-W{:02} / {} TO {} / {} DONE / {} JIRA",
        report.iso_year,
        report.iso_week,
        report.start,
        report.end,
        report.completed,
        report.jira_linked,
    )
}

pub(super) fn draw(
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
            items.push(task_item(entry));
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
            items.push(task_item(entry));
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

fn task_item(entry: &Entry) -> ListItem<'static> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Task, WeekDay};
    use ratatui::{Terminal, backend::TestBackend};

    fn entry(id: &str, text: &str) -> Entry {
        Entry {
            date: "2026-09-01".to_owned(),
            task: Task {
                id: id.to_owned(),
                text: text.to_owned(),
                completed: true,
                done: None,
                jira: None,
                doc: None,
            },
        }
    }

    #[test]
    fn selection_follows_flattened_report_order() -> Result<()> {
        let report = WeekReport {
            iso_year: 2026,
            iso_week: 36,
            start: "2026-08-31".to_owned(),
            end: "2026-09-06".to_owned(),
            completed: 2,
            jira_linked: 0,
            days: vec![WeekDay {
                date: "2026-09-01".to_owned(),
                tasks: vec![entry("dated", "dated task")],
            }],
            undated: vec![entry("undated", "undated task")],
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 8))?;

        terminal.draw(|frame| draw(frame, frame.area(), &report, 1, Color::Cyan))?;

        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.find("dated task").unwrap() < rendered.find("Undated").unwrap());
        assert!(rendered.find("Undated").unwrap() < rendered.find("undated task").unwrap());
        assert!(rendered.contains(">   [x] undated task"));
        Ok(())
    }

    #[test]
    fn keys_update_owned_state_and_return_root_intents() {
        let mut state = State::default();
        state.open();

        assert_eq!(state.handle_key(KeyCode::Down, 2), Action::JiraResetScroll);
        assert_eq!(state.selected(), 1);
        assert_eq!(
            state.handle_key(KeyCode::Char('e'), 2),
            Action::EditDocument
        );
        assert_eq!(state.handle_key(KeyCode::Enter, 2), Action::JiraFocus);
        assert_eq!(state.handle_key(KeyCode::Esc, 2), Action::Close);
        assert!(!state.is_active());
    }
}
