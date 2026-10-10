# Changelog

## 0.3.0 — 2026-10-10

Install it with one command, run it like Claude Code, and a terminal client that survives
whatever you paste into it.

### Install and launch
- `curl -fsSL https://agent-chat-i4sv.onrender.com/install.sh | bash` installs for your user
  (no sudo): the app, a prebuilt static terminal client on Linux x86_64 (SHA-256 checked),
  uv if missing, and the `agent-chat` command. Data lives outside the code folder, so updates
  and uninstalls keep it. The landing page and the script are served from `site/` on Render.
- `agent-chat` starts the app in the background when nothing answers on the port and opens the
  terminal client; `web`, `status`, `stop`, `restart`, `logs`, `update [VER]`, `uninstall
  [--purge]`. Updates swap the code atomically and roll back if anything fails.
- `scripts/build-release.sh` builds the static (musl) client and its checksum for a release.

### Slash commands
- `/` at the start of the message box, in the web UI and the terminal client, opens a menu of
  27 commands: `/help`, `/new`, `/resume`, `/rename`, `/pin`, `/branch`, `/retry`, `/edit`,
  `/stop`, `/compact [instructions]`, `/export`, `/copy`, `/delete`, `/agents`, `/model`,
  `/memory`, `/remember`, `/permissions`, `/settings`, `/routines`, `/files`, `/verbose`,
  `/theme`, `/context`, `/usage`, `/status`, and `/quit` in the terminal. Aliases (`/clear`,
  `/fork`, `/config`, `/cost`, …) match too. Commands never reach the agent and run while it
  works; ones that would change its work wait and keep your text. `//` sends a literal `/`.
- `POST /api/chats/{id}/compact` takes optional `instructions` for what the summary must keep.
- A test keeps the two clients' command lists identical.

### Chat footer
- The end of every chat shows how many times it was compacted and how long it has been going
  (`compacted 2× · going for 3h 12m`), live, in both clients; `/usage` and `/context` too.

### Terminal client: pastes and rendering
- **Large pastes no longer break the screen.** Pasted `\r` / `\r\n` became raw carriage returns
  that sent the cursor to column 0, and tabs in a sent message moved the real cursor, leaving
  stale text down the right side. Pastes are now normalized (line endings, escape codes and
  control characters), every string drawn goes through one sanitizer, and a final pass over
  each frame blanks any control character left.
- Pastes over 10 lines or 1,000 characters go in as a `[Pasted text #N +L lines]` token and
  are sent in full. Terminals without bracketed paste are detected by timing, so a pasted
  newline is a line break, not Enter, and keys inside a paste never run shortcuts.
- Emoji whose width terminals disagree on (VS16, ZWJ sequences, skin tones, flags, keycaps)
  are drawn in a form that keeps rows aligned; text wraps by display width (CJK, emoji).
- No panics at any size (1×1 up), dialogs that scroll, a restored terminal after a panic,
  toasts capped at three with repeats counted, one redraw per batch of input.

### Terminal client: fixes
- Approvals: `y`/`a`/`n` only when the trace has the focus; Alt+Y/Alt+A/Alt+N from anywhere.
  Before, the first letter typed into an empty box or the sidebar filter could answer one.
- One event stream per chat visit (switching A→B→A applied every event twice); UTF-8 split
  across network reads; a deleted or missing chat closes once instead of retrying forever.
- Ctrl+C closes a dialog, clears the filter or the box, and only then quits.
- Mouse clicks and focus follow what is on screen at narrow widths; welcome-page numbers
  start chats; the Calendars form adds on Enter; memory delete asks first; prompts move their
  cursor; the files drawer refreshes; the palette keeps its selection in view; edits that
  fail keep your text; and about sixty more found by a feature-by-feature audit.

### Tests
- New suites: `tests/e2e_tui_keys.py`, `tests/e2e_tui_render.py`, `tests/e2e_tui_slash.py`,
  `tests/e2e_ui_more.py`; the Rust unit tests grew from 4 to over 200. Screen corruption is
  checked by comparing the screen with a forced full repaint.
- `TEST_PORT_BASE` moves every port in `tests/run_all.sh`; `SKIP_TUI=1` skips the terminal.

## 0.2.0 — 2026-10-09

A product release: a new UI, and the backend pieces a daily driver needs.

### Terminal client
- `tui/`: a Rust (ratatui) client for the same app. Sidebar with agents and live status, chats
  grouped by day with unread dots, every reply as a trace with nested handoffs and approvals,
  live shell output, the command palette, files drawer with a viewer, memory, routines,
  settings, agent/routine/email/calendar editors, attachments, branching, pin/rename/export,
  mouse support and OSC 52 copy. Tested end to end in a pseudo-terminal (`tests/e2e_tui.py`).

### New look
- **Trace console.** Every reply is a trace: a line in the agent's color with its steps
  (thinking, tool calls, handoffs to other agents in their own nested trace, the answer).
- One sidebar: agents with a live status word (working, needs you, → Coder), then chats.
  The patch-panel rail and the cords are gone; delegation is drawn inside the reply.
- New type (Bricolage Grotesque for the UI, IBM Plex Mono for data), a near-black palette
  with one electric-blue accent and amber for "needs you"; a light theme on the same tokens.

### New
- **Command palette** (Ctrl+K): find chats by title or by what was said in them, start a chat
  with any agent, and run every command from the keyboard.
- **Chat list** grouped by day, with pinned chats on top, unread dots for replies that
  arrived while you were elsewhere, and a context menu (rename, pin, export, delete) on
  right-click or the ⋯ button. "Delete all idle chats" for an agent or for everything.
- **Branching**: start a new chat from any point of an existing one ("Branch from here" on
  any message, or from the end). The original is untouched.
- **Workspace files** drawer (Ctrl+.): browse what the agent's file and shell tools can see,
  open any file in a viewer with syntax highlighting, download it. Refreshes itself when an
  agent writes files or runs commands.
- **Images**: attach, paste or drop images; they go to vision models as image parts (and
  show as thumbnails in the chat). If the model can't take images, the text is sent alone
  with a notice instead of failing the turn. Browser screenshots show inline.
- **Live shell output**: a running command's output streams into its card.
- **Approve all for this run**: one click on an approval card approves everything else the
  agent asks for until that run ends. Calls that ran this way are marked.
- **Model-written chat titles** after the first reply (off in Settings → Agents; a name you
  typed yourself is never replaced).
- **Try again** on error cards, an offline bar when the model server isn't reachable, and
  a welcome-page note that points at Settings.
- **Agent editor**: preview the exact system prompt a chat starts with, duplicate an agent,
  export it as JSON, import one from a file; a Data Analyst template. Drag agents on the rail
  to reorder them.
- **Settings** in tabs (model, agents, web & browser, email, calendars) with the server kind
  shown by "Test connection".
- Per-reply stats (tok/s, tokens in/out, thinking time) in the turn header; timestamps on
  hover; a "Newest" pill when you scroll up during a reply; keyboard shortcuts dialog (?);
  collapsible chat list (Ctrl+B); JSON export of a chat with its agent-to-agent work.
- API: `/api/health`, `POST /api/chats/{id}/fork`, `POST /api/chats/delete`,
  `GET /api/chats/{id}/export.json`, `PATCH` pinned, `PUT /api/agents/order`,
  `GET /api/agents/{id}/prompt`, `GET /api/workspace`, `GET /api/workspace/file`.

### Fixed
- Background work (naming chats, memory extraction) no longer stalls for every chat while one
  chat waits for an approval.
- The sidebar could show a stale status when a status event arrived while the chat list was
  being refetched.
- Memory dedup missed near-duplicates that differed only by a trailing period.
- `timings_per_token` is only sent to llama-server (other servers may reject unknown fields).
- `web_fetch` no longer reads arbitrarily large downloads into memory (2 MB cap).
- Internal errors are logged, not only shown in the chat.

### Internal
- `static/js/*`: the UI is split into modules (state, util, ui, sidebar, chat, composer,
  files, agents, settings, memory, routines, palette, notify, main). No build step.
- Chat list summaries are cached by file mtime.
- Tests: `tests/test_unit.py` (unit), `tests/e2e_v2.py` (new API features),
  `tests/e2e_ui.py` (the real UI in headless Brave), and `tests/run_all.sh` to run everything.
- Workspace files are served with `Content-Security-Policy: sandbox`; HTML/SVG as text.

## 0.1.0

First release: agents that delegate to each other, shared memory, email and calendar agents,
a browser agent, routines, self-compacting chats, the message queue, and live tool calls.
