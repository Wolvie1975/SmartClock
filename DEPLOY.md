# Deploy Smart Clock to another machine

This guide covers a fresh deployment to a Fedora Workstation machine running GNOME. It builds Smart Clock on the destination so the binary links against that machine's libraries.

For the full system-package list and Rust setup, see [INSTALL.md](INSTALL.md). The app is installed per user, does not start at login, and alarms ring only while the app is running.
Internet is needed for the initial build and model download; News, weather, and optional Google services also need network access.

## 1. Transfer the source

The `development` branch may contain local commits that are not on GitHub yet. A Git bundle transfers committed history without pushing:

On the current machine, from the repository root:

```bash
git status --short
git bundle create SmartClock.bundle development
```

The bundle includes committed changes only. Commit any changes you need before creating it. Copy `SmartClock.bundle` to the destination with a USB drive or another trusted transfer method.

On the destination:

```bash
git clone /path/to/SmartClock.bundle SmartClock
cd SmartClock
git switch development
git log -1 --oneline
```

If the intended version has already been pushed, you can instead clone the repository's `development` branch from GitHub.

## 2. Install destination prerequisites

Install the Fedora packages and Rust toolchain listed in [INSTALL.md](INSTALL.md) sections 1 and 2. Then install Docker Engine using the current [Docker instructions for Fedora](https://docs.docker.com/engine/install/fedora/), and enable its service:

```bash
sudo systemctl enable --now docker
sudo docker version
```

The final command should show both the Docker client and server. This guide uses `sudo docker` so it does not require adding your account to the `docker` group, which grants root-equivalent privileges.

## 3. Create the local AI service

Start Ollama in Docker, bound to localhost so it is not exposed to the LAN:

```bash
sudo docker run -d --name ollama --restart unless-stopped \
  -p 127.0.0.1:11434:11434 \
  -v ollama:/root/.ollama \
  ollama/ollama
```

The named volume keeps models when the container is replaced. Download the default model:

```bash
sudo docker exec ollama ollama pull llama3.2:3b
```

Verify that Ollama is reachable and the model is listed:

```bash
curl -fsS http://127.0.0.1:11434/api/tags
sudo docker exec ollama ollama list
```

The model download is roughly 2 GB. It runs locally after download; the assistant does not need an external AI service for LOCAL conversations.

## 4. Build and install Smart Clock

From the cloned repository:

```bash
./scripts/install-local.sh
```

This builds the optimized release on the destination and adds Smart Clock to that user's GNOME app menu. Search for **Smart Clock** to launch it. Re-run the same script after updating the source.

## 5. First launch

- In **Settings → AI & Models**, use `http://127.0.0.1:11434` and `llama3.2:3b` for the local model, then save. These are the defaults, but confirm them if the assistant cannot connect.
- Set a weather location from the dashboard command bar, for example `location Austin, TX`.
- The News column and weather need internet access. News refreshes every 15 minutes and displays its last successful cache when offline.
- Google Calendar and Tasks are optional. Calendar uses GNOME Online Accounts; Tasks requires its OAuth client ID and sign-in on the destination.

Docker starts with the machine, and `--restart unless-stopped` starts the Ollama container after Docker starts. Check its state with:

```bash
sudo docker ps --filter name=ollama
```

## 6. Moving existing clock data

Smart Clock stores alarms, timers, reminders, preferences, and cached News in `~/.local/share/smart-clock/clock.db`. If you want to carry that data over, close Smart Clock on both machines, then copy the `smart-clock` data directory to the same location for the destination user. Back up the destination directory first if it already contains data.

Google refresh tokens and cloud API keys are stored in GNOME Keyring, not in the database. They are not transferred with the database; reconnect Google and re-enter any cloud credentials on the destination. The Ollama model is separate from the clock database and can be downloaded again using section 3.

## 7. Troubleshooting

| Problem | Check |
|---|---|
| Assistant cannot connect | `sudo docker ps --filter name=ollama`; then `curl http://127.0.0.1:11434/api/tags`. Confirm the URL in AI settings. |
| Model is missing | `sudo docker exec ollama ollama pull llama3.2:3b` |
| Port 11434 is already in use | Stop any other Ollama service/container using that port. Do not start a second host `ollama serve`. |
| App fails to launch due to a missing library | Recheck the Fedora packages in [INSTALL.md](INSTALL.md) section 1. |
| Alarms do not ring after closing the app | Expected currently: alarms ring only while Smart Clock is open. |
