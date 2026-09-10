use serde_json::{Value, json};

pub fn guide() -> Value {
    json!({
        "schema_version": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "purpose": "ZLS is a brief TUI and Markdown-backed task record for human and agent collaboration.",
        "boundaries": [
            "ZLS is not an agent runtime, Datadog proxy, or workflow database.",
            "Task Markdown is the durable human-visible record; ZLS does not parse workflow sections or keep a second representation."
        ],
        "entrypoint": "zls context <task>",
        "self_discovery": ["zls --help", "zls <command> --help"],
        "commands": [
            {"command": "zls context <task>", "effect": "read", "description": "Load the local task and its documentation; task.jira is the canonical linked key, issue is null unless fetched.", "examples": ["zls context TASK_ID", "zls context TASK_ID --format markdown"]},
            {"command": "zls context <task> --jira", "effect": "external_read", "description": "Include current linked Jira issue details without comments.", "examples": ["zls context TASK_ID --jira"]},
            {"command": "zls today --json", "effect": "read", "description": "Discover today's tasks and stable IDs.", "examples": ["zls today --json"]},
            {"command": "zls history --json", "effect": "read", "description": "Discover dated tasks and stable IDs.", "examples": ["zls history --json"]},
            {"command": "zls backlog list --json", "effect": "read", "description": "Discover unscheduled tasks and stable IDs.", "examples": ["zls backlog list --json"]},
            {"command": "zls docs show <task>", "effect": "read", "description": "Read existing task Markdown.", "examples": ["zls docs show TASK_ID"]},
            {"command": "zls docs path <task>", "effect": "read", "description": "Locate the linked task document.", "examples": ["zls docs path TASK_ID"]},
            {"command": "zls docs append <task>", "effect": "local_write", "description": "Append exact content to the durable task record and return a JSON write acknowledgement.", "examples": ["zls docs append TASK_ID --file update.md", "zls docs append TASK_ID --stdin"]},
            {"command": "zls docs set <task>", "effect": "local_write", "description": "Replace the complete task document and return a JSON write acknowledgement; preserve recorded decisions when rewriting.", "examples": ["zls docs set TASK_ID --file notes.md", "zls docs set TASK_ID --stdin"]}
        ],
        "workflow": [
            {"step": "load_context", "instruction": "Start with zls context <task>. Add --jira only when current issue details are required."},
            {"step": "interpret_intent", "instruction": "Treat the one-line task as intent, not a complete specification."},
            {"step": "discover", "instruction": "Perform read-only discovery before proposing or changing external resources."},
            {"step": "record_design", "instruction": "Append discovery evidence, a recommended design, success criteria, and explicit open decisions to the task document."},
            {"step": "resolve_decisions", "instruction": "Ask the user about every unresolved design choice. Do not infer approval from silence or continue past an open decision."},
            {"step": "record_answers", "instruction": "Record answers in task Markdown using - [ ] for unanswered decisions and - [x] for answered decisions with the recorded answer. Checked and unchecked decisions must survive future agent sessions."},
            {"step": "missing_evidence", "instruction": "If required telemetry or evidence is missing, stop and propose options. Do not create an approximation or supporting telemetry automatically."},
            {"step": "create_drafts", "instruction": "When the task explicitly requests creation and all design decisions are resolved, external drafts may be created without another confirmation."},
            {"step": "verify", "instruction": "Verify created drafts by reading them back and checking live evidence where applicable."},
            {"step": "record_outcome", "instruction": "Append work completed, validation, and remaining work. Publishing and Jira comments are separate actions requiring their own future workflow."}
        ],
        "documentation_sections": ["Discovery", "Proposed Design", "Success Criteria", "Open Decisions", "Work Completed", "Validation", "Remaining"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn contract_and_examples_match_the_cli() {
        let guide = guide();
        assert_eq!(guide["schema_version"], 1);
        assert_eq!(guide["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(guide["entrypoint"], "zls context <task>");
        assert_eq!(
            guide["documentation_sections"],
            json!([
                "Discovery",
                "Proposed Design",
                "Success Criteria",
                "Open Decisions",
                "Work Completed",
                "Validation",
                "Remaining"
            ])
        );
        for command in guide["commands"].as_array().unwrap() {
            assert!(
                ["read", "local_write", "external_read", "external_write"]
                    .contains(&command["effect"].as_str().unwrap())
            );
            for example in command["examples"].as_array().unwrap() {
                crate::Cli::try_parse_from(example.as_str().unwrap().split_whitespace()).unwrap();
            }
        }
        let workflow = guide["workflow"].as_array().unwrap();
        let instruction = |step| {
            workflow.iter().find(|item| item["step"] == step).unwrap()["instruction"]
                .as_str()
                .unwrap()
        };
        assert!(
            instruction("resolve_decisions")
                .contains("Do not infer approval from silence or continue past an open decision")
        );
        assert!(instruction("missing_evidence").contains("stop and propose options"));
        assert!(
            instruction("missing_evidence")
                .contains("Do not create an approximation or supporting telemetry automatically")
        );
        assert!(
            instruction("create_drafts")
                .contains("explicitly requests creation and all design decisions are resolved")
        );
        assert!(instruction("record_answers").contains("- [ ]"));
        assert!(instruction("record_answers").contains("- [x]"));
        assert!(
            instruction("record_outcome")
                .contains("Publishing and Jira comments are separate actions")
        );
    }
}
