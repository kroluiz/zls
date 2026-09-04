# ZLS Contribution Handover

This document is the working contract for AI agents and contributors changing ZLS. Keep changes small, preserve existing behavior unless the task explicitly changes it, and put code in the module that owns the relevant state.

## Before Changing Code

1. Work from the repository root: `zls/`.
2. Read the affected module and its tests before proposing a design.
3. Run `git status --short` and do not discard unrelated worktree changes.
4. Check recent commit style with `git log --oneline -10`.
5. Do not commit unless the user explicitly requests it. When commits are requested, use one commit per feature, fix, or refactor and do not bundle independent changes.

Do not read from or write to the user's live task store, Jira credentials, keyring, or `~/.config/zls/config.toml` in tests. Use `tempfile` and injected test state.

## Required Verification

Run the complete verification sequence before committing:

```console
cargo fmt
make lint
make test
make
git diff --check
```

`make lint` runs formatting checks and Clippy with warnings denied. `make` produces the release binary. Report any command that could not be run.

For TUI changes, add focused `ratatui::backend::TestBackend` coverage where rendering, clipping, scrolling, or selection visibility matters. Test narrow popup dimensions as well as normal dimensions.

## Architecture

### Application and persistence

- `src/main.rs`: CLI definitions and top-level command dispatch. Keep business logic in the owning module rather than growing this dispatcher.
- `src/store.rs`: Markdown task storage, task IDs, backlog, carry, moves, completion, and weekly reports. Store mutations rewrite the file into ZLS's canonical format; unsupported Markdown lines are not preserved.
- `src/config.rs`: application configuration and atomic TOML updates. Its `AppConfig` write path preserves unrelated sections and comments, follows config symlinks, and retains Unix permission bits.
- `src/docs.rs`: task-document creation, path validation, editor launching, and symlink rejection.
- `src/jira.rs`: Jira API client, authentication, keyring access, API models, caching, and ADF rendering. This is the backend integration, not the TUI.
- `src/keybindings.rs`: registry for normal TUI bindings. Full Help and compact normal-view hints derive from this file. Weekly controls and their hint currently live separately in `src/ui/week.rs`; update both places when appropriate. Compact labels must be explicit, not inferred from description text.

### TUI

`src/ui/mod.rs` is the composition root. It owns terminal lifecycle, global modes, overlay return behavior, cross-feature effects, the ordered async event receiver, document-editor suspension, and top-level layout composition. Do not move feature state back into it.

- `src/ui/tasks.rs`: normal task selection, history/backlog visibility, filtering, add and move dialogs, task mutations, and task rendering.
- `src/ui/week.rs`: weekly report state, week offset, weekly selection, navigation intents, and rendering.
- `src/ui/jira.rs`: Jira client/config ownership, card state, Jira dialogs, async requests/events, stale-response guards, and Jira rendering.
- `src/ui/configuration.rs`: configuration selection, accent transaction/custom input, connection-test cache, and configuration rendering.
- `src/ui/help.rs`: Help overlay state, scrolling, key handling, and rendering.
- `src/ui/layout.rs`: neutral shared popup geometry only.

Feature modules own their state and return owned actions or intents. The root applies cross-feature effects after the feature borrow ends. Do not pass `&mut ui::State` into a feature module, and do not make feature modules mutate sibling state directly.

## UI State Rules

- Normal tasks and weekly reports have separate selections. Do not reintroduce one shared selection index.
- Weekly view is base content, not a popup mode. Configuration and Help can open over it and must return without closing it.
- Help remembers the overlay that opened it. Closing Help returns to that caller.
- Global `?` must not capture text while a task, search, custom accent, or comment editor is accepting input.
- Jira keys have precedence over normal task keys where bindings overlap.
- When opening a Jira search for a task, capture the stable task ID. Do not recompute the target from the currently selected row when the async result is applied.
- Configuration rows are all selectable. Read-only rows remain selectable but do not mutate values.
- Decorative colors use the configured accent. Semantic success, warning, error, and Jira status colors remain fixed.
- Validate both wide two-pane layouts and narrow Tasks/Jira tab layouts.

## Async Rules

Jira and Configuration workers send feature-owned payloads through the single root `AsyncEvent` channel. This preserves enqueue order across features. Do not add a second private receiver for UI notices.

Overlapping read requests reject stale responses:

- Search and project requests use generations.
- Card refreshes, transition lists, and comment-history requests use generations plus issue-key checks.
- Connection tests use generations and a five-minute completion-time cache.
- A repeated connection test while one is running must not spawn another worker.

Transition and comment mutation completions are intentionally not generation-correlated today. They still report valid write completion and trigger refresh behavior; treat same-key write correlation as separate work rather than silently changing it during a refactor.

Workers may outlive the TUI. Sending to a closed channel must remain harmless. Never block the render loop on network I/O.

## Jira Ownership and Security

`src/ui/jira.rs` owns and exposes the authoritative TUI `JiraConfig` snapshot, including when credentials are unavailable and no client can be created. `JiraClient` also keeps the internal config it needs for requests; project updates must keep the client and the exposed snapshot synchronized. Configuration rendering and document creation read through `ui::jira::State`. Do not add another feature-level snapshot.

API tokens are secrets:

- Store tokens through Linux Secret Service or read `ZLS_JIRA_TOKEN`.
- Never write tokens to TOML, Markdown, logs, tests, snapshots, or error messages.
- Never display token values in the TUI; Configuration shows only `Hidden`.
- Tests must not invoke the live keyring or Jira account.

`src/jira.rs` and `src/ui/jira.rs` have intentionally different responsibilities. Alias the backend module clearly when working inside the UI Jira module.

## Configuration Rules

- `ui.accent` accepts only strict `#RRGGBB` values.
- Default accent is `#00FFFF`.
- Accent preview is transactional: arrows preview, `Enter` saves, and `Esc` restores the saved value.
- TOML migration must handle missing sections and existing partial sections without deleting unknown keys.
- `AppConfig` writes must remain atomic and preserve comments, symlink targets, unrelated sections, and existing permission bits.
- `JiraConfig::save` currently uses direct `fs::write` and TOML reserialization. It is not atomic and does not preserve comments or unknown keys inside `[jira]`; this is known technical debt. Do not claim stronger guarantees without fixing and testing that path.
- Jira connection status is cached in application state for five minutes from request completion. Selecting `Status` and pressing `Enter` forces a refresh unless one is already running.

## Storage and Documentation Invariants

- The Markdown task file is user data. Current Store mutations rewrite it canonically and discard unsupported lines. Do not assume arbitrary Markdown round trips; changing this requires explicit parser/persistence tests.
- Carry is idempotent and moves only overdue unfinished tasks. It must not move backlog or future tasks.
- Backlog entries use the reserved `## Backlog` heading when present; empty backlog sections may be omitted.
- Weekly reports use Monday through Sunday and keep dated entries before undated completions.
- Task documents are created lazily as `<task-id>.md` and linked from task metadata. Only explicit edits and `docs set/append` rewrite them.
- Reject document paths and symlinks that escape the configured documentation root.

## Testing Guidance

Prefer behavior-focused tests next to the owning module:

- Pure feature transitions belong in that feature's test module.
- Cross-feature action application belongs in `src/ui/mod.rs` tests.
- Persistence round trips belong in `store.rs`, `config.rs`, or `jira.rs` tests.
- Use deterministic injected clients/configuration or direct event payloads. Do not depend on network timing.
- For async behavior, test stale generations, same-key out-of-order responses, cancellation, and event ordering.
- For rendering, assert meaningful visible text and selection markers at realistic popup sizes, including approximately 60x16.

Do not weaken or delete a regression test merely to make a refactor compile. Move it to the new owner or replace it with equivalent feature and root-integration coverage.

## Change Discipline

- Prefer the smallest correct implementation.
- Preserve established keybindings and interaction flow unless explicitly asked to change them.
- Update `README.md` when user-facing commands, configuration, keybindings, or behavior change.
- Add or update `src/keybindings.rs` for every user-facing binding change.
- Avoid compatibility layers unless persisted user data or shipped external behavior requires them.
- Keep refactors behavior-preserving and separate from feature changes.
- Stage only files belonging to the current change, and only commit when requested.
- Use concise imperative commit messages matching repository history, for example `extract task UI module` or `reject stale Jira responses`.

## Definition of Done

A change is complete when:

1. State and behavior live in the correct owning module.
2. Cross-feature effects are explicit and applied by the root.
3. User data and secrets remain safe.
4. Regression tests cover the changed behavior and important narrow-layout cases.
5. Formatting, Clippy, tests, release build, and `git diff --check` pass.
6. Documentation is current.
7. If a commit was requested, it contains exactly one coherent change; otherwise the worktree changes are clearly reported.
