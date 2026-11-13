<div align="center">

# tern-claude

**Your Claude Code sessions as cards inside [Tern](https://stencil.so/tern).**

Every session on the machine, one card each. Continue one, fork it, hand it to a fresh agent with a short brief, or hide it. Running sessions stay pinned at the top with their status and memory use. One Rust binary, no runtime to install.

[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![rust 2024](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)
[![tern plugin](https://img.shields.io/badge/tern-plugin-6f42c1.svg)](https://stencil.so/tern)

</div>

Built on [`tern-sdk`](https://crates.io/crates/tern-sdk). It reads the transcripts Claude Code already keeps under `~/.claude` and never writes there.

## Why

Each open Claude Code session is a live process holding a few hundred MB. A finished session does not need one: its transcript on disk is the bookmark. The board shows every session as a card and only starts a process when you pick one up again.

## Features

- One card per session: title (Claude's own, or your custom one), where it was opened (CLI or VS Code), last activity, folder, branch, prompt count and the last prompt you typed.
- Running sessions pinned at the top, busy before idle, with the session name, status and working-set memory.
- **Continue**: resumes a finished session in a new tab (`claude --resume`). If the session is already running in this Tern window, it focuses that pane. If it is running somewhere else (VS Code, another terminal), it opens a fork so two processes never write one transcript.
- **Fork**: a new session that starts from this one's full context (`--fork-session`).
- **Brief**: writes a short handoff note (title, folder, last prompt, last reply, transcript path) and starts a fresh session that reads it. A few hundred tokens instead of the whole context.
- **Close**: closes the Tern pane of a running session. The transcript stays, so the card becomes a plain Continue card.
- **Hide / Restore**: takes a card off the board. Only the board's own `hidden.json` changes.
- Filter with `/` across title, last prompt, folder, branch, session id and status.
- Sessions nobody typed into (IDE-opened shells) are left out unless they are running.
- Fast: transcripts are re-read only when their size or modified time changes, and the index is cached on disk.

## Install

You need [Tern](https://stencil.so/tern), [Claude Code](https://claude.com/claude-code) on your `PATH`, and a stable Rust toolchain (2024 edition, Rust 1.85 or newer).

```sh
git clone https://github.com/Fiasco071/tern-claude
cd tern-claude
cargo install --path .
tern plugin link plugin
```

Then open the Tern command palette:

| Command | What it does |
|---|---|
| Claude: Sessions | Opens the board in a new tab |
| Claude: Sessions Beside Current Pane | Opens it in a split to the right |

Reinstalling while a board is open fails with "Access is denied" on Windows, because the board is running the exe being replaced. Close the board tab first.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Select a card |
| `g` / `G`, `Home` / `End` | First / last card |
| `PageUp` / `PageDown` | Move eight cards |
| `Enter` | Continue (or Focus / Attach for a running session) |
| `f` | Fork |
| `b` | Brief a fresh session |
| `c` | Close the running session's pane |
| `d` / `x` | Hide, or restore a hidden card |
| `/` | Filter (`Enter` keeps it, `Esc` clears it) |
| `H` | Show or hide hidden cards |
| `r` | Refresh now |
| `q` / `Esc` / `Ctrl+C` | Quit |

Every card also has buttons for the same actions. The board refreshes on its own every 30 seconds.

Do not press `c` on the card of the session you are typing in from another pane unless you mean to end it.

## Command line

```text
tern-claude                         the board (needs Tern)
tern-claude list                    the sessions as a table, in any shell
tern-claude index [--why NAME] [--quiet]
                                    rebuild the index; --quiet never exits non-zero
tern-claude selftest [ROLE]         a 20 s frame counter, for checking a surface role
```

## Optional: keep the index fresh with hooks

The board rebuilds the index itself, so this is not required. If you want `tern-claude list` and a newly opened board to be current at once, add these to `~/.claude/settings.json` beside any hooks you already have:

```json
{
  "hooks": {
    "SessionStart": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "tern-claude index --quiet --why SessionStart", "timeout": 10, "async": true }] }
    ],
    "UserPromptSubmit": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "tern-claude index --quiet --why UserPromptSubmit", "timeout": 10, "async": true }] }
    ],
    "Stop": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "tern-claude index --quiet --why Stop", "timeout": 10, "async": true }] }
    ]
  }
}
```

In `--quiet` mode errors go to the log and the exit code stays 0, so a broken index never blocks Claude.

## What it reads and writes

Reads, under `~/.claude` (or `CLAUDE_CONFIG_DIR`):

- `projects/<slug>/<id>.jsonl`: the transcripts.
- `history.jsonl`: typed prompts for CLI sessions.
- `sessions/<pid>.json`: running sessions. The `.key` files beside them hold tokens and are never opened.

Writes, only under `%APPDATA%\tern-claude` on Windows and `~/.config/tern-claude` elsewhere:

| File | Contents |
|---|---|
| `index.json` | The cached session index |
| `hidden.json` | Ids of hidden cards |
| `handoff-<id>.md` | Brief notes. These hold your last prompt and Claude's last reply in plain text |
| `index.log`, `board.log` | What the indexer and board did; each rolls over at 256 KB |

The index, logs and handoff notes contain session titles and prompt text. Treat that folder like the transcripts themselves.

## Platform support

| OS | Status |
|---|---|
| Windows | Built and used daily |
| Linux | Has code paths (`/proc`), not tested by hand |
| macOS | Not tested. Running-session detection relies on `/proc`, so running sessions will not be pinned |

Checked against Claude Code 2.1.318. The transcript and session file formats are not a public contract, so a Claude Code update can change what the board sees.

## Related

- [tern-mssql](https://github.com/zachristmas/tern-mssql): a SQL Server client in Tern, built the same way.
- [tern-azdo](https://github.com/zachristmas/tern-azdo): Azure DevOps pull requests in Tern.

## License

[MIT](LICENSE)
