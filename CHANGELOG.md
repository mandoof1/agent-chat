# Changelog

## 0.3.3 — 2026-10-14

Subagents, MCP servers, the terminal client's thinking in full, a loop guard, "continue" after
Stop picking up where the agent was, and fixes from running a 4B model for real.

### Stop, then "continue", picks up where it was
- After you stop an agent, the next message you send carries the end of what the stopped reply
  had thought (up to 10% of the window, at most 24,000 characters), like a cut-off reply's
  continuation. Before, that thinking was dropped with the rest of the turn's (chat templates
  drop older thinking anyway), so the model saw an empty reply and started over. It applies to
  a reply stopped mid-stream and to one stopped while its tool ran; once you send another
  message after that, it goes like any older turn's thinking.

### Loop guard
- A reply that keeps writing the same passage is stopped while it streams: once the newest 200
  characters of its thinking or answer have appeared three times over at least 2,000
  characters. The model gets the reply up to the loop, with the passage once, and is told to
  act on it rather than write it again. Three such replies in a row end the turn with an error.
  Length alone never stops a reply. Replayed over 305 long replies from real Qwen chats, it
  stopped 12, each repeating long passages word for word (the worst had thought one plan out 19
  times, 115K characters, before it was stopped by hand), and none of the others.
- The same tool call returning the same result a third time in one task gets a note in its
  result saying that repeating it changes nothing; the fifth ends the turn. Outputs cleared to
  save context don't count.
- Both clients show what the guard did, live and in saved chats. Settings → Loop guard turns it
  off (the terminal client's Settings form has the same switch).

### Subagents
- A new `subagent` tool lets an agent hand one self-contained task to a fresh copy of itself:
  same instructions and tools, an empty context, a report back. Searching a codebase, reading
  through many pages or doing one part of a bigger job no longer fills the agent's own context
  with every file and page it looked at; only the report comes back. Coder, Researcher and
  Reviewer have it (existing installs get it once, and keep it off if you untick it); tick
  "Start subagents" in any agent's tools to give it to others.
- Subagents started in one reply run one after the other by default, so each keeps its prompt
  in the server's cache. Settings → Agents → **Subagents at once** runs more side by side (0: as
  many as llama-server has slots). Not the slots by default: llama-server's automatic
  `--parallel` gives four slots sharing one context, which four subagents can fill.
- A subagent can't see the conversation or ask anything back, and can't start subagents of its
  own. Each one gets its own read-only chat, nested in the trace under its task's label like a
  handoff (`↳ subagent Part 1` in the terminal client) and listed as a subagent's chat. A model
  error ends that subagent with an error result instead of failing the whole turn; Stop keeps
  the reports that came back and marks the rest cancelled, in call order.

### Terminal client: thinking you can read
- Thinking shows in full, while it streams and after it ends, so the trace scrolls through all
  of it. It used to show only the last six lines while the model thought and then collapse to
  one line, so most of it was never on screen. **Alt+T** (or "Hide thinking" in Ctrl+K) hides
  it again, remembered for next time; hidden, a block shows its size while it streams. Alt+O
  still shows thinking together with every tool detail.
- Thinking that has ended is wrapped once per width, not again on every frame: a reply in
  progress after 100K characters of thinking redraws in about 1.6 ms (5 ms re-wrapping it all).

### MCP servers as agent tools
- Settings → MCP takes servers in the `mcpServers` JSON shape MCP READMEs give for Claude
  Desktop (stdio servers: a command, args, env). Tick a server under Tools in an agent's
  settings and the agent can use its tools. **Test** starts it and shows its tools and their
  context cost.
- A server starts when an agent that has it begins a turn and keeps running, shared by every
  agent, until its settings change or the app stops. A crash is reported with the end of the
  server's stderr, and the next call starts it again. Stop and the per-server timeout cancel the
  call on the server, and the server's progress messages show live on the tool card.
- Small servers' tools are offered by name (`<server>__<tool>`). A server whose definitions would
  take more than ~4K tokens is searched instead: the agent gets `find_tools` (keyword search
  returning the best matches with their argument schemas) and `call_tool`. REA, the reverse
  engineering server, has 170 tools worth about 67K tokens; a search returns about 1K.
- A call asks you first unless the tool says it is read-only or non-destructive; per server,
  `"confirm"` can be `"always"` or `"never"`. Images a tool returns are saved under `mcp/` in the
  workspace.

### Fixes from a real model
Found by watching Qwen3.5-4B (Q4_K_M, RTX 3050 4 GB, llama.cpp) build a todo app with tests:
- **Shell commands run outside Agent Chat's own virtualenv.** They inherited the app's `.venv`
  on PATH and `VIRTUAL_ENV`, so an agent's `python3` lacked your packages ("No module named
  pytest") and an install could change the app's environment.
- **Edits stopped failing over copied line numbers.** `read_file` numbers lines like `cat -n`
  (a tab, not spaces the model copies as indentation), and `edit_file`, when the text matches
  nowhere, tries again without copied line numbers or one extra indentation, and says so.
  Reading past the end of a file says how long it is instead of "(empty file)".
- **Summaries, titles and memory notes skip thinking** on llama-server
  (`enable_thinking: false`): a compaction took 100–196 s, once returning nothing; now 12–22 s.
- **Clearing old outputs works in small windows.** It kept the three newest outputs, which at
  8k were whole files filling most of the window; it now keeps as many as fit in 20% of it.
  Old `write_file` contents are cleared too (the file holds them), tagged "cleared" like outputs.

### Platforms and CI
- The terminal client is prebuilt for Linux and macOS on x86_64 and ARM. A `vX.Y.Z` tag builds
  all four (`.github/workflows/release.yml`), `scripts/publish-release.sh` publishes them, and
  the installer picks the one for your machine (cargo for anything else).
- CI on every push and pull request: the app and API suites, the web UI in headless Chrome, the
  Rust tests on Linux and macOS, the four terminal-client suites, and the installer end to end
  on Linux and macOS (`tests/install_smoke.sh`).
- macOS: the installer and the launcher broke paths with `//` under bash 3.2 (macOS's
  `TMPDIR`); fixed.

## 0.3.2 — 2026-10-12

### Old tool outputs are cleared before anything is compacted
- In an agent run most of the context is old tool output: files read, command output, pages.
  Past 70% of the compaction point (49% of the window by default) the oldest outputs are
  replaced, in what the model sees, by a one-line note ("Output cleared to save context: it was
  18,400 characters. Run the tool again if you need it."). The chat keeps them: their cards are
  tagged "cleared" in both clients, live and afterwards, and editing or regenerating an earlier
  message brings them back.
- It needs no model call, unlike a summary, and clears in batches (down to 40% of the
  compaction point, and only when that frees 10% of the window), so llama-server re-processes the
  prompt from the first cleared output once per batch instead of after every step. The newest
  three outputs, the current step's, teammates' answers (`ask_agent`) and memory results stay.
  When the server rejects a prompt as too long, outputs are cleared before anything is compacted.
- On a fake llama-server with a prefix cache, three tool-heavy runs that used to compact two or
  three times now never compact, and the server processes 11–50% fewer prompt tokens.
- One tool output may take at most a quarter of the window (start and end kept), so a single
  file or page can't fill an 8k context.

### Fixed
- 0.3.1 gave up on a prompt the server rejected as too long when the server's real limit was far
  below the context size it reports: the check for whether compacting was worth it counted a
  summary sized for the reported window. On overflow, compaction now runs whenever it removes
  anything (as in 0.3.0); a summary that grows the context is still dropped.

## 0.3.1 — 2026-10-12

### Fixed: compaction that repeated itself
- **A chat no longer compacts over and over.** Compaction never checked that it freed anything.
  When the older messages it summarized weren't even sent (earlier cut-off replies, of which only
  the newest goes to the model), it added a summary on top: the context grew ("7.0k → 7.9k
  tokens") and the same thing happened at every cut-off. Now a compaction that would free less
  than 10% of the window is skipped, and one whose summary comes out longer than what it
  replaced is dropped.
- **Compaction leaves room to work.** The word-for-word tail was always 20% of the window,
  whatever else the prompt held, so with a big pasted request, a small window or a long system
  prompt a chat landed just under the trigger and compacted again a step or two later, freeing a
  little each time. The tail now shrinks to make room for what can't be summarized (system
  prompt, tools, the request, the summary), aiming for half the trigger point. Summaries are
  asked to stay short (about 6% of the window).
- The "N → M tokens" on a compaction marker now counts the system prompt and tools, like the
  context meter.
- A cut-off reply hands back at most 10% of the window (still at most 24,000 characters) of its
  thinking, instead of 24,000 characters whatever the window, which nearly filled an 8k–16k one.
- After four cut-offs in a row the turn stops with an error, instead of running until the step
  limit.

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
