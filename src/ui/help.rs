use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::keybindings::{KEYBINDINGS, Section};

use super::layout::centered;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Close,
}

#[derive(Default)]
pub(super) struct State {
    scroll: u16,
}

impl State {
    pub(super) fn open(&mut self) {
        self.scroll = 0;
    }

    pub(super) fn handle_key(&mut self, key: KeyCode) -> Action {
        match key {
            KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Esc => Action::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(8);
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(8);
                Action::None
            }
            _ => Action::None,
        }
    }

    pub(super) fn draw(&self, frame: &mut Frame, accent: Color) {
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
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
                .scroll((self.scroll, 0)),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_resets_scroll() {
        let mut state = State::default();
        state.handle_key(KeyCode::PageDown);

        state.open();

        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn handles_scroll_keys_with_saturation() {
        let mut state = State::default();

        state.handle_key(KeyCode::Down);
        state.handle_key(KeyCode::Char('j'));
        state.handle_key(KeyCode::PageDown);
        assert_eq!(state.scroll, 10);

        state.handle_key(KeyCode::Up);
        state.handle_key(KeyCode::Char('k'));
        state.handle_key(KeyCode::PageUp);
        state.handle_key(KeyCode::PageUp);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn close_keys_request_close() {
        let mut state = State::default();

        for key in [KeyCode::Char('q'), KeyCode::Char('?'), KeyCode::Esc] {
            assert_eq!(state.handle_key(key), Action::Close);
        }
    }
}
