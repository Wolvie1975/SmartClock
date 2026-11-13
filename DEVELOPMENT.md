# Fedora AI Smart Clock — Development Guide

A full-screen desktop clock and dashboard for a Fedora 45 laptop, written in **Rust + Slint**. It has timers, alarms, reminders, weather, a day timeline, a briefing, Google Calendar/Tasks/Gmail integration, and a multi-provider AI assistant with push-to-talk voice.

| Reference | Link |
|---|---|
| Design canvas (all screens, dark + light, clickable) | https://claude.ai/artifact/LBAE37B37bFNrFxicYzqxG |
| Design handoff doc (decisions, tokens, behavior) | https://claude.ai/artifact/1D8z1npXjRiowFfg6vKsbt |

> **Setting up a machine?** See [INSTALL.md](INSTALL.md).
>
> **How to use this file:** work through the phases in section 14 in order. Each phase ends with something you can run. Section numbers are referenced from the checklists.

---

## 1. Scope

**Target:** Fedora 45 Workstation, GNOME on Wayland, one display at **1920×1080, 100% scale**, running full screen.

**Screens (from the design):**

| Screen | Purpose |
|---|---|
| Idle (screensaver) | Huge clock, one info strip, day timeline. No controls. Shown after the idle timeout. |
| Dashboard | Clock, weather panel (now, next 8 hours, today and tomorrow), day timeline, Timers · Alarms · Reminders · Briefing columns, command bar. Shown on mouse movement. |
| Assistant | Conversation list, transcript with route labels, LOCAL / CLOUD switch, privacy side panel. |
| Settings | 01 AI & Models · 02 Accounts · 03 Appearance · 04 Idle screen · 05 Voice & Mic · 06 Alarm sounds · 07 Startup |
| Timer takeover | Last 10 seconds of a timer, then TIME'S UP. Overlays everything. |
| Alarm ringing | Full-screen alarm with SNOOZE / DISMISS, snoozed and dismissed states. Overlays everything. |

Top-level tabs: **DASHBOARD · ASSISTANT · SETTINGS** (there is no Alarms tab; alarms are managed on the dashboard).

---

## 2. Decisions already made

| Area | Decision |
|---|---|
| UI toolkit | Slint (not Tauri/React) |
| Visual direction | Concept 3 "Instrument": monospace numerals, 1 px hairline grid, 4 px radius |
| Themes | Dark and light, toggle in header and Settings; light uses tinted mid-tones, no white |
| Clock format | 12-hour everywhere (AM/PM), including timeline, weather and pickers |
| Screen model | Idle after timeout (default **5 min**) → dashboard on mouse movement |
| Second time zone | None |
| Timers / alarms / reminders | Added and removed directly on the dashboard |
| Timer finish | Last 10 s take over the whole screen |
| Briefing | **Manual only** (button or "brief me"); no weather (the weather panel covers it); today's calendar incl. birthdays; Gmail new-unread **count only** |
| Reminders ↔ Google | Untimed reminders sync with **Google Tasks**; timed reminders stay local (Tasks API has no time of day) |
| Google sign-in | GNOME Online Accounts (calendar) first; in-app OAuth for Tasks / Gmail / calendar edit |
| AI settings | Inside Settings → AI & Models; no separate tab |
| Weather | Open-Meteo (no API key) |
| Storage | Embedded SQLite (`rusqlite`, bundled). No database server, so alarms never depend on Docker or a service being up |
| Credentials | GNOME Keyring via Secret Service (`oo7`), never in SQLite |
| Packaging | RPM first, Flatpak later |

---

## 3. Fedora prerequisites

### 3.1 Toolchain

```bash
# Rust via rustup (recommended over the distro package for faster updates)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup component add rust-src rustfmt clippy
```

### 3.2 System packages

```bash
sudo dnf install -y \
  gcc gcc-c++ clang clang-devel cmake pkgconf-pkg-config git \
  fontconfig-devel freetype-devel \
  libxkbcommon-devel libxkbcommon-x11-devel libxkbcommon-x11 wayland-devel libxcb-devel \
  alsa-lib-devel
```

| Package(s) | Needed by |
|---|---|
| `fontconfig-devel`, `freetype-devel`, `libxkbcommon-*`, `wayland-devel`, `libxcb-devel` | Slint's windowing and text rendering |
| `alsa-lib-devel` | `cpal` audio (reaches PipeWire through the ALSA plugin, installed by default on Fedora) |
| `clang`, `clang-devel`, `cmake` | `whisper-rs` builds whisper.cpp and runs bindgen |
| `sqlite` (optional) | Only for the `sqlite3` CLI to inspect `clock.db`; the app bundles SQLite |

> **Note:** `libxkbcommon-x11` is needed at runtime even under Wayland when the X11 fallback is probed; without it Slint panics with *"libxkbcommon-x11.so could not be loaded"*.

### 3.3 Runtime tools (later phases)

**Ollama** (local LLM) runs in Docker, bound to `127.0.0.1:11434`, with `llama3.2:3b` as the default model (this laptop has no GPU Ollama can use, so a small model keeps replies quick). The commands are in [INSTALL.md](INSTALL.md) section 5.

```bash
# Piper TTS, isolated in its own environment (Phase 4)
pipx install piper-tts        # or: python -m venv ~/.local/piper && ~/.local/piper/bin/pip install piper-tts
```

> **Piper status:** active development moved from `rhasspy/piper` (archived, MIT) to `OHF-Voice/piper1-gpl` (GPL-3.0). Run it as a **separate process** called from the app instead of linking it in. That keeps the build simple, and it's worth a look if the GPL matters for how you distribute.

---

## 4. VS Code setup

### 4.1 Extensions

| Extension | ID | Why |
|---|---|---|
| rust-analyzer | `rust-lang.rust-analyzer` | Rust language server |
| Slint | `Slint.slint` | `.slint` highlighting, completion and **live preview** |
| CodeLLDB | `vadimcn.vscode-lldb` | Debugging |
| Even Better TOML | `tamasfe.even-better-toml` | `Cargo.toml` editing |
| Dependi | `fill-labs.dependi` | Shows outdated crate versions |

```bash
code --install-extension rust-lang.rust-analyzer
code --install-extension Slint.slint
code --install-extension vadimcn.vscode-lldb
code --install-extension tamasfe.even-better-toml
code --install-extension fill-labs.dependi
```

### 4.2–4.4 Config files

`.vscode/settings.json`, `launch.json` and `tasks.json` are committed in the repo. The debug launch config uses `RUST_LOG=smart_clock=debug,clock_core=debug`.

> **Tip:** run with `--windowed` while developing. Full screen makes debugging painful; windowed at 1920×1080 matches the design closely.

---

## 5. Project layout

A Cargo workspace (at the repo root) keeps the clock logic testable without the UI, and lets the scheduler become a separate background service later (Phase 6).

```text
SmartClock/
├── Cargo.toml                  # workspace
├── crates/
│   ├── clock-core/             # domain logic, no UI: timers, alarms, reminders,
│   │   └── src/                #   scheduler, command parser, storage (SQLite)
│   ├── ai/                     # provider trait + adapters (Phase 3)
│   ├── voice/                  # cpal capture, whisper-rs, Piper bridge (Phase 4)
│   ├── google/                 # GOA, OAuth, Calendar, Tasks, Gmail (Phase 5)
│   └── app/                    # the Slint application (binary: smart-clock)
│       ├── build.rs
│       ├── src/main.rs
│       ├── src/inhibit.rs      # GNOME idle inhibit
│       └── ui/
│           ├── app.slint       # AppWindow, header tabs, screen switching
│           ├── theme.slint     # global Theme (tokens)
│           ├── idle.slint
│           ├── dashboard.slint
│           ├── assistant.slint
│           ├── settings.slint
│           ├── takeover.slint  # timer + alarm overlays
│           ├── widgets/        # Label, DayTimeline, Segmented, Toggle…
│           └── fonts/          # JetBrains Mono, Inter Tight (.ttf + OFL)
├── data/
│   ├── smart-clock.desktop
│   └── icons/
└── .vscode/
```

> **Why `clock-core`, not `core`:** a crate named `core` shadows Rust's built-in `core` library, and code generated by other crates' macros (serde, tracing) that refers to `::core` can break.

### 5.1 Dependencies

Use `cargo add` so you get current versions, and add each crate's dependencies in its own phase.

```bash
# clock-core: domain + storage
cargo add -p clock-core chrono serde --features serde/derive
cargo add -p clock-core rusqlite --features bundled
cargo add -p clock-core thiserror tracing

# app: UI + glue
cargo add -p smart-clock slint chrono tracing anyhow zbus
cargo add -p smart-clock tracing-subscriber --features env-filter
cargo add -p smart-clock --build slint-build
cargo add -p smart-clock clock-core --path crates/clock-core
cargo add -p smart-clock tokio --features rt-multi-thread,macros,time,sync
cargo add -p smart-clock reqwest --no-default-features --features json,rustls-tls   # Open-Meteo (Phase 2)

# ai (Phase 3)
cargo add -p ai reqwest --no-default-features --features json,rustls-tls,stream
cargo add -p ai serde serde_json async-trait futures tokio thiserror --features serde/derive

# voice (Phase 4) — pin whisper-rs to an exact version (=x.y.z); its API changes between releases
cargo add -p voice cpal anyhow tracing
cargo add -p voice whisper-rs@=<version>

# google (Phase 5)
cargo add -p google oauth2 reqwest zbus oo7 serde serde_json chrono anyhow \
  --features serde/derive
```

> **zbus 6** has no `blocking` module; it's async-only. `inhibit.rs` runs a small current-thread tokio runtime on its own thread to hold the D-Bus connection.

---

## 6. Slint foundation

The Phase 1 implementation is in the repo; this section records the important choices.

### 6.1 Theme tokens (`ui/theme.slint`)

These values match the handoff doc exactly. Text colors meet 4.5:1 contrast on their backgrounds.

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg` | `#0E0E0C` | `#C8C6B7` | Window background |
| `panel` | `#151512` | `#D2D0C2` | Inputs, command bar, filled timeline |
| `line` | `#2E2E29` | `#A8A696` | Hairlines |
| `text` | `#EDEBE3` | `#1A1A17` | Body text |
| `muted` | `#9C9A8F` | `#47463F` | Labels |
| `accent-fill` | `#C6F36B` | `#A6CF4A` | Buttons, progress, markers |
| `accent-text` | `#C6F36B` | `#3A520C` | Accent-colored text/icons |
| `on-accent` | `#0E0E0C` | `#1A1A17` | Text on accent fills |

Type: JetBrains Mono for numerals and labels, Inter Tight for body text. Clock sizes are 400 px (idle), 260 px (dashboard) and 560 px (timer countdown), all weight 300. Labels are 13 px uppercase mono with 1.5 px letter spacing (≈0.12em; Slint's `letter-spacing` takes a length). Write label text in uppercase in the source. Shape: 4 px radius, 1 px hairlines, 48 px page padding, 44 px minimum touch target.

> **Fonts:** JetBrains Mono ships as static weights (Light/Regular/Medium/Bold). Inter Tight is the Google Fonts **variable** font (`InterTight.ttf`), covering all weights in one file.

### 6.2 Window and screen switching (`ui/app.slint`)

- `AppWindow` holds `screen` (idle / dashboard / assistant / settings) and `overlay` (none / timer / alarm).
- Rust pushes clock strings, the day progress, the info strip and the burn-in drift every tick.
- One full-window `TouchArea` raises `activity()` on `changed mouse-x`, `changed mouse-y` and `pointer-event`, so hover alone wakes the idle screen.

### 6.3 Main loop (`src/main.rs`)

- 4 Hz Slint timer: clock text, day progress, burn-in drift step, idle check (timer and alarm checks join in Phase 2).
- Idle timeout comes from the `idle_minutes` setting (default 5). `--idle-minutes N` overrides it for one run and accepts fractions (`0.25` = 15 s).
- The theme toggle flips `Theme.dark` and saves `theme` to the settings table. `start_on = idle` opens on the idle screen.
- Full screen by default; `--windowed` for development.
- Default log filter is `info,arboard=error` (arboard warns that GNOME lacks wlr-data-control; harmless).

---

## 7. Screen and overlay states

| State | Enters when | Leaves when |
|---|---|---|
| Idle | No mouse movement for the idle timeout (default 5 min) | Any mouse movement → Dashboard |
| Dashboard | Mouse moves, or the app starts (Startup → "Open on") | Idle timeout → Idle |
| Timer takeover (overlay) | A running, unpaused timer reaches ≤ 10 s; the closest to finishing wins | STOP, DISMISS or +1 MIN → previous state |
| Alarm ringing (overlay) | An alarm that is on reaches its time; breaks through Idle | SNOOZE → Snoozed; DISMISS → Dismissed; auto-stop after the set time |

**Timer takeover:** a giant countdown, the timer name, and ten blocks that empty once per second, with +1 MIN and STOP. At zero it shows TIME'S UP, pulsing between accent and background once per second (never faster), with +1 MIN, RESTART and DISMISS. It is announced to screen readers and plays the timer sound.

**Alarm ringing:** current time in the accent color, the alarm label, a volume bar that ramps from 20% to 100% over 30 s, and the elapsed ringing time. Buttons are SNOOZE · 9 MIN (the snooze length comes from Settings) and DISMISS. The background alternates bg/panel once per second. Snoozed shows a countdown and "rings again at…". Dismissed shows "Good morning", PLAY BRIEFING (manual) and DASHBOARD →.

**Missed alarms:** an alarm found more than 5 minutes late (app closed or laptop asleep) is skipped and logged rather than rung. Catch-up after sleep is Phase 6.

**Screen awake:** while the app runs, `inhibit.rs` calls `org.gnome.SessionManager.Inhibit("smart-clock", 0, "Clock display", 8)` (8 = inhibit idle) and keeps the cookie. Dropping the guard calls `Uninhibit`; if the process dies, GNOME drops the inhibit when the D-Bus connection closes.

**Burn-in:** on the idle screen, shift the whole layout by a few pixels each minute using the offset cycle (0,0) → (3,2) → (−2,3) → (−3,−1) → (2,−3) → (1,1). The brightness setting runs from 30% to 100%.

**Wayland notes:** GNOME won't let apps place their own windows or force always-on-top. Running full screen avoids both problems. Watch for activity only inside the app's own window. No global idle API is needed.

---

## 8. Data model (SQLite)

Database path: `~/.local/share/smart-clock/clock.db` (respects `$XDG_DATA_HOME`). Store all times as UTC and format them as 12-hour local time only in the UI.

Migrations live in `clock-core/src/store.rs` as an append-only list, tracked with `PRAGMA user_version`. Never edit an applied migration; add a new one.

```sql
CREATE TABLE alarms (
  id          INTEGER PRIMARY KEY,
  time_local  TEXT    NOT NULL,          -- "07:00" (local wall-clock time)
  days        INTEGER NOT NULL DEFAULT 127, -- bitmask Mon=1 … Sun=64; 0 = one-off
  label       TEXT    NOT NULL DEFAULT '',
  enabled     INTEGER NOT NULL DEFAULT 1,
  sound       TEXT    NOT NULL DEFAULT 'chime',
  next_fire_utc TEXT                     -- recomputed after each fire/edit/resume
);

CREATE TABLE reminders (
  id           INTEGER PRIMARY KEY,
  text         TEXT    NOT NULL,
  due_date     TEXT,                     -- "2026-11-13" or NULL
  due_time     TEXT,                     -- "17:00" or NULL; set ⇒ LOCAL only
  done         INTEGER NOT NULL DEFAULT 0,
  source       TEXT    NOT NULL DEFAULT 'local',  -- 'local' | 'google'
  google_task_id   TEXT,
  google_list_id   TEXT,
  google_etag      TEXT,
  updated_utc  TEXT    NOT NULL
);

CREATE TABLE timers (                    -- persisted so a crash/restart keeps them
  id          INTEGER PRIMARY KEY,
  name        TEXT    NOT NULL,
  total_ms    INTEGER NOT NULL,
  ends_utc    TEXT,                      -- NULL while paused
  paused_left_ms INTEGER
);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- theme, clock_format, idle_minutes, burn_in, keep_awake, mic_device, whisper_model,
-- language, speak_replies, tts_voice, tts_speed, alarm_sound, timer_sound, volume,
-- ramp, snooze_min, autostop_min, open_at_login, start_on, fullscreen, display,
-- run_in_background, catch_up, google_method, tasks_sync, briefing_last_utc …

CREATE TABLE conversations (id INTEGER PRIMARY KEY, title TEXT, created_utc TEXT);
CREATE TABLE messages (
  id INTEGER PRIMARY KEY, conversation_id INTEGER REFERENCES conversations(id),
  role TEXT, content TEXT, route TEXT, cloud INTEGER DEFAULT 0, created_utc TEXT
);
```

> **Secrets never go here.** API keys and Google refresh tokens go in GNOME Keyring with `oo7` (section 12.3).

---

## 9. Clock command parser (offline, before any LLM)

The dashboard command bar and the assistant both run input through this parser first. A match never touches a model, and the UI shows it as **BUILT-IN · NO AI USED** with an UNDO button.

| Input | Result |
|---|---|
| `timer 20m`, `timer 90s`, `20 min timer`, `timer 1h 15m` | Start a timer |
| `tea timer 4m` | Timer named "Tea" |
| `alarm 7am`, `alarm 6:45 weekdays`, `wake me at 7` | Add an alarm |
| `remind me at 5 to check the mail` | Timed reminder (local) |
| `remind me to renew car registration` | Untimed reminder (syncs to Google Tasks) |
| `remind me tomorrow to call the bank` | Dated reminder, no time (syncs) |
| `cancel tea timer`, `stop all timers` | Remove timers |
| `brief me` | Play the briefing |
| `location Austin, TX` | Set the weather location (looked up once via Open-Meteo geocoding; handled by the dashboard, not the offline parser, because it needs the network) |

Also understood: `set a 5 minute timer`, `remind me in 20 minutes to …`, `remind me to … tomorrow at 9`, `alarm 7am mon wed fri`, `alarm 5:30am gym weekdays` (label "Gym"), `noon` / `midnight`, `cancel timer` (latest).

```rust
// crates/clock-core/src/command.rs
pub enum Command {
    StartTimer { name: Option<String>, duration: std::time::Duration },
    AddAlarm   { time: NaiveTime, days: u8, label: Option<String> },   // days: Mon=1 … Sun=64, 0 = one-off
    AddReminder{ text: String, date: Option<NaiveDate>, time: Option<NaiveTime> },
    CancelTimer{ target: TimerTarget },                                // Latest | Named(String) | All
    Brief,
}

/// `now` is local wall-clock time (pass `Local::now().naive_local()`), which keeps tests
/// independent of the machine's time zone. None → send the text to the AI router.
pub fn parse(input: &str, now: NaiveDateTime) -> Option<Command>;
```

**Time rules** (all covered by tests):
- An hour with no AM/PM means the next time that o'clock comes round: "at 5" is 5 AM before 5 AM, otherwise 5 PM, otherwise tomorrow 5 AM. "12am" is midnight.
- On an explicit future date ("tomorrow at 3"), 7–11 → AM; 12 and 1–6 → PM.
- A repeating alarm ("alarm 6:45 weekdays") reads 4–11 as AM; 12 and 1–3 as PM.
- `07:00`, `0:30` and 13–23 are 24-hour.
- "on friday" said on a Friday means next week.
- Input starting with a question word ("what is a 5 minute timer") is never a timer.

> **Test first.** This parser is pure and easy to test. Write the table above as `#[test]` cases before you implement it, including the edge cases: "at 5" means the next 5 o'clock (5 PM if it's already past 5 AM), and "12am" means midnight.

---

## 10. AI layer

### 10.1 Provider trait

```rust
// crates/ai/src/lib.rs
use futures::stream::BoxStream;

pub struct Capabilities { pub tools: bool, pub streaming: bool, pub vision: bool, pub local: bool }

pub enum Chunk { Text(String), ToolCall { name: String, args: serde_json::Value }, Done }

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;                                // "ollama", "openrouter", "anthropic", "gemini", "openai"
    fn capabilities(&self, model: &str) -> Capabilities;
    async fn chat(&self, model: &str, messages: &[Message], tools: &[ToolSpec])
        -> anyhow::Result<BoxStream<'static, anyhow::Result<Chunk>>>;
    async fn test(&self) -> anyhow::Result<()>;          // Settings → TEST
}
```

| Adapter | Covers | Notes |
|---|---|---|
| `OpenAiCompatible` | OpenAI, Ollama (`/v1`), OpenRouter | One adapter, different base URLs and keys |
| `Anthropic` | Claude models | Native Messages API |
| `Gemini` | Gemini models | Native API, API key only (not the Google account sign-in) |

### 10.2 Tools the model can call

`set_timer`, `cancel_timer`, `add_alarm`, `add_reminder`, `list_today` (time, timers, alarms, reminders, weather; calendar in Phase 5), `get_weather`. Each call runs through the same code path as the parser (`actions::apply`) and shows with UNDO.

Lessons from testing with `llama3.2:3b` (see the ignored live tests in `ai` and `smart-clock`):
- **Arguments arrive loosely** (`"minutes": "5"`), so tools accept numbers as strings, and times and dates as text resolved by the parser's rules.
- **When every call in a round is a successful action, the app writes the confirmation** ("Alarm set for 6:30 AM on weekdays.") and skips the model's second round. That halves the wait and stops invented details.
- **Lookups give exact facts** ("rings at 9:01 AM") so the model never has to do time arithmetic, and the system prompt tells it to state only facts from the tools.
- **A tool call written as JSON text** is run if it names a real tool; otherwise the question is retried with no tools offered.

### 10.3 Routing and privacy

The assistant is there for the clock's job: time, alarms, timers, reminders, today's schedule, weather and the briefing. It is not a general coding or research assistant, so there is no routing by task type.

| Input | Route |
|---|---|
| Clock commands | Built-in parser · no AI |
| Everything else, including the daily briefing | Ollama · local (`llama3.2:3b`) |
| A conversation you switch to CLOUD | Anthropic · cloud (Gemini later) |

The per-conversation switch is **LOCAL / CLOUD**, and every conversation starts on LOCAL. Nothing leaves the laptop unless you flip a conversation to CLOUD yourself. The app never decides to send something to the cloud.

- Every cloud message is labelled in the transcript: `CLOUD · SENT OFF DEVICE`.
- If Ollama is unreachable, the assistant says so and offers to send that one message to the cloud. It never switches by itself.
- The briefing always runs locally, whatever the switch says.

**Gmail data never goes to a cloud model.** The briefing only uses the unread count anyway.

---

## 11. Voice

| Step | Library | Notes |
|---|---|---|
| Capture | `cpal` | Default input, or the device chosen in Settings; 16 kHz mono f32 for Whisper |
| Push to talk | UI | Hold the mic button, or hold SPACE while the window is focused |
| Speech → text | `whisper-rs` (pinned) | Model chosen in Settings: tiny / base / small / medium (default base). Language auto or English |
| Text → speech | Piper (separate process) | `piper --model <voice>.onnx --output-raw` → play the PCM with `cpal`. Speed 0.9× / 1.0× / 1.2× |
| Wake word | later | openWakeWord or `rustpotter` + VAD; never Whisper running all the time |

Store Whisper models and Piper voices in `~/.local/share/smart-clock/models/`.

---

## 12. Google integration

### 12.1 What uses what

| Feature | Data | Permission (scope) | Works with GNOME Online Accounts? |
|---|---|---|---|
| Timeline + briefing events, birthdays | Calendar (read) | `calendar.readonly` | Yes |
| Assistant adds/moves events | Calendar (edit) | `calendar.events` | No, needs in-app sign-in |
| Reminders sync | Tasks | `tasks` | No, needs in-app sign-in |
| Briefing new-unread count | Gmail | `gmail.metadata` (or `gmail.labels` if only the unread total is needed) | No, needs in-app sign-in |

> **Verify current rules.** Google changes scope rules and verification requirements. Gmail scopes are classed as restricted. For a personal app in testing mode, Google shows an "unverified app" screen, and refresh tokens expire after about 7 days, so the Accounts screen shows a "reconnect by" date.

### 12.2 GNOME Online Accounts (calendar, no Google Cloud setup)

Two ways to read the calendar:
1. **GOA access token → Calendar API directly.** List accounts on `org.gnome.OnlineAccounts` and call `OAuth2Based.GetAccessToken` on the Google account. Use the token with `reqwest` against `https://www.googleapis.com/calendar/v3/...`. This is the simplest path if GOA's token includes calendar access.
2. **Evolution Data Server** over D-Bus. GNOME already syncs the calendar locally. This is more D-Bus plumbing but works offline.

Start with option 1 and fall back to option 2 if needed.

### 12.3 In-app OAuth (Tasks, Gmail, calendar edit)

1. Create a Google Cloud project, enable the Calendar, Tasks and Gmail APIs, and create an **OAuth client of type "Desktop app"**.
2. Use the `oauth2` crate's authorization-code flow with **PKCE**, redirecting to `http://127.0.0.1:<random port>`.
3. Open the consent URL in the browser (`xdg-open`), then catch the redirect on a one-shot local listener.
4. Save the refresh token in GNOME Keyring:

```rust
let keyring = oo7::Keyring::new().await?;
keyring.create_item(
    "Smart Clock · Google",
    &[("app", "smart-clock"), ("service", "google")],
    refresh_token.as_bytes(),
    true, // replace an existing item
).await?;
```

5. Request only the scopes ticked in Settings → Accounts. Changing them starts a new consent.

### 12.4 Reminders ↔ Google Tasks rules

- The Tasks API stores **only a due date**. Any time is discarded and can't be read back.
- A reminder **without a time** has `source = google` and syncs both ways (create, complete, delete, edit text or date).
- A reminder **with a time** stays local, and the clock's own scheduler fires it. The UI tags it LOCAL and shows a note when a time is entered.
- Sync on startup, every 5 minutes, and right after a local change. Resolve conflicts by `updated` timestamp (last writer wins) and keep `etag` to detect changes.
- Use one Tasks list, either the default "My Tasks" or a chosen list (a later setting).

### 12.5 Briefing data

- **Calendar today:** all-day items and birthdays first, then timed events. Birthdays come from Google Calendar's birthday events, so `calendar.readonly` covers them. Timed events also become markers on the day timeline.
- **Gmail new unread:** at each briefing, save the mailbox `historyId`. Next time, count messages added with the UNREAD label since then using `users.history.list`. If that history has expired, fall back to the INBOX unread total. Show the count only; never senders, subjects or content.
- **Summary:** generated by the local model from those facts plus open reminders and the next alarm, then spoken by Piper. It isn't shown as text, to save space.

---

## 13. Background, suspend and startup

| Concern | Approach |
|---|---|
| Alarms keep working when the window is closed | Phase 6: move the scheduler from `clock-core` into a small **systemd user service** (`smart-clock-scheduler.service`). The UI talks to it over D-Bus (`zbus` interface). Until then, closing the window stops alarms; show that in Settings → Startup. |
| Suspend / resume | Listen for logind `PrepareForSleep(bool)` on the system bus. On resume, recompute `next_fire_utc` and show missed alarms and reminders ("Catch up after sleep"). |
| App menu | `scripts/install-local.sh` installs a release build, the launcher (`data/smart-clock.desktop.in`) and the icon (`data/icons/smart-clock.svg`) for the current user. The window's app id is `smart-clock` to match the launcher. |
| Open at login | Not wanted for now: the clock is opened manually from the app menu. If it comes back, write `~/.config/autostart/smart-clock.desktop` when a setting is on. |
| Open on | The Startup setting picks Dashboard or Idle. |
| Tray icon | Optional only; GNOME needs the AppIndicator extension. |

The launcher template is `data/smart-clock.desktop.in`; the install script fills in the binary path and adds an **Open in a Window** action.

---

## 14. Build phases

### Phase 1: Skeleton and clock
- [x] Workspace, crates and VS Code config (sections 4–5)
- [x] `theme.slint` tokens and fonts; theme toggle saved to settings
- [x] AppWindow with idle ↔ dashboard switching and a 5-minute idle timeout (section 6)
- [x] Idle screen: date / time zone / week row, 400 px clock, info strip, day timeline, burn-in drift
- [x] `--windowed` flag; full screen by default
- [x] GNOME idle inhibit (section 7)

**Done when:** the clock runs full screen, goes idle after the timeout, and wakes on mouse movement.

### Phase 2: Timers, alarms, reminders (local)
- [x] SQLite schema and repository layer in `clock-core` (section 8); `actions::apply` + UNDO shared by every input path
- [x] Command parser with tests (section 9)
- [x] Dashboard: four columns, command bar, day timeline markers
- [x] Timer takeover overlay and alarm ringing overlay (section 7), with sounds through `cpal` (synthesized chime/beeps for now)
- [x] Snooze, auto-stop and volume ramp (settings keys `snooze_min` 9, `autostop_min` 10, `ramp` true, `volume` 80; the Settings screen comes later)
- [x] Weather panel from Open-Meteo: now, 8-hour strip, today and tomorrow with range bars (°F / mph; set the place with `location <city>` in the command bar)

**Done when:** everything on the dashboard works offline with no AI and no Google.

### Phase 3: Assistant
- [ ] `ai` crate: trait, OpenAI-compatible adapter (Ollama first, via its `/v1` endpoint) ✓, then Anthropic (raw HTTP; there is no official Rust SDK), then Gemini
- [x] Tool calls wired to the `clock-core` actions, with UNDO (command bar → answer card; full Assistant screen next)
- [x] Assistant screen: conversations (saved in SQLite, migration 3), transcript with route labels, LOCAL / CLOUD switch, privacy panel; clock commands answered BUILT-IN with UNDO
- [ ] Settings → AI & Models: local model (Ollama URL + model), cloud provider + model (key in the keyring)

### Phase 4: Voice
- [ ] `cpal` capture and push to talk, with a level meter in Settings → Voice & Mic
- [ ] `whisper-rs` transcription, then the same parser/AI path as typed input
- [ ] Piper replies, voice test, speed
- [ ] Manual briefing playback (PLAY BRIEFING, "brief me")

### Phase 5: Google
- [ ] Settings → Accounts: method choice, status states, permission checklist
- [ ] Calendar through GOA (section 12.2): timeline markers and the briefing list
- [ ] In-app OAuth with PKCE, refresh token in the keyring (section 12.3)
- [ ] Tasks sync for untimed reminders, GOOGLE/LOCAL tags (section 12.4)
- [ ] Gmail new-unread count (section 12.5)

### Phase 6: Background and packaging
- [ ] Scheduler as a systemd user service with a D-Bus interface
- [ ] logind resume handling and catch-up
- [x] App-menu launcher and icon (`scripts/install-local.sh`); open at login is not wanted for now
- [ ] RPM spec (`cargo-generate-rpm` or a hand-written `.spec`); Flatpak later (the sandbox complicates mic, keyring and autostart)

---

## 15. Still open

| Item | Notes |
|---|---|
| Idle ↔ dashboard transition | Cut or fade. Currently a cut; try a 200 ms fade and decide by feel. |
| 12-hour time picker in Slint | Needed for alarms and reminders; the browser picker in the prototype doesn't exist in Slint. |
| Cloud model | Used only for conversations switched to CLOUD. Defaults to Claude Opus 5.5; changeable in Settings → AI & Models. |
| Google connection | GOA first (recommended); in-app OAuth for Tasks and Gmail. |
| Which Tasks list syncs | The default list for now; maybe a picker later. |
