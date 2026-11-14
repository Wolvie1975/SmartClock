# Fedora AI Smart Clock — Design Handoff

Updated Nov 13, 2026 · Jason

| Reference | Link |
|---|---|
| Design canvas (every screen, dark + light, clickable) | https://claude.ai/artifact/LBAE37B37bFNrFxicYzqxG |
| Live handoff doc | https://claude.ai/artifact/1D8z1npXjRiowFfg6vKsbt |
| Clock faces canvas (4 new faces, dashboard sizes, Settings picker) | https://claude.ai/artifact/5SPJ8dP6i7bit8CiKSNjnw |
| Build guide | `DEVELOPMENT.md` (setup, code skeleton, data model, phases) |
| News column guide | `NEWS-UPDATE.md` (replacing the Briefing column with Google News) |

## Overview

The visual direction is **Concept 3, Instrument**: a type-led grid with monospace numerals, in dark and light themes. These screens are designed and work as clickable prototypes:

- **Idle screen**
- **Dashboard**
- **Assistant**
- **Settings**, all seven sections
- **Timer takeover**
- **Ringing alarm**

The app is a Rust + Slint desktop clock for a Fedora 44 laptop. It shows a full-screen idle clock, switches to a dashboard when the mouse moves, and has a multi-provider AI assistant with push-to-talk voice. Concepts 1 (Graphite / Stone) and 2 (Halo) stay on the canvas for reference only.

## Decisions made

| Area | Decision |
|---|---|
| UI toolkit | Slint (used on another project), not Tauri + React |
| Scope | Separate from the TV dashboard project; not related |
| Target display | 1920×1080 at 100% scale |
| Visual direction | Concept 3 · Instrument |
| Themes | Dark and light, with a toggle; light uses tinted mid-tones, not white |
| Screen model | Full-screen idle (screensaver) view; dashboard appears on mouse activity |
| Idle timeout | Default 5 minutes (options: 1, 2, 5, 10) |
| Weather | Expanded panel: now, 8-hour strip, 5-day range bars |
| Second time zone | Removed |
| Clock format | 12-hour everywhere (AM/PM) |
| Clock face | Selectable in Settings → Appearance: **Digital** (default), **Chronograph**, **Split-flap**, **Tape**, **Dot matrix**. One choice applies to the dashboard and the idle screen; alarm and timer overlays always use Digital |
| Clock sizes (Digital) | Idle 480 px, dashboard 320 px, alarm overlay 400 px, timer countdown 560 px |
| Top-level tabs | DASHBOARD · ASSISTANT · SETTINGS (no Alarms tab; alarms are managed on the dashboard) |
| Timers, alarms, reminders | Added and removed directly on the dashboard |
| Timer finish | Last 10 seconds take over the whole screen |
| AI settings | Folded into Settings as "AI & Models"; no separate AI tab |
| Column 04 | **News** replaces the Briefing entirely: the top 3 Google News stories, fetched by the app itself from Google News' public RSS feed |
| Briefing | Removed: no briefing column, no PLAY BRIEFING, no "brief me" command, no spoken summary, no Gmail count |
| Reminders ↔ Google | Untimed reminders sync with Google Tasks; timed reminders stay local |
| Google sign-in | GNOME Online Accounts first (calendar); in-app OAuth for Tasks and calendar edit. Gmail access is no longer needed |

## Screens (Concept 3 · Instrument)

Every screen is 1920×1080 and exists in dark and light.

### Idle screen (screensaver)

- Date, time zone and week number across the top.
- The selected clock face. Digital: very large monospace time (480 px), seconds in the accent color, then AM/PM. Other faces fill the same clock area (see the Clock faces canvas).
- One divided strip: current weather (temp, condition, high/low, rain chance), next alarm or event, and a running timer with a progress bar.
- A thin 24-hour day timeline at the bottom with a "now" line and markers, plus a "MOVE MOUSE FOR DASHBOARD" hint.
- No controls. The layout drifts a few pixels each minute against burn-in. Brightness is adjustable from 30% to 100%.

### Dashboard

- **Header:** app name, tabs, mic status and a LIGHT/DARK toggle.
- **Clock:** the selected clock face (Digital: 320 px time with seconds and AM/PM), plus date, time zone and week.
- **Weather panel:**
  - Current conditions with "feels like".
  - Wind, humidity, rain chance and sunrise/sunset.
  - An 8-hour strip.
  - A 5-day forecast with low-to-high range bars on a shared scale.
- **Day timeline:** full width. Markers come from alarms that are on, open reminders and timed calendar events.
- **Four numbered columns:** 01 Timers, 02 Alarms, 03 Reminders, 04 News.
- **News column:**
  - The top 3 stories from Google News (US top stories), numbered 1–3.
  - Each story: headline (Inter Tight, up to 3 lines, then ellipsis), then publisher and age in muted mono caps (e.g. `AP NEWS · 2H AGO`).
  - Clicking a story opens it in the default browser.
  - Footer line: `UPDATED 2:15 PM · GOOGLE NEWS`. If the last fetch failed, it shows the cached stories with `OFFLINE · SHOWING 2:15 PM`.
  - Hairline dividers between stories; no images.
- **Command bar:** command-line style input at the bottom ("timer 20m", "remind me at 5…") with a hold-to-talk mic button.

### Assistant

- **Left:** numbered conversation list with + NEW.
- **Center:** transcript.
  - Each turn shows who sent it, the time, and the route it took (e.g. GENERAL → OLLAMA · LOCAL).
  - Cloud replies carry a CLOUD · SENT OFF DEVICE tag.
  - Built-in commands show as a result row with UNDO and "no AI used".
- **Header:** AUTO / LOCAL ONLY / CLOUD routing switch for the conversation.
- **Right:** routing table, privacy switches, the tools the AI can use, and a link to Settings → AI & Models.
- **Bottom:** command bar with hold-to-talk. While listening, a level meter shows "Whisper · local · release to send".

### Settings

Each section has its own board on the canvas.

1. **AI & Models:**
   - Providers tagged LOCAL or CLOUD, with status and Test/Set up.
   - A task routing table.
   - Privacy switches: label cloud messages, ask before sending to cloud, fall back to local when offline.
2. **Accounts:**
   - Google connection by GNOME Online Accounts (recommended) or sign-in from the clock (OAuth).
   - Status: not connected, waiting or connected. Shows the account, calendars, and last sync or a reconnect-by date in OAuth test mode.
   - Access checklist: Calendar read (required), Calendar edit, Tasks. The last two need in-app sign-in.
3. **Appearance:**
   - Theme (dark/light) and clock format (12-hour/24-hour).
   - **Clock face:** a row of five cards with live thumbnails (Digital, Chronograph, Split-flap, Tape, Dot matrix). The selected card has an accent border. Applies to the dashboard and the idle screen.
   - Seconds: show/hide (suggested; hiding stops the per-second redraw on the idle screen).
4. **Idle screen:** idle timeout, burn-in protection, keep the screen awake.
5. **Voice & Mic:**
   - Microphone picker with a live level meter.
   - Push-to-talk key (SPACE).
   - Whisper model size (tiny/base/small/medium) and language.
   - Wake word, shown as "later".
   - Spoken replies via Piper, with voice choice, a test button and speed.
6. **Alarm sounds:**
   - Alarm sound list with previews (Chime, Bell, Pulse, Soft rise), plus your own file.
   - Timer sound.
   - Volume, ramp up over 30 s, snooze length, and stop-ringing-after.
7. **Startup:**
   - Open at login (GNOME autostart), open on the dashboard or idle screen, full screen, display.
   - A background service keeps alarms running when the window is closed.
   - Catch up on missed alarms after sleep.

### Timer takeover

- **Last 10 seconds:** a giant countdown, the timer name, and ten blocks that empty one per second. Buttons: +1 MIN and STOP.
- **At zero:** TIME'S UP, pulsing between accent and background once per second (slow enough not to strobe). Buttons: +1 MIN, RESTART, DISMISS.
- The timer closest to finishing goes first. Paused timers never take over.
- It breaks through the idle screen, plays the timer sound and is announced to screen readers.

### Ringing alarm

- **Ringing:** a full-screen takeover over idle or the dashboard.
  - Shows the current time large in the accent color and the alarm's label.
  - A volume bar ramps from 20% to full over 30 s, with elapsed ringing time.
  - Buttons: SNOOZE (9 min, from Settings) and DISMISS.
  - The background shifts gently between bg and panel once per second, never a strobe.
- **Snoozed:** a countdown and "rings again at…", with DISMISS ALARM.
- **Dismissed:** "Good morning" and a link to the dashboard.
- A bottom strip shows the weather, the first event and open reminders.

## Design tokens

Each theme is one set of named colors, mapped to a single Slint `global Theme` with a dark and a light value per token. Text colors meet 4.5:1 contrast on their backgrounds.

| Token | Dark | Light | Used for |
|---|---|---|---|
| bg | #0E0E0C | #C8C6B7 | Page background |
| panel | #151512 | #D2D0C2 | Inputs, command bar, filled timeline |
| line | #2E2E29 | #A8A696 | Hairline dividers, borders |
| text | #EDEBE3 | #1A1A17 | Primary text |
| muted | #9C9A8F | #47463F | Labels, secondary text |
| accent-fill | #C6F36B | #A6CF4A | Buttons, progress, markers |
| accent-text | #C6F36B | #3A520C | Accent-colored text and icons |
| on-accent | #0E0E0C | #1A1A17 | Text on accent fills |

**Type**

- JetBrains Mono for numerals, labels and buttons. Labels are uppercase with wide letter spacing (about 0.12em).
- Inter Tight for body text and list items.
- Weight 300 for large numerals: 480 px idle, 320 px dashboard, 560 px timer countdown, 400 px alarm.

**Shape and layout**

- 4 px corner radius; a 1 px hairline grid instead of cards.
- 48 px page side padding; minimum 44 px touch targets.
- Simple 2 px stroke icons.

## Behavior

The app has four full-screen states. Timer takeover and a ringing alarm sit on top of idle and the dashboard.

| State | Enters when | Leaves when |
|---|---|---|
| Idle | No mouse movement for the idle timeout (default 5 min) | Any mouse movement → Dashboard |
| Dashboard | Mouse moves, or the app starts | Idle timeout passes → Idle |
| Timer takeover | A running timer reaches 10 seconds or less | STOP, DISMISS, or +1 MIN → back to the previous state |
| Alarm ringing | An alarm that is on reaches its time (also breaks through idle) | SNOOZE → Snoozed; DISMISS → Dismissed; stops on its own after the set time |

**Timers**

- Add with quick buttons (+15S, +1M, +5M, +10M, +20M) or a name plus minutes.
- Each shows a live countdown and progress bar, with pause/resume and remove.
- A finished timer shows DONE and can be restarted.
- The column header counts running timers.

**Alarms**

- Add with a time and a label; each has an on/off switch and remove.
- The list is sorted by time, and the header counts how many are on.

**News**

- Fetched on start, then every 15 minutes while the app runs, and again on resume from sleep.
- The last good result is cached in SQLite, so the column is never empty when offline.
- Stories are shown in feed order; the feed's title suffix (" - Publisher") is stripped and the publisher is shown on its own line.

**Reminders**

- Add with text and an optional time.
- Tick to mark done (struck through), or remove. The header counts open reminders.
- Google Tasks sync is toggled in the column header.
  - Reminders without a time sync both ways and are tagged GOOGLE.
  - Reminders with a time stay on the clock, tagged LOCAL, because the Tasks API keeps only the date. A note explains this when a time is entered.
  - Sync needs in-app Google sign-in with the Tasks permission.

All times display in 12-hour format, including timeline markers, axis labels, hourly weather and sunrise/sunset.

## Technical notes

Run full screen on the laptop display and watch only the app's own window for mouse movement. That avoids GNOME Wayland's limits on watching the whole desktop.

| Area | Approach |
|---|---|
| Idle detection | Pointer events on the app's own full-screen window plus a timeout; no global idle API needed |
| Screen awake | Ask GNOME to inhibit idle and lock while the clock runs |
| Burn-in | Shift the layout a few pixels per minute on the idle screen |
| Window placement | GNOME Wayland won't let apps position themselves or force always-on-top; full screen sidesteps this |
| Tray icon | Optional only; GNOME needs the AppIndicator extension |
| Alarms and suspend | Store alarms as absolute times in SQLite. On resume, listen for logind's PrepareForSleep signal and handle missed alarms |
| Scheduler | A background process, so closing the window doesn't stop alarms |
| Credentials | Secret Service (GNOME Keyring) via the `oo7` crate; never in SQLite |
| AI providers | One OpenAI-compatible adapter (OpenAI, Ollama, OpenRouter) plus native Anthropic and Gemini adapters |
| Provider interface | Must support tool/function calling (create reminder, set timer), streaming, and per-model capability flags |
| Clock commands | A rule-based parser handles "timer 20m" and similar offline, before any LLM |
| Speech-to-text | `whisper-rs` (whisper.cpp bindings); push-to-talk first |
| Wake word (later) | A dedicated detector (openWakeWord or `rustpotter`) plus voice activity detection, not Whisper running constantly |
| Text-to-speech | Piper, run as a separate process (now `OHF-Voice/piper1-gpl`, GPL-3.0) |
| Audio I/O | CPAL, which reaches PipeWire through ALSA |
| Weather | Open-Meteo (no API key) |
| News | The app fetches `https://news.google.com/rss?hl=en-US&gl=US&ceid=US:en` with `reqwest` and parses it with the `rss` crate; takes the first 3 items (title, `<source>`, `link`, `pubDate`). RSS is used instead of scraping the HTML page, which changes without notice. Google marks the feed for personal, non-commercial use, which fits this app |
| Opening links | `xdg-open` (or the `open` crate) on the item link, which goes through a news.google.com redirect |
| Packaging | RPM first; Flatpak later (the sandbox complicates mic, keyring and autostart) |
| Theme in Slint | One `global Theme` with dark/light values per token (see Design tokens) |
| Clock faces in Slint | `export enum ClockFace { digital, chronograph, split-flap, tape, dot-matrix }`, saved in settings. Each face is its own component taking the same inputs (hour, minute, second, AM/PM, a size) and is chosen with `if` branches in both the idle screen and the dashboard. Chronograph uses `Path` for ticks and hands; Split-flap animates on digit change; Tape slides a minute scale under a fixed needle; Dot matrix draws a 5×7 grid of rectangles per digit. No images needed |

## Google integration

The app can reach Google services once you authorize it. Start with GNOME Online Accounts for the calendar, and add in-app OAuth for Tasks and calendar editing. Google News needs no sign-in (see Technical notes).

| | GNOME Online Accounts | In-app OAuth ("Sign in from the clock") |
|---|---|---|
| How it works | Sign in once in GNOME Settings → Online Accounts; the app reads the calendar through GNOME (access token or Evolution Data Server over D-Bus) | The app opens Google's consent screen in the browser, catches the reply on a temporary localhost address, and calls Google's APIs directly |
| Setup | None in Google Cloud | Google Cloud project plus a "Desktop app" OAuth client |
| Credentials | Held by GNOME's keyring | Refresh token stored in the keyring via `oo7`, never in SQLite |
| Scope | Read-only calendar | Only what the app requests: calendar edit, Tasks |
| Rust crates | D-Bus client (`zbus`) | `oauth2` for the sign-in flow, `reqwest` for API calls |
| Limits | GNOME only | Unverified personal apps: in testing mode, tokens expire after about a week; production without verification shows an "unverified app" warning. Check Google's current rules when building |

- **Gemini** is separate. It needs only an API key in Settings → AI & Models, not a Google account sign-in.
- **Reminders and Google Tasks:** Google moved Assistant and Calendar reminders into Google Tasks, so the dashboard's reminders sync with Tasks. The Tasks API stores only a due date; the time is discarded and can't be read. So only reminders without a time sync. Timed ones stay local and fire from the clock's own scheduler.
- **Calendar events** now appear only as day-timeline markers, the idle screen's "next event" and the ringing-alarm bottom strip. Birthdays (all-day events) no longer have a place on screen.
- **Gmail** is no longer used.

## Open items

- [x] Redesign the Assistant screen in the Instrument style
- [x] Design the remaining Settings sections (Voice & Mic, Alarm sounds, Startup)
- [x] Design the ringing-alarm screen
- [x] Decide briefing playback (superseded: Briefing replaced by News)
- [x] Replace the Briefing column with News (top 3 Google News stories)
- [ ] Update the design canvas: News column on the dashboard (dark + light), and the dismissed-alarm screen without PLAY BRIEFING
- [x] Clock faces: four new faces plus Digital, chosen in Settings → Appearance
- [ ] Draw the clock faces in the light theme
- [ ] Update the main design canvas's Settings → Appearance board with the Clock face picker
- [ ] Design a 12-hour time picker for Slint to replace the browser's built-in picker
- [ ] Decide the transition between idle and dashboard (cut or fade)
- [ ] Choose the default local model and first cloud provider for routing (prototype assumes `llama3.1:8b` on Ollama, OpenRouter for coding/research)
- [ ] Confirm the Google connection: GNOME Online Accounts first, in-app OAuth for Tasks / calendar edit
- [ ] Choose which Google Tasks list syncs (default list for now)
- [ ] Scaffold the Slint project: `global Theme`, idle/dashboard state switching, live clock (Phase 1 in `DEVELOPMENT.md`)
