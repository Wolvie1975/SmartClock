# Installing Smart Clock

How to set up Smart Clock on a Fedora Workstation laptop (GNOME on Wayland), from a fresh machine to a running clock with local AI.

For how the code is organised and the build plan, see [DEVELOPMENT.md](DEVELOPMENT.md).

---

## 1. System packages

```bash
sudo dnf install -y \
  gcc gcc-c++ clang clang-devel cmake pkgconf-pkg-config git \
  fontconfig-devel freetype-devel \
  libxkbcommon-devel libxkbcommon-x11-devel libxkbcommon-x11 wayland-devel libxcb-devel \
  alsa-lib-devel
```

Optional: `sudo dnf install -y sqlite` gives you the `sqlite3` command for looking inside the clock's database. The app doesn't need it.

## 2. Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup component add rust-src rustfmt clippy
```

Open a new terminal afterwards, or run `source "$HOME/.cargo/env"`, so `cargo` is on your PATH.

## 3. Build and run

```bash
git clone https://github.com/Wolvie1975/SmartClock.git
cd SmartClock
git switch development
```

### 3.1 Add it to the app menu (recommended)

```bash
scripts/install-local.sh
```

This builds a release copy and adds **Smart Clock** to the GNOME app menu with its icon. It does **not** start at login; you open it yourself. Run the same command again after pulling new code to update it.

- Open it from the app menu (press Super and type "Smart Clock"). It opens full screen.
- Right-click its icon for **Open in a Window**.
- Quit with **CLOSE** at the top right (it asks once, because alarms stop while the clock is closed), or Alt+F4.
- Remove it with `scripts/install-local.sh --uninstall`. That keeps your alarms, reminders and settings.

It installs `~/.local/bin/smart-clock`, `~/.local/share/applications/smart-clock.desktop` and the icon in `~/.local/share/icons/hicolor/scalable/apps/`.

### 3.2 Run from the source folder (development)

```bash
cargo run --release -p smart-clock                # full screen
cargo run -p smart-clock -- --windowed            # in a window, debug build
```

| Flag | What it does |
| --- | --- |
| `--windowed` | Run in a 1920×1080 window instead of full screen |
| `--idle-minutes N` | Override the idle timeout for this run only; fractions work (`0.25` = 15 s) |

The first build takes several minutes. Later builds are quick.

## 4. First-run setup

In the command bar at the bottom of the dashboard:

| Type | To |
| --- | --- |
| `location Austin, TX` | Set the weather location (use your own city; the state or country after the comma is optional) |

The **LIGHT / DARK** button in the top-right corner switches the theme. The app remembers your choice.

## 5. Local AI: Ollama in Docker

The assistant runs its everyday questions and the daily briefing on a local model through [Ollama](https://ollama.com), so that data stays on the laptop. Ollama runs in a Docker container.

Only the assistant needs Ollama. The clock, timers, alarms, reminders and the command bar all work without it.

### 5.1 Start Ollama

```bash
docker run -d --name ollama --restart unless-stopped \
  -p 127.0.0.1:11434:11434 \
  -v ollama:/root/.ollama \
  ollama/ollama
```

- `-p 127.0.0.1:11434:11434` makes Ollama reachable only from this laptop, not from your network.
- `-v ollama:/root/.ollama` keeps downloaded models in a Docker volume, so they survive container updates.
- `--restart unless-stopped` starts Ollama again after a reboot, as long as the Docker service is enabled (`sudo systemctl enable --now docker`).

If `docker` says *permission denied*, either put `sudo` in front of the docker commands, or add yourself to the `docker` group once and log out and back in:

```bash
sudo usermod -aG docker "$USER"
```

### 5.2 Download the model

```bash
docker exec ollama ollama pull llama3.2:3b
```

### 5.3 Check it works

```bash
curl http://127.0.0.1:11434/api/tags
```

The output should list `llama3.2:3b`. To try the model directly:

```bash
docker exec -it ollama ollama run llama3.2:3b "Say hello in five words."
```

### 5.4 Which model

This laptop runs models on the CPU (the integrated Intel graphics can't be used by Ollama), so smaller models are much faster.

| Model | Memory | Speed on this laptop | Notes |
| --- | --- | --- | --- |
| `llama3.2:3b` | ~2 GB | ~10–15 tokens/s | **Default.** Quick, supports tool calls |
| `qwen3:4b` | ~2.6 GB | roughly half the speed | Smarter answers |
| `llama3.1:8b` | ~5 GB | ~5 tokens/s | Only if you have the memory to spare |

To try another model, pull it the same way (`docker exec ollama ollama pull qwen3:4b`) and choose it in Settings → AI & Models once that screen exists.

### 5.5 Everyday commands

```bash
docker ps --filter name=ollama            # is it running?
docker logs --tail 50 ollama              # recent log output
docker exec ollama ollama list            # downloaded models
docker exec ollama ollama rm qwen3:4b     # delete a model you no longer want
docker stop ollama / docker start ollama  # stop or start it

# Update Ollama itself (models are kept in the volume)
docker pull ollama/ollama
docker rm -f ollama
# …then run the `docker run` command from 5.1 again
```

## 6. Cloud AI (later)

Coding and research questions go to a cloud model (Anthropic first, then Gemini). API keys are entered in Settings → AI & Models and stored in GNOME Keyring, never in the app's database. That screen arrives with the assistant in Phase 3.

## 7. Where things are kept

| What | Where |
| --- | --- |
| Alarms, timers, reminders, settings, cached weather | `~/.local/share/smart-clock/clock.db` |
| API keys and Google sign-in (later) | GNOME Keyring (open **Passwords and Keys** to see them) |
| Ollama models | Docker volume `ollama` |

## 8. Troubleshooting

| Problem | Fix |
| --- | --- |
| *libxkbcommon-x11.so could not be loaded* | `sudo dnf install -y libxkbcommon-x11` |
| No alarm or timer sound | Check the output device and volume in GNOME Settings → Sound. The app plays through PipeWire's ALSA plugin, which Fedora installs by default (`pipewire-alsa`). |
| Weather says OFFLINE or UNAVAILABLE | No network, or Open-Meteo is unreachable. It retries every 2 minutes and shows the last forecast meanwhile. |
| Weather says SET A LOCATION | Type `location <your city>` in the command bar. |
| Assistant can't reach the local model | `docker ps --filter name=ollama` should show it running, and `curl http://127.0.0.1:11434/api/tags` should list the model. |
| `ollama serve` says *bind: address already in use* | The Docker container is already serving on that port, so nothing is wrong. Don't run `ollama serve` on the host; use `docker exec ollama ollama …`. If Fedora's own package is installed (`rpm -q ollama-base`), remove it with `sudo dnf remove ollama-base` so there's only one Ollama. |
| An alarm didn't ring | Alarms only ring while the app is open (a background service comes in Phase 6). An alarm found more than 5 minutes late, for example after the laptop slept, is skipped. |

## 9. Uninstall

```bash
scripts/install-local.sh --uninstall      # remove the app from the menu
rm -rf ~/.local/share/smart-clock         # the clock's data
docker rm -f ollama                       # the Ollama container
docker volume rm ollama                   # downloaded models
```
