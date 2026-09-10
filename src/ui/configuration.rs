use std::{
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    config::{AppConfig, DEFAULT_ACCENT, parse_hex_color},
    jira::{JiraClient, JiraConfig},
};

use super::{AsyncEvent, layout::centered_fixed};

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

const CONFIG_ACCENT: usize = 3;
const CONFIG_PROJECT: usize = 4;
const CONFIG_TEST_JIRA: usize = 10;
const CONFIG_LAST_WITH_JIRA: usize = CONFIG_TEST_JIRA;
const JIRA_CONNECTION_TTL: Duration = Duration::from_secs(5 * 60);

pub(super) enum Event {
    Result(u64, Instant, Result<String, String>),
}

enum JiraConnectionStatus {
    NotChecked,
    Checking,
    Connected(String),
    Failed(String),
}

pub(super) enum Action {
    None,
    Close,
    OpenAccent,
    OpenCustomAccent,
    OpenProject,
    Message(String),
    AccentSaved(String),
}

pub(super) struct State {
    selected: usize,
    config_path: String,
    task_path: String,
    docs_path: String,
    accent: Color,
    accent_hex: String,
    accent_original: String,
    accent_selected: Option<usize>,
    accent_columns: usize,
    custom_accent: String,
    connection_status: JiraConnectionStatus,
    connection_checked_at: Option<Instant>,
    connection_generation: u64,
    connection_sender: Sender<AsyncEvent>,
}

impl State {
    pub(super) fn new(
        config_path: String,
        task_path: String,
        docs_path: String,
        accent_hex: String,
        connection_sender: Sender<AsyncEvent>,
    ) -> Result<Self> {
        Ok(Self {
            selected: 0,
            config_path,
            task_path,
            docs_path,
            accent: accent_color(&accent_hex)?,
            accent_selected: ACCENT_PRESETS
                .iter()
                .position(|preset| preset.hex.eq_ignore_ascii_case(&accent_hex)),
            accent_original: accent_hex.clone(),
            accent_hex,
            accent_columns: 4,
            custom_accent: String::new(),
            connection_status: JiraConnectionStatus::NotChecked,
            connection_checked_at: None,
            connection_generation: 0,
            connection_sender,
        })
    }

    #[cfg(test)]
    pub(super) fn test_default(sender: Sender<AsyncEvent>) -> Self {
        Self::new(
            String::new(),
            String::new(),
            String::new(),
            DEFAULT_ACCENT.to_owned(),
            sender,
        )
        .expect("the default accent is valid")
    }

    pub(super) fn accent(&self) -> Color {
        self.accent
    }

    pub(super) fn set_width(&mut self, width: u16) {
        self.accent_columns = if width >= 92 { 4 } else { 2 };
    }

    pub(super) fn open(
        &mut self,
        jira: Option<&JiraClient>,
        jira_error: Option<&str>,
    ) -> Option<String> {
        self.selected = 0;
        self.connection_test_due(Instant::now())
            .then(|| self.request_connection_test(jira, jira_error))
    }

    pub(super) fn apply_event(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Result(generation, checked_at, result) => {
                if generation != self.connection_generation {
                    return None;
                }
                self.connection_checked_at = Some(checked_at);
                match result {
                    Ok(display_name) => {
                        let message = format!("Jira connection verified as {display_name}");
                        self.connection_status = JiraConnectionStatus::Connected(display_name);
                        Some(message)
                    }
                    Err(error) => {
                        let message = error.clone();
                        self.connection_status = JiraConnectionStatus::Failed(error);
                        Some(message)
                    }
                }
            }
        }
    }

    pub(super) fn handle_overview(
        &mut self,
        key: KeyCode,
        jira: Option<&JiraClient>,
        jira_error: Option<&str>,
    ) -> Action {
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(CONFIG_LAST_WITH_JIRA);
                Action::None
            }
            KeyCode::Enter => match self.selected {
                CONFIG_ACCENT => {
                    self.accent_original = self.accent_hex.clone();
                    self.accent_selected = ACCENT_PRESETS
                        .iter()
                        .position(|preset| preset.hex.eq_ignore_ascii_case(&self.accent_hex));
                    Action::OpenAccent
                }
                CONFIG_PROJECT if jira.is_some() => Action::OpenProject,
                CONFIG_PROJECT => {
                    Action::Message(jira_error.unwrap_or("Jira is unavailable").to_owned())
                }
                CONFIG_TEST_JIRA => Action::Message(self.request_connection_test(jira, jira_error)),
                _ => Action::Message("This configuration value is read-only".to_owned()),
            },
            KeyCode::Char('g' | 'q') | KeyCode::Esc => Action::Close,
            _ => Action::None,
        }
    }

    pub(super) fn handle_accent_palette(
        &mut self,
        key: KeyCode,
        config: &mut AppConfig,
    ) -> Result<Action> {
        let action = match key {
            KeyCode::Esc => {
                self.accent_hex = self.accent_original.clone();
                self.accent = accent_color(&self.accent_hex)?;
                Action::Close
            }
            KeyCode::Char('c') => {
                self.custom_accent.clear();
                Action::OpenCustomAccent
            }
            KeyCode::Char('d') => self.save_accent(config, DEFAULT_ACCENT)?,
            KeyCode::Enter => {
                let accent = self.accent_selected.map_or_else(
                    || self.accent_hex.clone(),
                    |index| ACCENT_PRESETS[index].hex.to_owned(),
                );
                self.save_accent(config, &accent)?
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let mut selected = self.accent_selected.unwrap_or(0);
                let columns = self.accent_columns;
                selected = match key {
                    KeyCode::Left if !selected.is_multiple_of(columns) => selected - 1,
                    KeyCode::Right
                        if selected % columns < columns - 1
                            && selected + 1 < ACCENT_PRESETS.len() =>
                    {
                        selected + 1
                    }
                    KeyCode::Up if selected >= columns => selected - columns,
                    KeyCode::Down if selected + columns < ACCENT_PRESETS.len() => {
                        selected + columns
                    }
                    _ => selected,
                };
                self.accent_selected = Some(selected);
                self.accent_hex = ACCENT_PRESETS[selected].hex.to_owned();
                self.accent = accent_color(&self.accent_hex)?;
                Action::None
            }
            _ => Action::None,
        };
        Ok(action)
    }

    pub(super) fn handle_custom_accent(
        &mut self,
        key: KeyCode,
        config: &mut AppConfig,
    ) -> Result<Action> {
        let action = match key {
            KeyCode::Enter => {
                let accent = self.custom_accent.to_ascii_uppercase();
                match accent_color(&accent) {
                    Ok(_) => {
                        let action = self.save_accent(config, &accent)?;
                        self.custom_accent.clear();
                        action
                    }
                    Err(error) => Action::Message(error.to_string()),
                }
            }
            KeyCode::Esc => {
                self.custom_accent.clear();
                self.accent_hex = self.accent_selected.map_or_else(
                    || self.accent_original.clone(),
                    |index| ACCENT_PRESETS[index].hex.to_owned(),
                );
                self.accent = accent_color(&self.accent_hex)?;
                Action::Close
            }
            KeyCode::Backspace => {
                self.custom_accent.pop();
                self.preview_custom_accent();
                Action::None
            }
            KeyCode::Char(character) => {
                self.custom_accent.push(character);
                self.preview_custom_accent();
                Action::None
            }
            _ => Action::None,
        };
        Ok(action)
    }

    fn preview_custom_accent(&mut self) {
        let accent = self.custom_accent.to_ascii_uppercase();
        if let Ok(color) = accent_color(&accent) {
            self.accent = color;
            self.accent_hex = accent;
        }
    }

    fn save_accent(&mut self, config: &mut AppConfig, accent: &str) -> Result<Action> {
        config.set_accent(accent)?;
        self.accent = accent_color(accent)?;
        self.accent_hex = accent.to_owned();
        self.accent_original = accent.to_owned();
        self.accent_selected = ACCENT_PRESETS
            .iter()
            .position(|preset| preset.hex == accent);
        Ok(Action::AccentSaved(format!("Accent saved as {accent}")))
    }

    fn connection_test_due(&self, now: Instant) -> bool {
        !matches!(self.connection_status, JiraConnectionStatus::Checking)
            && self.connection_checked_at.is_none_or(|checked_at| {
                now.saturating_duration_since(checked_at) >= JIRA_CONNECTION_TTL
            })
    }

    fn request_connection_test(
        &mut self,
        jira: Option<&JiraClient>,
        jira_error: Option<&str>,
    ) -> String {
        if matches!(self.connection_status, JiraConnectionStatus::Checking) {
            return "Jira connection test is already running".to_owned();
        }
        let Some(client) = jira else {
            let error = jira_error
                .unwrap_or("Jira is unavailable; check credentials and token")
                .to_owned();
            self.connection_status = JiraConnectionStatus::Failed(error.clone());
            self.connection_checked_at = Some(Instant::now());
            return error;
        };
        self.connection_generation = self.connection_generation.wrapping_add(1);
        let generation = self.connection_generation;
        self.connection_status = JiraConnectionStatus::Checking;
        let client = client.clone();
        let sender = self.connection_sender.clone();
        thread::spawn(move || {
            let result = client.test_auth().map_err(|error| error.to_string());
            let _ = sender.send(AsyncEvent::Configuration(Event::Result(
                generation,
                Instant::now(),
                result,
            )));
        });
        "Testing Jira connection...".to_owned()
    }
}

pub(super) fn draw_overview(
    frame: &mut Frame,
    state: &State,
    config: Option<&JiraConfig>,
    jira_error: Option<&str>,
) {
    let area = centered_fixed(frame.area(), 70, 19);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" CONFIGURATION / arrows select / Enter action / g, q, or Esc close ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let connection = match &state.connection_status {
        JiraConnectionStatus::NotChecked if jira_error.is_some() || config.is_none() => {
            "Unavailable".to_owned()
        }
        JiraConnectionStatus::NotChecked => "Not checked".to_owned(),
        JiraConnectionStatus::Checking => "Checking...".to_owned(),
        JiraConnectionStatus::Connected(display_name) => format!("Connected as {display_name}"),
        JiraConnectionStatus::Failed(_) => "Failed - Enter to retry".to_owned(),
    };
    let mut lines = Vec::new();
    for (section, details) in [
        (
            "ZLS",
            [
                (0, "Config file", state.config_path.as_str()),
                (1, "Tasks file", state.task_path.as_str()),
                (2, "Docs path", state.docs_path.as_str()),
            ]
            .as_slice(),
        ),
        (
            "APPEARANCE",
            [(CONFIG_ACCENT, "Accent", state.accent_hex.as_str())].as_slice(),
        ),
        (
            "JIRA",
            [
                (
                    CONFIG_PROJECT,
                    "Project",
                    config
                        .and_then(|config| config.project.as_deref())
                        .unwrap_or("Not selected"),
                ),
                (
                    5,
                    "Site",
                    config.map_or("Not configured", |config| config.site.as_str()),
                ),
                (
                    6,
                    "Email",
                    config.map_or("Not configured", |config| config.email.as_str()),
                ),
                (
                    7,
                    "Cloud ID",
                    config.map_or("Not configured", |config| config.cloud_id.as_str()),
                ),
                (
                    8,
                    "Credentials",
                    if jira_error.is_none() {
                        "Available"
                    } else {
                        "Unavailable"
                    },
                ),
                (9, "API token", "Hidden"),
                (CONFIG_TEST_JIRA, "Status", connection.as_str()),
            ]
            .as_slice(),
        ),
    ] {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(section_separator(section, inner.width));
        lines.extend(details.iter().map(|&(index, label, value)| {
            selectable_detail_line(label, value, state.selected == index, state.accent)
        }));
    }
    let mut connection_error_lines = 0;
    if let JiraConnectionStatus::Failed(error) = &state.connection_status {
        let wrapped = wrap_fixed_width(&format!("Error: {error}"), usize::from(inner.width.max(1)));
        let max_lines = usize::from(inner.height.saturating_sub(1));
        connection_error_lines = wrapped.len().min(max_lines);
        lines.extend(
            wrapped
                .into_iter()
                .take(max_lines)
                .map(|line| Line::styled(line, Style::default().fg(Color::Yellow))),
        );
    }
    let mut selected_line = match state.selected {
        0..=2 => state.selected + 1,
        CONFIG_ACCENT => 6,
        selected => selected + 5,
    } as u16;
    if state.selected == CONFIG_TEST_JIRA && connection_error_lines > 0 {
        selected_line = selected_line.saturating_add(connection_error_lines as u16);
    }
    let scroll = selected_line.saturating_add(1).saturating_sub(inner.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

pub(super) fn draw_accent(frame: &mut Frame, state: &State) {
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

pub(super) fn draw_custom_accent(frame: &mut Frame, state: &State) {
    let area = centered_fixed(frame.area(), 70, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(state.custom_accent.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Custom accent / #RRGGBB / Enter save / Esc back"),
        ),
        area,
    );
    frame.set_cursor_position((
        area.x + state.custom_accent.chars().count() as u16 + 1,
        area.y + 1,
    ));
}

fn accent_color(hex: &str) -> Result<Color> {
    let (red, green, blue) = parse_hex_color(hex)?;
    Ok(Color::Rgb(red, green, blue))
}

fn selectable_detail_line(
    label: &str,
    value: &str,
    selected: bool,
    accent: Color,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{}{label:<12} ", if selected { "> " } else { "  " }),
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

fn wrap_fixed_width(value: &str, width: usize) -> Vec<String> {
    value
        .chars()
        .collect::<Vec<_>>()
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DocsConfig, TasksConfig, UiConfig};
    use ratatui::Terminal;

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

    fn state() -> State {
        let (sender, _receiver) = std::sync::mpsc::channel();
        State::new(
            String::new(),
            String::new(),
            String::new(),
            DEFAULT_ACCENT.to_owned(),
            sender,
        )
        .unwrap()
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
    fn fixed_width_wrapping_preserves_unicode_characters() {
        assert_eq!(wrap_fixed_width("abçdé", 2), ["ab", "çd", "é"]);
        assert!(wrap_fixed_width("", 2).is_empty());
    }

    #[test]
    fn accent_grid_stays_in_column_at_edges() -> Result<()> {
        let mut state = state();
        state.accent_selected = Some(1);
        state.accent_columns = 4;
        let mut config = test_config();
        state.handle_accent_palette(KeyCode::Up, &mut config)?;
        assert_eq!(state.accent_selected, Some(1));

        state.accent_selected = Some(13);
        state.handle_accent_palette(KeyCode::Down, &mut config)?;
        assert_eq!(state.accent_selected, Some(13));

        state.accent_columns = 2;
        state.accent_selected = Some(1);
        state.handle_accent_palette(KeyCode::Up, &mut config)?;
        assert_eq!(state.accent_selected, Some(1));
        Ok(())
    }

    #[test]
    fn cancelling_custom_input_restores_the_palette_preview() -> Result<()> {
        let mut state = state();
        state.accent_original = DEFAULT_ACCENT.to_owned();
        state.accent_hex = ACCENT_PRESETS[1].hex.to_owned();
        state.accent = accent_color(ACCENT_PRESETS[1].hex)?;
        state.accent_selected = Some(1);
        state.custom_accent = "#112233".to_owned();
        state.preview_custom_accent();
        assert_eq!(state.accent_hex, "#112233");

        assert!(matches!(
            state.handle_custom_accent(KeyCode::Esc, &mut test_config())?,
            Action::Close
        ));
        assert_eq!(state.accent_hex, ACCENT_PRESETS[1].hex);
        Ok(())
    }

    #[test]
    fn every_configuration_row_is_selectable() {
        let mut state = state();

        for expected in 1..=CONFIG_LAST_WITH_JIRA {
            state.handle_overview(KeyCode::Down, None, None);
            assert_eq!(state.selected, expected);
        }
        state.handle_overview(KeyCode::Down, None, None);
        assert_eq!(state.selected, CONFIG_LAST_WITH_JIRA);
        state.handle_overview(KeyCode::Up, None, None);
        assert_eq!(state.selected, CONFIG_LAST_WITH_JIRA - 1);
    }

    #[test]
    fn overview_keeps_each_selected_row_visible_at_normal_and_narrow_sizes() -> Result<()> {
        let config = JiraConfig {
            site: "https://example.atlassian.net".to_owned(),
            email: "user@example.test".to_owned(),
            cloud_id: "cloud-id".to_owned(),
            project: Some("OBS".to_owned()),
        };
        let labels = [
            "Config file",
            "Tasks file",
            "Docs path",
            "Accent",
            "Project",
            "Site",
            "Email",
            "Cloud ID",
            "Credentials",
            "API token",
            "Status",
        ];
        for (width, height) in [(120, 30), (60, 16)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
            for config in [None, Some(&config)] {
                let mut state = state();
                for (index, label) in labels.iter().enumerate() {
                    state.selected = index;
                    terminal.draw(|frame| draw_overview(frame, &state, config, None))?;
                    let rendered = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>();
                    assert!(
                        rendered.contains(&format!("> {label}")),
                        "{width}x{height}: {label}"
                    );
                    if width == 120 {
                        let positions = labels.map(|label| rendered.find(label).unwrap());
                        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
                        assert!(rendered.contains("Hidden"));
                        assert!(rendered.contains(config.map_or("Not selected", |_| "OBS")));
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn unselected_accent_uses_the_same_style_as_unselected_project() {
        let accent = selectable_detail_line("Accent", "#00FFFF", false, Color::Cyan);
        let project = selectable_detail_line("Project", "OBS", false, Color::Cyan);

        assert_eq!(accent.spans[0].style, project.spans[0].style);
        assert_eq!(accent.spans[1].style, project.spans[1].style);
        assert!(!accent.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn jira_project_row_is_visible_in_a_short_popup() -> Result<()> {
        let backend = ratatui::backend::TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend)?;
        let mut state = State::new(
            "/home/user/a/very/long/configuration/path/zls/config.toml".to_owned(),
            "/home/user/a/very/long/task/storage/path/todo.md".to_owned(),
            "/home/user/a/very/long/documentation/storage/path".to_owned(),
            DEFAULT_ACCENT.to_owned(),
            std::sync::mpsc::channel().0,
        )?;
        let config = JiraConfig {
            site: "https://example.atlassian.net".to_owned(),
            email: "user@example.com".to_owned(),
            cloud_id: "cloud-id".to_owned(),
            project: Some("OBS".to_owned()),
        };
        state.selected = CONFIG_PROJECT;

        terminal.draw(|frame| draw_overview(frame, &state, Some(&config), None))?;
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
    fn jira_connection_result_expires_after_five_minutes() {
        let checked_at = Instant::now();
        let mut state = state();
        state.connection_status = JiraConnectionStatus::Connected("User".to_owned());
        state.connection_checked_at = Some(checked_at);

        assert!(
            !state.connection_test_due(checked_at + JIRA_CONNECTION_TTL - Duration::from_secs(1))
        );
        assert!(state.connection_test_due(checked_at + JIRA_CONNECTION_TTL));
    }

    #[test]
    fn jira_connection_row_is_visible_when_selected() -> Result<()> {
        let backend = ratatui::backend::TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend)?;
        let mut state = state();
        state.selected = CONFIG_TEST_JIRA;
        state.connection_status = JiraConnectionStatus::Connected("Test User".to_owned());

        terminal.draw(|frame| draw_overview(frame, &state, None, None))?;
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Status"));
        assert!(rendered.contains("Connected as Test User"));
        Ok(())
    }

    #[test]
    fn unavailable_jira_result_is_cached() {
        let mut state = state();

        let message = state.request_connection_test(None, Some("missing Jira token"));

        let checked_at = state.connection_checked_at.unwrap();
        assert_eq!(message, "missing Jira token");
        assert!(matches!(
            state.connection_status,
            JiraConnectionStatus::Failed(ref error) if error == "missing Jira token"
        ));
        assert!(!state.connection_test_due(checked_at + Duration::from_secs(60)));
    }

    #[test]
    fn repeated_jira_connection_test_is_ignored_while_checking() {
        let mut state = state();
        state.connection_status = JiraConnectionStatus::Checking;
        state.connection_generation = 4;

        let message = state.request_connection_test(None, Some("unavailable"));

        assert_eq!(state.connection_generation, 4);
        assert_eq!(message, "Jira connection test is already running");
    }

    #[test]
    fn jira_connection_error_is_visible_in_a_short_popup() -> Result<()> {
        let backend = ratatui::backend::TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend)?;
        let mut state = state();
        state.selected = CONFIG_TEST_JIRA;
        state.connection_status = JiraConnectionStatus::Failed(
            "Jira authentication failed because the API token was rejected".to_owned(),
        );

        terminal.draw(|frame| {
            draw_overview(frame, &state, None, Some("authentication failed"));
        })?;
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Status"));
        assert!(rendered.contains("Error: Jira authentication failed"));
        Ok(())
    }
}
