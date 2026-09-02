# ZLS

A popup-first daily task list written in Rust and backed by Markdown.

ZLS stores its task-file path in `~/.config/zls/config.toml`. On first run, it keeps using an existing project-level `todo.md`; otherwise it defaults to `~/.local/share/zls/todo.md`.

```toml
[tasks]
path = "/home/luiz/Documents/notes/appoena/todo.md"

[docs]
path = "/home/luiz/.local/share/zls/docs"
```

Edit these paths to move the task store or documentation directory. `ZLS_FILE` and `--file` remain available as temporary task-file overrides.

## Build

```console
make
```

The release executable is created at `target/release/zls`.

Install it system-wide as `/usr/bin/zls`:

```console
sudo make install
```

Use `make install-user` instead to install through Cargo for the current user. `PREFIX`, `BINDIR`, and `DESTDIR` are supported for packaged or staged installations.

## Tmux setup

Add this key binding to `~/.tmux.conf`:

```tmux
bind-key t display-popup -E -w 80% -h 70% -s 'fg=default,bg=default' '/home/luiz/Documents/notes/appoena/zls/target/release/zls ui'
```

Reload tmux with `tmux source-file ~/.tmux.conf`, then press prefix followed by `t`.

Press `?` in the popup to see every keybinding. The compact header and help overlay are generated from the same registry in `src/keybindings.rs`, which is the authoritative place for keybinding descriptions.

Press `g` to inspect the configuration currently loaded by ZLS, including the config and task-file paths, Jira site, account, Cloud ID, project, and credential availability. API tokens are never displayed.

Press `/` to filter visible tasks in real time by text, Jira key, date, or task ID. `Enter` keeps the filtered view so task actions apply to those results; `Esc` clears the filter.

Press `e` to create or edit documentation for the selected task in `$VISUAL`, falling back to `$EDITOR`. ZLS temporarily leaves the TUI while the editor is open, then restores it. Documented tasks display a `[doc]` marker.

The Jira card opens on the right for the selected linked task. On narrow terminals, ZLS switches to full-width Tasks and Jira tabs. Press `s` to select the one Jira project ZLS will use; the selection is saved in the Jira configuration and all subsequent searches are restricted to it. In the Jira search picker, `Enter` links the highlighted issue and `i` imports it as a new local task. Comments are multiline; use `Ctrl-S` to review, then confirm before posting.

When the popup opens, unfinished tasks from previous days are automatically carried into today. Completed tasks remain under their original date in history. The operation is idempotent, so reopening the popup does not duplicate or rewrite already-carried tasks.

Backlog tasks have no scheduled date and live under a permanent `## Backlog` section in the Markdown file. Automatic carry-over ignores them. Press `b` to show or hide the separately titled `BACKLOG` pane beneath `TASKS`. Press `p` to promote the selected backlog entry into today.

Press `M` on any visible task to open the move selector. Tasks moved to tomorrow are stored under tomorrow's dated section and remain hidden until that day; automatic carry-over only moves overdue tasks, never future tasks. Use history mode (`h`) if you need to find and reschedule a future task.

The same operation is available through the CLI:

```console
./target/release/zls move 1 backlog
./target/release/zls move 1 tomorrow
./target/release/zls move TASK_ID today
./target/release/zls move TASK_ID 2026-09-10
```

Backlog CLI operations are also available:

```console
./target/release/zls backlog add "Investigate a future improvement"
./target/release/zls backlog list
./target/release/zls backlog list --json
./target/release/zls backlog promote 1
```

## CLI

```console
./target/release/zls add "Prepare the presentation"
./target/release/zls today
./target/release/zls done 1
./target/release/zls history
./target/release/zls carry
./target/release/zls status
```

Use `today --json` or `history --json` for scripts and AI tools. Tasks can be changed by displayed number or stable ID.

## Task documentation

Each task can own one Markdown document under the configured `[docs] path`. The document is created lazily as `<task-id>.md`, linked explicitly from the task metadata, and never rewritten by ZLS after creation.

```console
./target/release/zls docs edit 1
./target/release/zls docs show TASK_ID
./target/release/zls docs path TASK_ID
```

For AI tools, `context` emits JSON containing the local task, its complete documentation, and its Jira correlation. Use `--format markdown` for readable output or `--jira` to fetch current issue details without comments. The default command makes no network requests.

```console
./target/release/zls context TASK_ID
./target/release/zls context TASK_ID --format markdown
./target/release/zls context TASK_ID --jira > context.json
```

## Jira Cloud

Create a scoped API token at <https://id.atlassian.com/manage-profile/security/api-tokens> with these Jira scopes:

```text
read:jira-work
write:jira-work
read:jira-user
```

Configure and verify your account:

```console
./target/release/zls jira auth
./target/release/zls jira test
```

Open the popup and press `s` to choose your project from the projects visible to your Jira account. You can also configure it through the CLI:

```console
./target/release/zls jira projects
./target/release/zls jira project OBS
./target/release/zls jira project
```

The final command prints the currently selected project.

`jira auth` prompts for the Jira site, account email, and token. It discovers the Jira Cloud ID automatically and stores only non-secret configuration in `~/.config/zls/config.toml`. The token is stored through the Linux Secret Service using `secret-tool`. If Secret Service is unavailable, export `ZLS_JIRA_TOKEN` before running ZLS; the token is never written to the configuration or task file.

Install `secret-tool` through your distribution's `libsecret-tools` or `libsecret` package if needed.

Jira CLI operations are also available:

```console
./target/release/zls jira search "missing traces"
./target/release/zls jira show OBS-482
./target/release/zls jira link 1 OBS-482
./target/release/zls jira unlink 1
./target/release/zls jira import OBS-482
./target/release/zls jira comment OBS-482 "Investigation update"
./target/release/zls jira comment OBS-482 --body-file jira-comment.md
./target/release/zls jira comment OBS-482 --stdin < jira-comment.md
```

Comment text, `--body-file`, and `--stdin` are mutually exclusive. Use `--body-file -` as another way to read the body from stdin.

Issue cards include a structured Details section with creator, reporter, parent, labels, Sprint, issue type, components, fix versions, timestamps, status, priority, assignee, subtasks, issue links, and the five newest comments. Press `t` to open a dropdown containing only the status transitions Jira currently allows for that issue.

Status transitions are also available through the CLI:

```console
./target/release/zls jira transitions OBS-482
./target/release/zls jira transition OBS-482 31
```

Fetched cards are cached under `~/.cache/zls/jira`; when Jira is unavailable, the last card is shown with a `CACHED` marker. Comment writes and status transitions are never queued offline.

On the first change to a separator-based file, ZLS saves the original as `todo.md.legacy.bak`. The first section becomes today and older sections are retained as open tasks under `Legacy 1`, `Legacy 2`, and so on.

To show a compact reminder in the tmux status bar:

```tmux
set-option -g status-interval 30
set-option -ag status-right ' #(/home/luiz/Documents/notes/appoena/zls/target/release/zls status)'
```

## Development

```console
make test
make lint
make check
make install
make install-user
```
