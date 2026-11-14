# Smart Clock — Night Dimming (In-App Overlay)

Dims the **screensaver view** at night with a black overlay and an optional low-contrast night palette. It runs entirely in the app: no D-Bus, no root, and it doesn't touch the hardware backlight.

> **Scope:** This affects the screensaver/idle view only. When the mouse moves and the dashboard appears, dimming turns off. When the app goes back to the screensaver, it turns back on if it's still within the night window.

---

## 1. Overview

| Piece | Where | Purpose |
|---|---|---|
| `night-mode` property | Slint (`ScreensaverView`) | Turns the overlay and night palette on or off |
| `dim-level` property | Slint | Overlay opacity (0.0–0.9) |
| `NightSchedule` | Rust (`src/night.rs`) | Decides whether "now" is inside the night window |
| Settings fields | Slint Settings + config file | Start time, end time, dim level, enable toggle |
| 30 s timer | Rust (`main.rs`) | Re-checks the schedule and pushes `night-mode` to the UI |

---

## 2. Dependencies

Add to `Cargo.toml` (skip any you already have):

```toml
[dependencies]
chrono = "0.4"
serde = { version = "1", features = ["derive"] }
```

---

## 3. Slint — overlay in the screensaver view

In `ui/screensaver.slint`, add the properties and put the overlay **last** inside the root element so it draws on top of everything:

```slint
export component ScreensaverView inherits Rectangle {
    in property <bool> night-mode: false;
    in property <float> dim-level: 0.65;   // 0.0 = no dim, 0.9 = very dark

    // ... existing clock face, weather, etc. ...

    // Night dim overlay — keep this as the LAST child
    Rectangle {
        width: 100%;
        height: 100%;
        background: black;
        opacity: root.night-mode ? root.dim-level : 0.0;
        animate opacity { duration: 2s; easing: ease-in-out; }
    }
}
```

> **Tip:** The 2-second fade matters. If the overlay snaps on at 10:00 PM, a dark bedroom suddenly goes darker, which is jarring. A slow fade is barely noticeable.

---

## 4. Slint — optional night palette

An overlay alone makes bright colors look muddy. Swapping the clock to a dim amber palette looks better and gives off less light.

Add to your theme globals (for example `ui/theme.slint`):

```slint
export global NightPalette {
    out property <color> digits:  #b8611f;   // warm amber
    out property <color> accent:  #6e3a14;
    out property <color> muted:   #3a2414;
    out property <color> bg:      #000000;
}
```

Then pick colors in the screensaver based on `night-mode`:

```slint
Text {
    text: root.time-text;
    color: root.night-mode ? NightPalette.digits : Theme.clock-fg;
    animate color { duration: 2s; }
}
```

Do the same for the background (`NightPalette.bg`) and any secondary text (`NightPalette.muted`).

> **Clock faces:** Each face (Digital, Chronograph, Split-flap, Tape, Dot matrix) should take its colors from properties instead of hard-coding them, so the night palette applies to every face. Pass `night-mode` down into whichever face component is active.

---

## 5. Settings UI

Add a "Night dimming" group to the Settings view:

```slint
// in ui/settings.slint
in-out property <bool>   night-enabled: true;
in-out property <string> night-start: "10:00 PM";
in-out property <string> night-end:   "6:00 AM";
in-out property <float>  night-dim:   0.65;

GroupBox {
    title: "Night dimming";
    VerticalLayout {
        spacing: 8px;
        CheckBox { text: "Dim screensaver at night"; checked <=> root.night-enabled; }
        HorizontalLayout {
            spacing: 8px;
            Text { text: "From"; vertical-alignment: center; }
            LineEdit { text <=> root.night-start; enabled: root.night-enabled; }
            Text { text: "to"; vertical-alignment: center; }
            LineEdit { text <=> root.night-end; enabled: root.night-enabled; }
        }
        HorizontalLayout {
            spacing: 8px;
            Text { text: "Dim level"; vertical-alignment: center; }
            Slider {
                minimum: 0.0; maximum: 0.9;
                value <=> root.night-dim;
                enabled: root.night-enabled;
            }
        }
    }
}
```

> **Note:** Times are entered in 12-hour format to match the rest of the app. Swap the `LineEdit`s for a time picker later if you want; the Rust parser below accepts `h:mm AM/PM`.

---

## 6. Config struct

Add to your existing config (for example `src/config.rs`):

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightConfig {
    pub enabled: bool,
    pub start: String,   // "10:00 PM"
    pub end: String,     // "6:00 AM"
    pub dim_level: f32,  // 0.0–0.9
}

impl Default for NightConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            start: "10:00 PM".into(),
            end: "6:00 AM".into(),
            dim_level: 0.65,
        }
    }
}
```

Then add `#[serde(default)] pub night: NightConfig,` to your top-level `Config` so older config files still load.

---

## 7. Rust — schedule logic

Create `src/night.rs`:

```rust
use chrono::{Local, NaiveTime};

/// Parse "10:00 PM" / "6:00 AM" (12-hour). Falls back to None on bad input.
pub fn parse_12h(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%I:%M %p")
        .or_else(|_| NaiveTime::parse_from_str(s.trim(), "%l:%M %p"))
        .ok()
}

/// True if `now` falls inside [start, end). Handles windows that cross midnight.
pub fn in_window(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        now >= start && now < end          // e.g. 1:00 AM – 5:00 AM
    } else {
        now >= start || now < end          // e.g. 10:00 PM – 6:00 AM
    }
}

pub fn is_night(cfg: &crate::config::NightConfig) -> bool {
    if !cfg.enabled {
        return false;
    }
    match (parse_12h(&cfg.start), parse_12h(&cfg.end)) {
        (Some(s), Some(e)) => in_window(Local::now().time(), s, e),
        _ => false, // bad config → never dim rather than always dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(h: u32, m: u32) -> NaiveTime { NaiveTime::from_hms_opt(h, m, 0).unwrap() }

    #[test]
    fn crosses_midnight() {
        let (s, e) = (t(22, 0), t(6, 0));
        assert!(in_window(t(23, 30), s, e));
        assert!(in_window(t(2, 0), s, e));
        assert!(!in_window(t(6, 0), s, e));
        assert!(!in_window(t(12, 0), s, e));
    }

    #[test]
    fn same_day() {
        let (s, e) = (t(1, 0), t(5, 0));
        assert!(in_window(t(3, 0), s, e));
        assert!(!in_window(t(23, 0), s, e));
    }

    #[test]
    fn parses_12h() {
        assert_eq!(parse_12h("10:00 PM"), Some(t(22, 0)));
        assert_eq!(parse_12h("6:00 AM"), Some(t(6, 0)));
        assert_eq!(parse_12h("12:30 AM"), Some(t(0, 30)));
    }
}
```

Register it in `main.rs` with `mod night;`.

---

## 8. Rust — wire it to the UI

In `main.rs`, after creating the window, start a timer that re-checks the schedule every 30 seconds. Also run it once at startup so the app doesn't wait 30 s for the first update:

```rust
use slint::{Timer, TimerMode};
use std::time::Duration;

let ui_weak = ui.as_weak();
let config = config.clone(); // Arc<RwLock<Config>> or similar shared handle

let apply_night = {
    let ui_weak = ui_weak.clone();
    let config = config.clone();
    move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let cfg = config.read().unwrap().night.clone();
        ui.set_night_mode(night::is_night(&cfg));
        ui.set_night_dim(cfg.dim_level.clamp(0.0, 0.9));
    }
};

apply_night(); // initial state

let night_timer = Timer::default();
night_timer.start(TimerMode::Repeated, Duration::from_secs(30), apply_night.clone());
// keep `night_timer` alive for the life of the app (don't let it drop)
```

> **Property names:** `ui.set_night_mode` / `ui.set_night_dim` assume you've forwarded the properties up to your root `AppWindow`:
>
> ```slint
> export component AppWindow inherits Window {
>     in property <bool>  night-mode;
>     in property <float> night-dim: 0.65;
>     // ...
>     if root.screensaver-active : ScreensaverView {
>         night-mode: root.night-mode;
>         dim-level:  root.night-dim;
>     }
> }
> ```
>
> Because the overlay lives in `ScreensaverView`, the dashboard is never dimmed and you don't need extra logic on mouse wake.

### Apply Settings changes right away

When the user saves Settings, write the new values into the config and call the same check again, so changes show up immediately instead of on the next 30 s tick:

```rust
ui.on_save_settings({
    let config = config.clone();
    let apply_night = apply_night.clone();
    move |night_enabled, night_start, night_end, night_dim| {
        {
            let mut c = config.write().unwrap();
            c.night.enabled = night_enabled;
            c.night.start = night_start.to_string();
            c.night.end = night_end.to_string();
            c.night.dim_level = night_dim;
            c.save().ok();
        }
        apply_night();
    }
});
```

(Adjust the callback signature to match your existing `save-settings` callback.)

---

## 9. Testing checklist

| Test | How | Expected |
|---|---|---|
| Unit tests | `cargo test night` | All pass |
| Night window now | Set start/end to bracket the current time, save | Screensaver fades dark over ~2 s |
| Outside window | Set the window to a different part of the day, save | Overlay fades out |
| Crosses midnight | Default 10:00 PM – 6:00 AM | Dim at 11 PM and at 2 AM, not at noon |
| Mouse wake | Move mouse while dimmed | Dashboard shows at full brightness |
| Back to idle | Let it go idle again inside the window | Screensaver comes back dimmed |
| Bad input | Enter `25:99` as the start time | No dimming, no crash |
| Disabled | Uncheck "Dim screensaver at night" | Never dims |
| All clock faces | Switch faces in Settings at night | Each face uses the amber night palette |

> **Quick test without waiting for night:** Temporarily set the window to the next few minutes, for example `9:15 PM` to `9:20 PM`, and watch the fade in and out.

---

## 10. Later: real backlight control (optional)

The overlay darkens pixels, but the panel backlight stays at its current level. If the room still looks too bright, add hardware dimming through systemd-logind's `Session.SetBrightness` over D-Bus (`zbus` crate, no root needed). Run it on the same `is_night()` signal, and restore the saved brightness on mouse wake and on exit. You can build that on top of this without changing anything above.
