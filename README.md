# Agent Chat

A local chat app where every chat belongs to an **agent**: a persona with its own purpose,
system prompt, tools and (optionally) model. Every agent knows the rest of the team and can
give any teammate a task or ask it a question. You watch every step live, and each agent's
work for another agent shows up as its own chat you can open.

Agents share a **memory** about you that grows as you chat, so they get more personal over
time. A **Mail** agent can read, sort, draft and (with your approval) send email, and a
**Planner** reads your calendars. **Routines**
run agents on a schedule (a morning email briefing, say). Long chats **compact themselves**
before they run out of context.

It talks to any OpenAI-compatible server. It's set up for llama.cpp's `llama-server` on
`http://127.0.0.1:8080/v1`, and Ollama, LM Studio and vLLM work too. Everything — chats,
memory, email and calendar credentials — stays on your machine.

## Screenshots

Pick an agent, or let one hand work to another. The rail is a patch panel: a lit jack is an
agent at work, a cord is one agent waiting on another.

![The agent gallery: ten starter agents, each with its own job and tools](docs/img/gallery.png)

Watch delegation live. Here the **Orchestrator** patches in the **Coder**, whose shell command
waits for your approval — the nested card is the Coder's own chat, openable on its own.

![Orchestrator delegating to Coder, with a shell command awaiting approval](docs/img/hero.png)

A shared memory about you grows as you chat; pin the facts every agent should always see.

![The memory panel with pinned and categorized facts](docs/img/memory.png)

Thinking streams into its own block and is never cut short, with a live context meter and tok/s.

![An assistant reply with an expandable thinking block](docs/img/thinking.png)

## Run

```bash
# 1. start your model (this app never starts it for you), e.g.
llama-server --jinja -m your-model.gguf   # tool calling needs --jinja
# 2. start the app
./run.sh
# 3. open http://127.0.0.1:8765
```

On first run, `uv` creates the virtualenv and installs dependencies. Point
**Settings → Base URL** at your server if it isn't on `http://127.0.0.1:8080/v1`.

To have it start by itself when you log in (needed for routines to fire on time), run
`./install-service.sh` once (`--remove` undoes it). It starts only this app, never the model.

## Using it

- **The jack panel** on the left has one socket per agent, each with a label strip and a lamp.
  Click a jack to see that agent's chats; **All** shows the chats you started.
  - **Amber blinking lamp:** the agent is working. Click its jack to watch what it's doing.
  - **Red blinking lamp:** it needs your approval.
  - **Steady amber lamp:** it's waiting on another agent.
  - **Patch cords:** while one agent waits on another, a cord connects their two jacks, and its
    flow shows which way the request went.
- **Theme:** the bottom-right rail button cycles Auto (follows your system), Light and Dark.
- **New chat** starts a chat with the selected agent (or asks which one).
- The **pencil** edits an agent: name, icon, purpose, system prompt, tools, who it can talk to, memory,
  model, temperature and workspace folder. **New** on the rail creates an agent.
- **Memory**, **Routines** and **Settings** sit at the bottom of the rail (see below).
- **Search** (Ctrl+K) matches chat titles and the text of every message, including agents' work for each other.
- **Attach files** by dropping or pasting them, or with 📎. Text files go into the message, and every
  file is also saved to the workspace's `uploads/` folder. **⤓** in the header exports a chat as Markdown.
- **Notifications:** when a reply finishes or needs your approval while the tab is in the background
  (allow notifications when the browser asks).
- Hover a message to **edit and resend** it, **copy** it, or **↻ regenerate** a reply.
- **Stop** cancels a reply mid-stream; the partial text is kept.
- **Message queue** (like Claude Code's): you can keep typing while an agent works. Enter queues
  the message (the button reads **Queue**), and it shows above the message box. The agent gets it at
  its next step: after the tool calls in flight finish and before it writes again, so it can change
  course mid-task. The model sees it marked as sent while it was working. If the agent answers
  first, the queued messages start the next turn right away, joined into one message. While a
  helper agent works, the message waits for the agent you are talking to. ✎ or ✕ takes one message
  back out, and **↑** in an empty message box takes them all back to edit. If the run ends any
  other way (Stop, an error, the step limit), queued messages go back into the message box unsent.
- **Ctrl+O** (or the expand button in the chat header) shows every thinking block and tool call
  in full, including inside delegated work, and new ones open as they stream. Press it again to
  collapse. The choice is remembered.
- **Live tool calls:** while an agent writes a tool call you watch it being written. A file
  being created shows its code appearing line by line with syntax highlighting. Shell commands,
  edits, emails and messages to other agents stream the same way.
- **Permissions:** the chip under the message box shows the mode. "Ask before acting" (the
  default) asks you before every shell command and every email send. Click it, or tick
  "Bypass permissions" in Settings, to let agents act without asking. The chip turns red while
  bypass is on, and every tool call that ran without asking is marked "auto-approved". This
  also applies to routines. Anything an agent reads (web pages, emails) could try to trick it,
  so only bypass when you trust the task.
- The **compact** button in the chat header (two arrows pointing at a line) summarizes older messages on demand.
- The header shows a segmented context meter (`CTX 12K / 85K`) and the last reply's speed in tok/s.
  The meter updates live (`ctx` events): at every step (after tool results), twice a second while
  a reply streams (exact counts from llama-server's `timings_per_token`, or an estimate on other
  servers), and right after compaction.
- Runs continue in the background. Switch chats or reload the page, and the live view picks up where it is.

## Starter agents

| Agent | Purpose | Tools |
|---|---|---|
| 💬 Assistant | General chat | team |
| 📧 Mail | Reads, sorts and answers your email | team, email |
| 📅 Planner | Knows your schedule and plans your days | team, calendar, read/write files |
| 🧭 Orchestrator | Plans tasks and hands them out | team, list/read files |
| 💻 Coder | Writes, runs and fixes code | team, files, shell |
| 🔎 Researcher | Web research with sources | team, web search, fetch, write files |
| 🌐 Browser | Drives a real Brave browser to open, read and act on web pages | team, browser, read/write files |
| ✍️ Writer | Drafts and edits text | team, read/write files |
| 🧐 Reviewer | Code review | team, list/read files |

"team" = the `ask_agent` tool, which reaches every other agent by default. Every agent also
has memory unless you switch it off for that agent. New starter agents added in later versions
appear automatically; ones you deleted stay deleted.

## Tools

| Tool | What it does |
|---|---|
| `list_dir`, `read_file`, `write_file`, `edit_file` | Work on files **inside the agent's workspace folder only**. Paths that escape it are refused. |
| `run_shell` | Runs a command with the workspace as its working directory. **Not sandboxed.** By default every command needs your Approve/Deny in the chat. |
| `web_search` | Top 8 results (titles, URLs, snippets) from your SearXNG instance, see [Web search](#web-search). Without one it scrapes DuckDuckGo, which often blocks it. |
| `web_fetch` | Downloads a page and returns its readable text. |
| `browser_navigate`, `browser_read`, `browser_links`, `browser_click`, `browser_type`, `browser_screenshot`, `browser_close` | Drive a real [Brave](https://brave.com/) browser for pages that need JavaScript, a login, or clicking/typing through steps — where `web_fetch` isn't enough. One shared browser session persists across the calls (same page, same cookies). Headless or visible per **Settings → Browser**; screenshots save into the workspace. See [Browser control](#browser-control). |
| `ask_agent` | Sends a task or a question to another agent. The other agent runs its own tool loop. Its work shows nested in the card and as its own chat. Maximum depth is 3. |
| `remember`, `recall_memory`, `forget_memory` | Manage the shared memory about you. |
| `email_list`, `email_search`, `email_read` | Read your mailbox (reading doesn't mark mail as read). |
| `email_draft` | Saves a draft in your mailbox without sending. |
| `email_send` | Sends an email. Shows you the exact email to Approve or Deny first, unless you turned on bypass permissions. |
| `calendar_events` | Lists events from your connected calendars in local time (default: the next 7 days; a search looks a year ahead). Read-only. |

## How agents talk to each other

- **Everyone knows the team.** Each agent's system prompt gets its own name and job, plus the
  name and purpose of every other agent.
- **Everyone can reach everyone** by default ("Every other agent, including ones added later"
  in the editor). Untick it to choose specific agents, or untick the "Talk to other agents"
  tool to make an agent work alone.
- **Each conversation between two agents is its own chat.** It's listed under the agent
  doing the work, marked "↪ … for Orchestrator". Open it (or click the busy agent's icon) to
  watch live. These chats are read-only; you reply in the chat that started them.
- **Conversations are remembered.** Asking Coder again continues where it left off,
  including in later turns of the same chat. The caller can pass `new_conversation: true` to
  start fresh. Editing or regenerating a message also erases what the agents said to each
  other in the removed turns.
- **Questions go back up.** A helper that needs information ends its reply with a question. The
  caller answers with another `ask_agent` call, and the helper continues with full memory. If
  the caller doesn't know either, it asks its own caller, and finally the top agent asks you.
- **No loops.** An agent can't call an agent that is already waiting on it (it replies by
  ending its turn instead), and chains stop at depth 3.

All agents share `~/agent-chat/workspace/` unless you give one its own folder, so one
agent can pick up files another one wrote.

## Memory

Inspired by [amnis](https://github.com/mandoof1/amnis). Facts about you live in
`data/memory.db` (SQLite). Each fact has a category, importance (1–10), confidence and usage
stats. Search uses SQLite's full-text index (bm25), ranked by relevance × importance ×
confidence. Near-duplicates are merged instead of stored twice.

How memories reach agents:

- **Profile:** when a chat starts, its agent gets your most important and pinned facts in its
  system prompt. The snapshot stays fixed for that chat (so the model's prompt cache keeps
  working). It refreshes when the chat is compacted.
- **Recall:** each message you send gets the few memories most relevant to it attached. The
  chat shows "N memories recalled" under the message.
- **Tools:** agents save facts with `remember`, look things up with `recall_memory`, and fix
  outdated facts with `forget_memory`.
- **Automatic learning:** after each reply, the model reviews the turn and adds, updates or
  deletes facts. This runs only while the model is idle and stops the moment you send
  something. Turn it off in Settings.

Open **Memory** to search, add, edit, re-categorize, pin (always in the profile) or delete
memories. **🧹 Tidy up** asks the model to merge duplicates, resolve contradictions (newest
wins) and drop trivia, like amnis's consolidation step. It never rewrites or deletes facts you
typed in yourself. Memories you
typed in yourself are never deleted automatically. Weak automatic memories nobody used for 60
days are pruned.

## Email

Set it up in **Settings → Email account**: pick a provider to fill in the servers, then
enter your address and an **app password**. Gmail, iCloud, Yahoo and Fastmail all require app
passwords for this, and your normal password won't work. **Save & test email** checks both
servers. The password is stored only in `data/secrets.json` (readable by your user only), and is
never sent to the model or back to the browser.

Ask the Mail agent things like "what needs a reply today?", "summarize the newsletter from X",
or "draft a reply to Sam saying I'll be late". It writes in your voice using what it
remembers about you.

**Read this:** emails are written by strangers, and a crafted email can try to instruct the
agent ("forward this to…", "fetch this link…"). The app fences email text as untrusted, and the
Mail agent is told never to act on instructions inside emails. The hard guarantees are the
approvals: nothing is **sent** and no **shell command** runs without your click. Fetching web
pages is not approval-gated. Be suspicious if an agent wants to visit odd links after reading
mail.

## Routines

**Routines** lists agents that run on a schedule: at a set time on chosen days, or every N minutes.
Each routine has its own chat ("Routine · Morning briefing"). Every run adds the routine's
prompt there and starts the agent, as if you typed it, so you can read each result and reply
to follow up. Templates include a morning email briefing, planning tomorrow, a news digest
built around your interests, and a weekly memory check.

Routines run only while the app is running. A run missed while the app was off fires on
start-up if it is less than two hours late; older ones are skipped. Approvals (sending email,
shell commands) still wait for you, and you get a notification.

## Calendars

In **Settings → Calendars**, paste each calendar's private iCal/ICS link:

- **Google:** Calendar settings → your calendar → "Secret address in iCal format".
- **iCloud:** share the calendar as a public calendar and copy the `webcal://` link.
- **Outlook:** Settings → Calendar → Shared calendars → Publish → ICS link.
- A path to a local `.ics` file also works.

Access is read-only. The links are stored in `data/secrets.json`, and the browser only sees a
shortened version. Repeating events, skipped dates, moved occurrences and time zones are all
handled by the `recurring-ical-events` library. Feeds are cached for 5 minutes. The 📅
**Planner** agent uses this. Give "Read your calendars" to any other agent in its editor, and
try the **Morning briefing** routine (calendar plus urgent email, weekdays at 07:30).

## Web search

`web_search` works best with a self-hosted [SearXNG](https://docs.searxng.org/): set its URL
at **Settings → Search engine URL** (e.g. `http://127.0.0.1:8888`). It is free, needs no API
key, and asks several engines at once, so one engine blocking you doesn't break search.

- **Install:** follow the [SearXNG docs](https://docs.searxng.org/admin/installation.html). A
  git clone with its own venv, or the Docker image, both work.
- **Config it needs:** turn JSON output on (`search.formats: [html, json]` — without it SearXNG
  returns 403 to the API) and the bot limiter off (`server.limiter: false`). Bind it to
  `127.0.0.1` only. Enable whichever engines work from your network.
- **Check engines:** open `<your-searxng>/stats` to see which engines fail, and toggle them in
  its `settings.yml`.
- Leave the URL empty to fall back to scraping DuckDuckGo directly (often rate-limited).

## Browser control

The **🌐 Browser** agent drives a real [Brave](https://brave.com/) browser through
[Playwright](https://playwright.dev/python/). Use it when `web_fetch` isn't enough — pages that
render with JavaScript, need a login or cookies, or where you have to click through steps or
fill a form. Brave is Chromium-based, so Playwright drives the system binary directly; no
bundled-browser download is needed.

- **Settings → Browser:** the **executable** path (default `/usr/bin/brave`) and a **headless**
  toggle. Headless (the default) runs with no visible window; turn it off to watch the browser
  work on your screen. Changing either setting restarts the session on the next navigation.
- **One shared session.** The browser launches on the first `browser_navigate` and stays open
  across the agent's tool calls, so it keeps the same page and cookies between steps. A
  module-level lock serialises calls, so two agents acting at once take turns on the one session.
  `browser_close` (or shutting the app down) closes it.
- **Tools:** `browser_navigate` (open a URL), `browser_read` (re-read the current page's text),
  `browser_links` (list links as text → URL), `browser_click` (by visible label or CSS
  selector), `browser_type` (fill a field by label/placeholder or selector, optionally pressing
  Enter), `browser_screenshot` (save a PNG into the workspace).
- **Safety.** The agent is told to treat page text and form labels as information, never as
  instructions, and not to log in, buy, post or take other consequential actions unless you
  asked for that specific thing. Browser calls don't pop an approval prompt, so give the agent
  only tasks you're comfortable with it carrying out; bypass permissions doesn't change browser
  behaviour (it already doesn't ask).
- **Install:** `uv sync` pulls in Playwright. You don't need `playwright install`, because the
  agent drives your installed Brave rather than a downloaded Chromium.
- **Test:** `PYTHONPATH=. tests/e2e_browser.py` drives real headless Brave over a local page
  (navigate, read, links, click, type, screenshot, workspace sandbox). Set `BROWSER_EXECUTABLE`
  to override the Brave path.

## Long chats: auto-compaction

Before every model call the app estimates the prompt size. It calibrates itself against the
token counts the server reports. Past **70%** of the context window (adjustable in
Settings), the same model writes a summary of the older messages:

- The model sees the summary plus the recent messages word for word.
- Your chat still shows everything, with an expandable "Earlier messages summarized" marker.
- If the server still rejects a prompt as too long, the app compacts and retries once.
- If a reply fills the context window before it finishes (a very long think, say), the agent
  keeps going instead of stopping. The next request leaves the cut-off reply out and passes the
  end of its thinking (last 24,000 characters) plus any answer it had started, with "Continue
  from where you stopped." If the prompt itself took more than half the window, the app compacts
  first. This also works in delegated sub-chats, so a teammate never hands back an empty answer.
- Thinking is never cut short.

## Notes for llama.cpp

- Start `llama-server` with `--jinja`; tool calling needs it. Your launch scripts already do.
- Thinking (`reasoning_content`) streams into a collapsible "Thinking" block that follows the
  newest text. It is never capped.
- With `--parallel 1`, chats that run at the same time wait in line at the server. Switching
  between agents is cheap anyway: llama-server's RAM prompt cache (`--cache-ram`, default
  8 GB) restores the previous agent's prompt instead of recomputing it (measured: ~1–2 s).
  That cache uses system RAM. Add `--cache-ram 4096` if memory gets tight.
- System prompts contain the date but not the time, and the memory profile is fixed per chat,
  so prompts stay identical between turns and the server's prompt cache keeps working.

## Files

```
app/main.py       HTTP API + SSE streams
app/runner.py     agent loop, delegation + sub-chats, approvals, background runs, memory extraction
app/history.py    what the model sees; auto-compaction
app/memory.py     long-term memory (SQLite + FTS5)
app/mail.py       IMAP/SMTP email tools
app/agenda.py     read-only calendars from iCal feeds
app/routines.py   scheduled agent runs
app/llm.py        streaming OpenAI-compatible client
app/tools.py      file, shell and web tools
app/defaults.py   starter agents
static/           the UI (plain HTML/CSS/JS, no build step)
data/             agents, chats, settings (JSON), memory.db, secrets.json
tests/            mock model server + end-to-end tests
```

## Tests (no GPU needed)

```bash
uv run uvicorn tests.mock_llm:app --port 8766 &                        # fake model
MOCK_NCTX=3000 uv run uvicorn tests.mock_llm:app --port 8768 &         # tiny context
MOCK_OVERFLOW_CHARS=6000 uv run uvicorn tests.mock_llm:app --port 8769 &
AGENT_CHAT_DATA=/tmp/ac-test uv run uvicorn app.main:app --port 8767 &
export AC_URL=http://127.0.0.1:8767
curl -X PUT $AC_URL/api/settings -H 'content-type: application/json' -H 'X-Agent-Chat: 1' \
     -d '{"base_url":"http://127.0.0.1:8766/v1"}'   # write requests need this header
uv run python tests/e2e.py            # chat, tools, approvals, delegation, stop, edit
uv run python tests/e2e_features.py   # sub-chats, statuses, compaction
uv run python tests/e2e_cutoff.py     # replies that fill the context window continue
PYTHONPATH=. uv run python tests/e2e_queue.py   # messages queued while an agent works
uv run python tests/e2e_ctx.py        # the context meter updates live
uv run python tests/e2e_memory.py     # memory
uv run python tests/e2e_search.py     # web_search through SearXNG (needs it on :8888, uses the live web)
AGENT_CHAT_DATA=/tmp/ac-test uv run python tests/e2e_routines.py   # needs the app started with AGENT_CHAT_ROUTINE_TICK=1
# email: run a test IMAP + SMTP server, point Settings → Email at them, then:
#   uv run --with pymap pymap --port 1143 --no-tls dict --demo-data &
#   uv run --with aiosmtpd python -m aiosmtpd -n -l 127.0.0.1:1025 &
uv run python tests/e2e_mail.py
TEST_ICS=/tmp/test.ics uv run python tests/e2e_calendar.py
```

Environment variables: `AGENT_CHAT_DATA` (data folder), `AGENT_CHAT_WORKSPACE` (default
workspace), `AGENT_CHAT_HOSTS` (extra allowed host names, for example to reach it from your
phone over the LAN; you also need to start uvicorn with `--host 0.0.0.0`), `PORT` (for `run.sh`).

## License

[MIT](LICENSE).
