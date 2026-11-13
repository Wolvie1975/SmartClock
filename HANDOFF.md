# Smart Clock: session handoff

Where the project stands at the end of the first build session (2026-11-13), for whoever picks it up next, human or Claude. Read this first, then [DEVELOPMENT.md](DEVELOPMENT.md) (plan and design) and [INSTALL.md](INSTALL.md) (setup).

## State in one paragraph

A full-screen Rust + Slint desk clock for a Fedora 45 GNOME/Wayland laptop. Phases 1 and 2 are done: clock, idle screen, timers, alarms and reminders in SQLite, the offline command parser, timer takeover, alarm ringing with snooze, and the weather panel. Phase 3 is mostly done: a local assistant on Ollama with tool calls, the command-bar answer card, and the Assistant screen with saved conversations. The app is in the GNOME app menu via `scripts/install-local.sh`. All work is committed on `development`. The repo is `https://github.com/Wolvie1975/SmartClock` (branch `development`). Commits after `0b0b6e7` had not been pushed when this file was written; run `git status -sb` to check.

## The user's decisions and preferences (keep to these)

- **Scope: clock duties only.** Time, alarms, timers, reminders, today's schedule, weather and the briefing. **No coding or research features, and no general-assistant creep.** The user removed the design's coding/research routes on purpose.
- **AI routing is local-first.** Clock commands go to the built-in parser (no AI). Everything else, including the briefing, goes to local Ollama. The cloud (Anthropic first, Gemini later) is used **only** when the user flips a conversation to CLOUD. Never auto-escalate to the cloud.
- **Ollama runs in Docker** (container `ollama`, `127.0.0.1:11434`, model `llama3.2:3b`). The laptop is CPU-only (Intel UHD 630, 11 GB RAM, 6 cores), so small models only. Fedora's own `ollama-base` package was also installed and clashed; use `docker exec ollama ollama …`.
- **Nothing the clock itself needs may depend on Docker.** Storage is embedded SQLite (the user asked about SQL Server and agreed SQLite fits).
- **Weather:** °F and mph. The location is set by typing `location <city>` in the command bar. Forecast shows **today and tomorrow only**.
- **Launch:** from the GNOME app menu, opened manually. **No start at login.** Quit with the header's CLOSE button (with a confirm).
- **Workflow:** the user tests on screen and reports bugs. Commit after each step; the user asked to commit and move on. Ask before pushing.

## How to run and test

```bash
cargo run -p smart-clock -- --windowed                 # dev, windowed
cargo run -p smart-clock -- --windowed --idle-minutes 0.25   # idle screen after 15 s
scripts/install-local.sh                               # rebuild + update the app-menu copy
cargo clippy --workspace --all-targets                 # must stay clean
cargo test --workspace                                 # 45 tests
cargo test -p ai -- --ignored --nocapture              # live: Ollama chat + tool call
cargo test -p smart-clock -- --ignored --nocapture     # live: full assistant loop, follow-ups
```

- To try something without touching the real database, run with `XDG_DATA_HOME=<scratch dir>`. The real data is in `~/.local/share/smart-clock/clock.db`.
- **Claude can't see the screen.** UI work has been built blind and checked by the user, who found three layout or behaviour bugs. Say so plainly, and ask the user to check visual changes.
- The design references (canvas `LBAE37B37bFNrFxicYzqxG`, handoff `1D8z1npXjRiowFfg6vKsbt`) can't be read through the Artifact tool, because both pages load their content at runtime. Work from DEVELOPMENT.md or ask the user for screenshots.

## Code map

| Path | What |
| --- | --- |
| `crates/clock-core/src/command.rs` | Offline parser + time rules (`parse`, `parse_clock_time`, `parse_date_text`) |
| `crates/clock-core/src/{model,store,actions}.rs` | Types; SQLite with append-only `MIGRATIONS` (3 so far); `apply` + `Undo` used by every input path |
| `crates/ai/src/{lib,openai}.rs` | `Provider` trait; OpenAI-compatible adapter (Ollama `/v1`) with SSE + tool-call assembly |
| `crates/app/src/main.rs` | Startup, 4 Hz tick, idle, theme, quit, app id |
| `crates/app/src/dashboard.rs` | Dashboard columns, command bar, location / ask hooks |
| `crates/app/src/ringing.rs`, `sound.rs` | Timer takeover, alarm ringing, snooze; synthesized tones via cpal |
| `crates/app/src/weather.rs` | Open-Meteo fetch, geocoding, cache, panel view, text summary for the model |
| `crates/app/src/assistant.rs`, `tools.rs` | Answer card + conversations; model loop; tools wired to `actions::apply` |
| `crates/app/ui/*.slint` | `app` (window, header), `dashboard`, `assistant`, `takeover`, `weather`, `state` (globals `Dash`, `Ring`, `Weather`, `Chat`), `theme` |
| `scripts/install-local.sh`, `data/` | App-menu install, launcher template, icon |

## Lessons from this session (avoid repeating)

- **Slint:** an element given its own `x`/`y` outside a layout defaults to its parent's **full width**. The header theme button covered the whole tab row and caught every click. Keep controls inside layouts, or set an explicit width.
- **Slint:** `row` is a built-in property, so name struct properties `item`. Flickable's `viewport-*` is now `content-*`. A component can't refer to `parent` in its own definition. `slint::set_xdg_app_id` must be called **after** `AppWindow::new()` and before `run()`.
- **zbus 6** has no `blocking` module (`inhibit.rs` uses a small tokio runtime on a thread). **reqwest 0.13** needs the `query` feature for `.query()`. In **cpal 0.18**, `SampleRate` is a `u32`. `cargo new` inside a repo once created a nested `.git` in `clock-core`.
- **`cargo fmt` reformats code before scripted edits run.** Match on the formatted text, or read the file first.
- **llama3.2:3b:**
  - It sends numbers as strings (`"minutes": "5"`), so tools parse leniently.
  - It does bad time arithmetic, so tools return exact times ("rings at 9:01 AM").
  - It sometimes writes a tool call as JSON text, which the loop recovers.
  - It invents details, so successful actions get app-written confirmations and skip the model's second round.
  - It confuses "SAT" with today, so the weather summary names "Today (Friday)" and "Tomorrow (Saturday)".
  - It turned "and tomorrow?" into a reminder, so `add_reminder` rejects date-only text, and the card keeps a short history.

## Known issues and limits

- **Alarms only ring while the app is open.** One found more than 5 minutes late (closed or asleep) is skipped and logged. Phase 6 adds the background scheduler and resume handling.
- **There's no Settings screen.** Values live in the `settings` table: `idle_minutes`, `snooze_min` (9), `autostop_min` (10), `ramp` (true), `volume` (80), `ollama_url`, `ollama_model`, `temp_unit`, `wind_unit`, `weather_*`, `theme`, `start_on`.
- **Not yet confirmed on screen:** the alarm ringing, snooze and dismiss screens, the timer takeover, idle wake on mouse movement, the light theme, and the CLOSE confirm. The user has checked the dashboard, the weather panel and the answer card pop-over.
- **The small model still slips on wording now and then,** for example calling tomorrow's weather "Today". The actions are reliable.
- **Model-created timers lose their name** ("tea") when the model leaves it out. Typed commands keep names, because the parser handles them.
- **A CLOUD conversation sends nothing** and shows a "not set up" notice.

## Next steps (in order)

1. **Push to GitHub, after asking the user:** `git push origin development`.
2. **Run a 10-minute on-screen check with the user:** `tea timer 15s` for the takeover; an alarm 2 minutes ahead, snoozed, then dismissed; `--idle-minutes 0.25` with mouse wake; light mode; CLOSE → CANCEL → CLOSE.
3. **Phase 3, rest:**
   - The **Anthropic adapter** in `crates/ai`. There's no official Rust SDK, so call the Messages API over raw HTTP with `reqwest`, with streaming and tool use. Load the `claude-api` skill first for the current API shapes. The default model is `claude-opus-5-5`; the user hasn't chosen otherwise.
   - **Settings → AI & Models:** the Ollama URL and model, plus the cloud provider, model and API key. The key goes in GNOME Keyring via `oo7`, never in SQLite.
   - Then **Gemini**.
4. **A Settings screen** for the values above: idle, alarm sounds, snooze, volume, theme, start screen.
5. **Phase 4:** voice (cpal capture, whisper-rs pinned to an exact version, Piper as a separate process) and the briefing, which runs on the local model only.
6. **Phase 5:** Google (calendar via GOA, Tasks sync for untimed reminders, Gmail unread count only).
7. **Phase 6:** the background scheduler as a systemd user service, logind resume and catch-up, and RPM packaging.
