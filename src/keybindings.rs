#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Tasks,
    Jira,
    Configuration,
    Navigation,
    Dialogs,
}

impl Section {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Tasks => "TASKS",
            Self::Jira => "JIRA",
            Self::Configuration => "CONFIGURATION",
            Self::Navigation => "NAVIGATION",
            Self::Dialogs => "DIALOGS AND EDITORS",
        }
    }
}

pub struct Keybinding {
    pub keys: &'static str,
    pub description: &'static str,
    pub section: Section,
    pub compact: bool,
}

pub const KEYBINDINGS: &[Keybinding] = &[
    Keybinding {
        keys: "?",
        description: "show every keybinding",
        section: Section::Navigation,
        compact: true,
    },
    Keybinding {
        keys: "q / Esc",
        description: "close ZLS or cancel the current dialog",
        section: Section::Navigation,
        compact: true,
    },
    Keybinding {
        keys: "j/k / arrows",
        description: "move selection down or up",
        section: Section::Navigation,
        compact: false,
    },
    Keybinding {
        keys: "Tab",
        description: "switch Tasks and Jira tabs on narrow terminals",
        section: Section::Navigation,
        compact: false,
    },
    Keybinding {
        keys: "PgUp/PgDn",
        description: "scroll the Jira card",
        section: Section::Navigation,
        compact: false,
    },
    Keybinding {
        keys: "a",
        description: "add a task for today",
        section: Section::Tasks,
        compact: true,
    },
    Keybinding {
        keys: "/",
        description: "filter visible tasks as you type",
        section: Section::Tasks,
        compact: true,
    },
    Keybinding {
        keys: "e",
        description: "edit documentation for the selected task",
        section: Section::Tasks,
        compact: true,
    },
    Keybinding {
        keys: "Space / d",
        description: "complete or reopen the selected task",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "h",
        description: "toggle today and task history",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "W",
        description: "open the weekly completion retrospective",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "B",
        description: "add a task directly to backlog",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "b",
        description: "show or hide the backlog pane",
        section: Section::Tasks,
        compact: true,
    },
    Keybinding {
        keys: "p",
        description: "promote the selected backlog task into today",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "M",
        description: "move task to today, tomorrow, or backlog",
        section: Section::Tasks,
        compact: true,
    },
    Keybinding {
        keys: "C",
        description: "manually carry overdue unfinished tasks",
        section: Section::Tasks,
        compact: false,
    },
    Keybinding {
        keys: "l",
        description: "search Jira and link the selected task",
        section: Section::Jira,
        compact: true,
    },
    Keybinding {
        keys: "u",
        description: "unlink the selected task from Jira",
        section: Section::Jira,
        compact: false,
    },
    Keybinding {
        keys: "t",
        description: "change the linked Jira issue status",
        section: Section::Jira,
        compact: false,
    },
    Keybinding {
        keys: "c",
        description: "write a comment on the linked Jira issue",
        section: Section::Jira,
        compact: false,
    },
    Keybinding {
        keys: "m",
        description: "load more Jira comments",
        section: Section::Jira,
        compact: false,
    },
    Keybinding {
        keys: "r",
        description: "refresh the linked Jira card",
        section: Section::Jira,
        compact: false,
    },
    Keybinding {
        keys: "g",
        description: "show and edit the loaded ZLS configuration",
        section: Section::Configuration,
        compact: false,
    },
    Keybinding {
        keys: "Enter",
        description: "select or apply; insert a newline while editing comments",
        section: Section::Dialogs,
        compact: false,
    },
    Keybinding {
        keys: "Backspace",
        description: "delete the last character in an active input",
        section: Section::Dialogs,
        compact: false,
    },
    Keybinding {
        keys: "i",
        description: "import highlighted Jira result as a local task",
        section: Section::Dialogs,
        compact: false,
    },
    Keybinding {
        keys: "Ctrl-S",
        description: "review a multiline Jira comment before posting",
        section: Section::Dialogs,
        compact: false,
    },
    Keybinding {
        keys: "y / n",
        description: "post a reviewed comment or return to editing",
        section: Section::Dialogs,
        compact: false,
    },
];

pub fn compact_hint() -> String {
    KEYBINDINGS
        .iter()
        .filter(|binding| binding.compact)
        .map(|binding| {
            format!(
                "{} {}",
                binding.keys,
                binding.description.split_whitespace().next().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entries_are_complete_and_help_is_discoverable() {
        assert!(
            KEYBINDINGS
                .iter()
                .all(|binding| !binding.keys.is_empty() && !binding.description.is_empty())
        );
        assert!(KEYBINDINGS.iter().any(|binding| binding.keys == "?"));
        assert!(!KEYBINDINGS.iter().any(|binding| binding.keys == "s"));
        assert!(compact_hint().contains("? show"));
        for section in [
            Section::Tasks,
            Section::Jira,
            Section::Configuration,
            Section::Navigation,
            Section::Dialogs,
        ] {
            assert!(KEYBINDINGS.iter().any(|binding| binding.section == section));
        }
    }
}
