# Slash commands

In the interactive TUI, start a message with `/` to open the command picker or
invoke a built-in command directly.

> The exact list is feature- and platform-dependent. Commands can also be
> hidden while a task is running or while you are inside a side conversation.

## Core conversation commands

| Command          | Purpose                                 |
| ---------------- | --------------------------------------- |
| `/new`           | Start a new chat                        |
| `/resume`        | Resume a saved chat                     |
| `/fork`          | Fork the current chat                   |
| `/rename`        | Rename the current thread               |
| `/clear`         | Clear the terminal and start a new chat |
| `/quit`, `/exit` | Exit the TUI                            |

## Session insight and output

| Command         | Purpose                                            |
| --------------- | -------------------------------------------------- |
| `/status`       | Show current session configuration and token usage |
| `/debug-config` | Show config layers and requirement sources         |
| `/diff`         | Show the Git diff, including untracked files       |
| `/copy`         | Copy the last response as Markdown                 |
| `/raw`          | Toggle raw scrollback mode for easier selection    |
| `/mention`      | Mention a file in the composer                     |

## Task and workflow control

| Command                | Purpose                                 |
| ---------------------- | --------------------------------------- |
| `/review`              | Review the current changes for issues   |
| `/compact`             | Compact/summarize conversation history  |
| `/plan`                | Switch to Plan mode                     |
| `/goal`                | Set or inspect a long-running task goal |
| `/side`, `/btw`        | Start an ephemeral side conversation    |
| `/agent`, `/subagents` | Switch active agent threads             |
| `/ps`                  | List background terminals               |
| `/stop`                | Stop background terminals               |

## Runtime configuration

| Command        | Purpose                                                      |
| -------------- | ------------------------------------------------------------ |
| `/model`       | Choose model and reasoning effort                            |
| `/permissions` | Choose what the agent is allowed to do                       |
| `/personality` | Choose a communication style                                 |
| `/keymap`      | Remap TUI shortcuts                                          |
| `/vim`         | Toggle Vim mode in the composer                              |
| `/theme`       | Choose a syntax highlighting theme                           |
| `/title`       | Configure terminal title content                             |
| `/statusline`  | Configure status line content                                |
| `/ide`         | Include selection/open-file context from the IDE integration |

## Extensibility and memory

| Command     | Purpose                             |
| ----------- | ----------------------------------- |
| `/skills`   | Browse or use installed skills      |
| `/hooks`    | View and manage lifecycle hooks     |
| `/mcp`      | List configured MCP tools           |
| `/apps`     | Manage apps                         |
| `/plugins`  | Browse plugins                      |
| `/memories` | Configure memory use and generation |

## Environment and auth

| Command         | Purpose                               |
| --------------- | ------------------------------------- |
| `/logout`       | Log out of the current auth session   |
| `/experimental` | Toggle experimental features          |
| `/realtime`     | Toggle realtime voice mode            |
| `/settings`     | Configure realtime microphone/speaker |

## Notes

- Some commands accept inline arguments, such as `/review check regressions`.
- The popup presentation order is curated in
  `codex-rs/tui/src/slash_command.rs` rather than alphabetical.
- Debug-only or internal commands are intentionally omitted from this guide.

If you are unsure what is available in your current session, type `/` and read
the in-app descriptions.
