# Smart Clock — Layout & Resolution Scaling

Two related fixes:

1. **Dashboard top row:** center the clock and make it bigger, and pin the weather panel to the right edge.
2. **Resolution scaling:** the same layout fills both **1920×1080** and **2560×1440**, from one set of sizes.

| Reference | Where |
|---|---|
| Sizes and tokens | `HANDOFF.md` → Design tokens, Decisions made (all values are 1080p) |
| Main build guide | `DEVELOPMENT.md` |

> **Order:** do Part 1 (scaling) first. Part 2 uses `Theme.u` everywhere, so the top row only needs to be written once.

---

## Part 1 — Resolution scaling

### 1.1 The idea

Every size in the design is a **1080p value**. A single unit, `Theme.u`, converts it to real pixels:

| Display | `Theme.u` | `320 * Theme.u` |
|---|---|---|
| 1920×1080 | 1 px | 320 px |
| 2560×1440 | 1.333 px | 427 px |
| 1920×1200 (16:10) | 1 px (height limits it) | 320 px |

`Theme.u` is the smaller of `width / 1920` and `height / 1080`, so nothing ever runs off the screen.

### 1.2 Add the unit to `ui/theme.slint`

```slint
export global Theme {
    // 1u = 1px at 1920×1080, 1.333px at 2560×1440.
    // Set by AppWindow on start and on every resize. Don't set it anywhere else.
    in-out property <length> u: 1px;

    // … existing color and font tokens stay as they are …
}
```

### 1.3 Keep it updated in `ui/app.slint`

```slint
import { Theme } from "theme.slint";

export component AppWindow inherits Window {
    // … existing properties …

    init => { Theme.u = min(self.width / 1920, self.height / 1080); }
    changed width  => { Theme.u = min(self.width / 1920, self.height / 1080); }
    changed height => { Theme.u = min(self.width / 1920, self.height / 1080); }

    // … rest of the window …
}
```

> **Slint version:** `changed` handlers need Slint **1.7 or newer** (`cargo tree -p slint` shows your version). On an older version, update the property from Rust instead. Add `in-out property <length> unit;` to `AppWindow`, bind `Theme.u` to it, and set it from a resize callback.

### 1.4 Convert existing sizes

Every fixed size becomes `N * Theme.u`, where N is the 1080p value.

| Kind of size | Before | After |
|---|---|---|
| Font size | `font-size: 15px;` | `font-size: 15 * Theme.u;` |
| Padding, spacing | `padding: 48px;` | `padding: 48 * Theme.u;` |
| Width, height | `width: 640px;` | `width: 640 * Theme.u;` |
| Letter spacing | `letter-spacing: 1.8px;` | `letter-spacing: 1.8 * Theme.u;` |
| Corner radius | `border-radius: 4px;` | `border-radius: 4 * Theme.u;` |
| **Hairlines and borders** | `height: 1px;` / `border-width: 1px;` | **Leave as `1px`**, so they stay sharp |
| Sizes taken from a parent | `parent.height * 0.6` | Leave as is; they already scale |
| `preferred-width` / `preferred-height` on `AppWindow` | `1920px` / `1080px` | Leave as is; this is only the starting window size |

#### Automatic pass (review the diff before you keep it)

This script rewrites every `<number>px` to `<number> * Theme.u`, except `0px`, `1px` and the window's `preferred-width`/`preferred-height` lines. Commit first so you can review and undo.

```bash
git add -A && git commit -m "before Theme.u conversion"

find crates/app/ui -name '*.slint' ! -name 'theme.slint' -print0 |
  xargs -0 perl -pi -e '
    next if /preferred-(width|height)/;
    s/(?<![\w.])(\d+(?:\.\d+)?)px\b/($1 eq "0" || $1 eq "1") ? "${1}px" : "$1 * Theme.u"/ge;
  '

git diff --stat
git diff crates/app/ui | less
```

Then add `import { Theme } from "theme.slint";` (adjust the relative path, e.g. `"../theme.slint"` in `widgets/`) to any file that now uses `Theme.u` without importing it. `cargo build` lists them as `Unknown unqualified identifier 'Theme'`.

> **Review these by hand:**
> - Negative offsets such as `x: -4px` are converted too (`-4 * Theme.u`), which is correct.
> - Pixel values inside string text (e.g. `"480px"` shown in Settings) would also be changed. Search the diff for `"` lines.
> - `1px` lines that are not hairlines (a 1 px gap you want to scale) can be changed by hand.

---

## Part 2 — Dashboard top row

### 2.1 What's wrong now

The clock group and the weather panel are sized separately, so the leftover space ends up to the **right of the weather**, and the clock sits left of center.

### 2.2 The fix

Put both in one `HorizontalLayout`:

- **Clock area:** `horizontal-stretch: 1`, so it takes all remaining space, with its contents centered.
- **Weather panel:** fixed width, `horizontal-stretch: 0`, so it lands on the right edge.

Remove any `x:` value or fixed `width` on the clock container; either one stops the layout from doing this.

### 2.3 `ui/dashboard.slint` — top row

```slint
import { Theme } from "theme.slint";

// … inside the Dashboard component, before the first row …

// Layout budget for the top row
property <length> side-pad: 48 * Theme.u;       // left margin
property <length> right-buffer: 64 * Theme.u;   // space kept clear right of the weather
property <length> gap: 48 * Theme.u;            // between clock area and weather
property <length> weather-w: 640 * Theme.u;
property <length> clock-room: root.width - side-pad - right-buffer - gap - weather-w;
// The clock group (time + seconds + side minutes + box padding) is ~5× the
// time's font size wide for "12:59". Cap at the 320 design size; shrink to fit.
property <length> clock-size: min(320 * Theme.u, clock-room / 5);

HorizontalLayout {
    padding-left: root.side-pad;
    padding-right: root.right-buffer;
    padding-top: 24 * Theme.u;
    spacing: root.gap;
    height: 500 * Theme.u;

    // ── Clock: fills the space left of the weather; contents centered ──
    VerticalLayout {
        horizontal-stretch: 1;
        alignment: center;
        spacing: 16 * Theme.u;

        Text {
            text: root.date-label;               // "FRIDAY, 13 NOVEMBER 2026"
            font-family: Theme.mono;
            font-size: 15 * Theme.u;
            letter-spacing: 1.8 * Theme.u;
            color: Theme.muted;
            horizontal-alignment: center;
        }

        HorizontalLayout {
            alignment: center;                   // centers the whole clock group
            spacing: 40 * Theme.u;

            // Previous minute
            Text {
                text: root.prev-minute;          // "37"
                font-family: Theme.mono;
                font-weight: 300;
                font-size: root.clock-size * 0.4;
                color: Theme.muted;
                vertical-alignment: center;
            }

            // Current time box
            Rectangle {
                background: Theme.panel;
                border-width: 1px;               // hairline: stays 1px
                border-color: Theme.line;
                border-radius: 4 * Theme.u;

                HorizontalLayout {
                    padding-left: 56 * Theme.u;
                    padding-right: 56 * Theme.u;
                    padding-top: 24 * Theme.u;
                    padding-bottom: 24 * Theme.u;
                    spacing: 20 * Theme.u;

                    Text {
                        text: root.time-hm;      // "4:38"
                        font-family: Theme.mono;
                        font-weight: 300;
                        font-size: root.clock-size; // 320 at most; shrinks to fit
                        color: Theme.text;
                        vertical-alignment: center;
                    }
                    Text {
                        text: root.time-ss;      // "07"
                        font-family: Theme.mono;
                        font-weight: 300;
                        font-size: root.clock-size * 0.3125;
                        color: Theme.accent-text;
                        vertical-alignment: center;
                    }
                }
            }

            // Next minute
            Text {
                text: root.next-minute;          // "39"
                font-family: Theme.mono;
                font-weight: 300;
                font-size: root.clock-size * 0.4;
                color: Theme.muted;
                vertical-alignment: center;
            }
        }

        Text {
            text: "HOUR  /  MINUTE";
            font-family: Theme.mono;
            font-size: 13 * Theme.u;
            letter-spacing: 1.6 * Theme.u;
            color: Theme.muted;
            horizontal-alignment: center;
        }
    }

    // ── Weather: fixed width, pinned to the right edge ──
    WeatherPanel {
        width: root.weather-w;
        horizontal-stretch: 0;
    }
}
```

> **Why the weather overran the right edge:** a `HorizontalLayout` can't shrink text below its natural width. If the clock group is wider than its space, the row overflows to the right and pushes the weather past the margin. Sizing the clock from `clock-room` stops that, and `right-buffer` keeps a clear gap at the edge.

> **Also check inside `WeatherPanel`:** if its own content is wider than 640 units (8 hourly columns at fixed widths, long condition text), it overflows on its own. Give the hourly columns `horizontal-stretch: 1` instead of fixed widths, and use `overflow: elide` on condition text such as "PARTLY CLOUDY".

> **Property names:** `prev-minute`, `next-minute`, `time-hm` and `time-ss` are placeholders; use the names your dashboard already has. The `HOUR / MINUTE` label moves under the box, where it reads as a caption instead of crowding the digits.

### 2.4 Adjusting the size

| To change | Edit |
|---|---|
| Space right of the weather | `right-buffer` (64 units) |
| Largest clock size | `320 * Theme.u` in `clock-size` |
| Weather width | `weather-w` (640 units). A narrower panel gives the clock more room |
| Row height | `height: 500 * Theme.u` on the row |
| Center on the whole screen instead | Take `WeatherPanel` out of this row and place it in a `Rectangle` overlay with `x: parent.width - self.width - root.right-buffer` |

What the clock comes out at (1080p units, so the same at 1440p after scaling):

| Weather width | Side minutes | Clock size |
|---|---|---|
| 640 | shown | ≈ 224 |
| 560 | shown | ≈ 240 |
| 640 | hidden (remove the 37/39 texts, use `clock-room / 3.8`) | ≈ 295 |
| 520 | hidden | ≈ 320 (full size) |

> **Getting to the full 320:** with a 640 weather panel beside it, the time and the side minutes can't all be 320 at 1920 wide. Either accept about 224, narrow the weather, or drop the 37/39 side minutes on the dashboard and keep them for the idle screen, which has the full width.

---

## Part 3 — Test both resolutions

You don't need a second monitor.

```bash
cargo run -p smart-clock -- --windowed
```

Then resize the window:

| Window size | Expect |
|---|---|
| 1920×1080 | `Theme.u` = 1. Clock 320 px, weather 640 px wide, flush right |
| 2560×1440 | `Theme.u` = 1.333. Everything ~33% larger, same proportions |
| Narrow (e.g. 1600×1080) | Everything shrinks to fit the width; nothing is clipped |

> **Exact sizes on GNOME Wayland:** apps can't set their own window size there. Either resize by dragging, or run under X11 for testing with `GDK_BACKEND=x11` (or `WAYLAND_DISPLAY= cargo run …` to force XWayland), then use `xdotool search --name "Smart Clock" windowsize 1920 1080`.

To check the value while testing, show it temporarily:

```slint
// anywhere in AppWindow, remove when done
Text { text: "u = " + Theme.u / 1px; color: red; x: 8px; y: 8px; }
```

---

## Checklist

- [ ] `Theme.u` added to `theme.slint` (1.2)
- [ ] `AppWindow` sets it on init and resize (1.3)
- [ ] All `.slint` sizes converted; hairlines left at `1px`; diff reviewed (1.4)
- [ ] Dashboard top row rebuilt: clock centered, weather pinned right (2.3)
- [ ] Weather has a clear gap on the right; clock shrinks to fit instead of pushing it (2.3, 2.4)
- [ ] Decide: accept ~224, narrow the weather, or hide side minutes on the dashboard (2.4)
- [ ] Checked at 1920×1080 and 2560×1440 (Part 3)

**Done when:** at both resolutions the clock is centered in the space left of the weather, the weather panel touches the right margin, and every element keeps the same proportions.
