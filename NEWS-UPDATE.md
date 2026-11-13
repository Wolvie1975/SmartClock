# Smart Clock — News Column Update

Replaces dashboard column **04 Briefing** with **04 News**: the top 3 stories from Google News, fetched by the app itself.

| Reference | Where |
|---|---|
| Decisions and screen spec | `HANDOFF.md` → Decisions made, Dashboard → News column, Behavior → News |
| Main build guide | `DEVELOPMENT.md` (workspace layout, theme, phases) |

> **Scope:** this file covers only the News change. It assumes the workspace from `DEVELOPMENT.md` §5 exists (`crates/core`, `crates/app`, …). If you haven't scaffolded Phase 1 yet, do that first; the News column needs the dashboard to exist.

---

## 1. What changes

| Area | Before | After |
|---|---|---|
| Column 04 | Briefing: calendar list, Gmail count, PLAY BRIEFING | News: top 3 Google News stories, click to open |
| Voice / commands | "brief me" | Removed |
| Dismissed alarm screen | "Good morning" + PLAY BRIEFING | "Good morning" + link to dashboard |
| Google OAuth scopes | Calendar edit, Tasks, Gmail | Calendar edit, Tasks |
| AI routing | A "briefing summary" task on the local model | Removed |
| New crate | — | `crates/news` |
| New table | — | `news_cache` in SQLite |

---

## 2. Where the news comes from

The app does **not** scrape the Google News web page. It reads Google News' public RSS feed, which gives the same top stories as clean XML and doesn't break when the page layout changes.

```text
https://news.google.com/rss?hl=en-US&gl=US&ceid=US:en
```

| Param | Meaning |
|---|---|
| `hl` | Interface language (`en-US`) |
| `gl` | Country edition (`US`) |
| `ceid` | Country:language pair (`US:en`) |

Each `<item>` in the feed has:

| Field | Example | Used for |
|---|---|---|
| `<title>` | `Headline text here - AP News` | Headline (strip the ` - Publisher` suffix) |
| `<source>` | `AP News` | Publisher line |
| `<link>` | `https://news.google.com/rss/articles/…` | Opened on click (redirects to the publisher) |
| `<pubDate>` | `Fri, 13 Nov 2026 19:02:00 GMT` | "2H AGO" |

No API key and no sign-in. The feed is marked for personal, non-commercial use, which fits this app.

> **Try it first:**
> ```bash
> curl -s "https://news.google.com/rss?hl=en-US&gl=US&ceid=US:en" | grep -oP '<title>\K[^<]+' | head -4
> ```
> The first title is the feed's own name ("Top stories - Google News"); the next three are what the column will show.

---

## 3. Create the `news` crate

```bash
cargo new --lib crates/news
cargo add -p news reqwest --no-default-features --features rustls
cargo add -p news rss chrono thiserror tracing
cargo add -p smart-clock news --path crates/news
cargo add -p smart-clock open        # opens links in the default browser
```

> **reqwest feature name:** reqwest 0.13 renamed the TLS feature from `rustls-tls` to `rustls`. If `cargo add` says `unrecognized feature … rustls-tls`, use `rustls`. The same change applies to the other `reqwest` lines in `DEVELOPMENT.md` §5.2.

### 3.1 `crates/news/src/lib.rs`

```rust
use chrono::{DateTime, Utc};
use std::time::Duration;

pub const TOP_STORIES_URL: &str =
    "https://news.google.com/rss?hl=en-US&gl=US&ceid=US:en";

#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub title: String,
    pub source: String,
    pub url: String,
    pub published: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum NewsError {
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("feed: {0}")]
    Feed(#[from] rss::Error),
    #[error("feed had no stories")]
    Empty,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("smart-clock/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

/// Fetch the feed and return the first `n` stories.
pub async fn fetch_top(client: &reqwest::Client, n: usize) -> Result<Vec<Story>, NewsError> {
    let bytes = client
        .get(TOP_STORIES_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let stories = parse(&bytes, n)?;
    if stories.is_empty() {
        return Err(NewsError::Empty);
    }
    Ok(stories)
}

/// Parse RSS bytes. Kept separate from the network so it can be unit-tested.
pub fn parse(xml: &[u8], n: usize) -> Result<Vec<Story>, NewsError> {
    let channel = rss::Channel::read_from(xml)?;
    Ok(channel
        .items()
        .iter()
        .filter_map(|item| {
            let raw_title = item.title()?.trim();
            let url = item.link()?.to_string();
            let source = item
                .source()
                .and_then(|s| s.title())
                .unwrap_or("")
                .trim()
                .to_string();
            Some(Story {
                title: strip_source_suffix(raw_title, &source),
                source,
                url,
                published: item
                    .pub_date()
                    .and_then(|d| DateTime::parse_from_rfc2822(d).ok())
                    .map(|d| d.with_timezone(&Utc)),
            })
        })
        .take(n)
        .collect())
}

/// "Headline - AP News" → "Headline"
fn strip_source_suffix(title: &str, source: &str) -> String {
    if !source.is_empty() {
        if let Some(t) = title.strip_suffix(&format!(" - {source}")) {
            return t.trim().to_string();
        }
    }
    title.to_string()
}

/// "NOW", "12M AGO", "2H AGO", "1D AGO" (uppercase per the Instrument labels)
pub fn age_label(published: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(p) = published else { return String::new() };
    let mins = (now - p).num_minutes().max(0);
    match mins {
        0..=1 => "NOW".into(),
        2..=59 => format!("{mins}M AGO"),
        60..=1439 => format!("{}H AGO", mins / 60),
        _ => format!("{}D AGO", mins / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Top stories - Google News</title>
<link>https://news.google.com</link><description>Google News</description>
<item><title>First headline - AP News</title><link>https://news.google.com/a</link>
<pubDate>Fri, 13 Nov 2026 19:02:00 GMT</pubDate><source url="https://apnews.com">AP News</source></item>
<item><title>Second - with dash - Reuters</title><link>https://news.google.com/b</link>
<source url="https://reuters.com">Reuters</source></item>
</channel></rss>"#;

    #[test]
    fn parses_and_strips_suffix() {
        let s = parse(SAMPLE.as_bytes(), 3).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "First headline");
        assert_eq!(s[0].source, "AP News");
        assert_eq!(s[1].title, "Second - with dash"); // only the trailing " - Source" goes
        assert!(s[1].published.is_none());
    }

    #[test]
    fn age_labels() {
        let now = Utc::now();
        assert_eq!(age_label(Some(now), now), "NOW");
        assert_eq!(age_label(Some(now - chrono::Duration::minutes(12)), now), "12M AGO");
        assert_eq!(age_label(Some(now - chrono::Duration::hours(2)), now), "2H AGO");
        assert_eq!(age_label(None, now), "");
    }
}
```

> **Check:** `cargo test -p news` should pass before you touch the UI.

---

## 4. Cache in SQLite

The column must never be empty when offline. Store the last good fetch.

### 4.1 Schema (add to the migrations in `crates/core`)

```sql
CREATE TABLE IF NOT EXISTS news_cache (
    position      INTEGER PRIMARY KEY,   -- 0, 1, 2
    title         TEXT NOT NULL,
    source        TEXT NOT NULL,
    url           TEXT NOT NULL,
    published_utc TEXT,                  -- RFC 3339, nullable
    fetched_utc   TEXT NOT NULL          -- RFC 3339
);
```

### 4.2 Repository functions (`crates/core`)

To keep `core` free of network code, `core` gets its own plain row type and the app converts between `news::Story` and it.

```rust
// crates/core/src/news_cache.rs
use rusqlite::{params, Connection};

pub struct CachedStory {
    pub title: String,
    pub source: String,
    pub url: String,
    pub published_utc: Option<String>,
    pub fetched_utc: String,
}

pub fn save(conn: &mut Connection, stories: &[CachedStory]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM news_cache", [])?;
    for (i, s) in stories.iter().enumerate() {
        tx.execute(
            "INSERT INTO news_cache (position, title, source, url, published_utc, fetched_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![i as i64, s.title, s.source, s.url, s.published_utc, s.fetched_utc],
        )?;
    }
    tx.commit()
}

pub fn load(conn: &Connection) -> rusqlite::Result<Vec<CachedStory>> {
    let mut stmt = conn.prepare(
        "SELECT title, source, url, published_utc, fetched_utc
         FROM news_cache ORDER BY position",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CachedStory {
            title: r.get(0)?,
            source: r.get(1)?,
            url: r.get(2)?,
            published_utc: r.get(3)?,
            fetched_utc: r.get(4)?,
        })
    })?;
    rows.collect()
}
```

Add `pub mod news_cache;` to `crates/core/src/lib.rs`.

---

## 5. Slint UI

### 5.1 `ui/widgets/news-column.slint`

```slint
import { Theme } from "../theme.slint";

export struct NewsItem {
    title: string,
    source: string,   // "AP NEWS"  (uppercase it in Rust)
    age: string,      // "2H AGO"
    url: string,
}

export component NewsColumn inherits VerticalLayout {
    in property <[NewsItem]> items;
    in property <string> footer;      // "UPDATED 2:15 PM · GOOGLE NEWS"
    in property <bool> offline;
    callback open(string);

    spacing: 0;

    // Column header, matching 01–03
    HorizontalLayout {
        height: 44px;
        Text {
            text: "04  NEWS";
            font-family: Theme.mono;
            font-size: 13px;
            letter-spacing: 1.5px;
            color: Theme.muted;
            vertical-alignment: center;
        }
    }
    Rectangle { height: 1px; background: Theme.line; }

    for item[i] in root.items: VerticalLayout {
        ta := TouchArea {
            mouse-cursor: pointer;
            clicked => { root.open(item.url); }

            HorizontalLayout {
                padding-top: 16px;
                padding-bottom: 16px;
                spacing: 16px;

                Text {
                    text: i + 1;
                    font-family: Theme.mono;
                    font-size: 13px;
                    color: Theme.accent-text;
                    vertical-alignment: top;
                }
                VerticalLayout {
                    spacing: 8px;
                    Text {
                        text: item.title;
                        font-family: Theme.sans;
                        font-size: 18px;
                        color: ta.has-hover ? Theme.accent-text : Theme.text;
                        wrap: word-wrap;
                        overflow: elide;
                        // 3 lines max at ~1.3 line height
                        max-height: 3 * 18px * 1.3;
                    }
                    Text {
                        text: item.age == "" ? item.source : item.source + " · " + item.age;
                        font-family: Theme.mono;
                        font-size: 12px;
                        letter-spacing: 1.4px;
                        color: Theme.muted;
                    }
                }
            }
        }
        if i < root.items.length - 1: Rectangle { height: 1px; background: Theme.line; }
    }

    Rectangle { vertical-stretch: 1; }   // push footer down

    Text {
        text: root.offline ? "OFFLINE · " + root.footer : root.footer;
        font-family: Theme.mono;
        font-size: 12px;
        letter-spacing: 1.4px;
        color: Theme.muted;
    }
}
```

> **Verify against the current Slint docs:** multi-line `overflow: elide` together with `wrap: word-wrap` needs a recent Slint (1.6+). If it doesn't elide on your version, cap the title length in Rust instead (≈ 110 characters + "…").

### 5.2 Wire it into the dashboard

In `ui/dashboard.slint`, replace the Briefing column with:

```slint
import { NewsColumn, NewsItem } from "widgets/news-column.slint";

// … inside the four-column row, in place of 04 Briefing:
NewsColumn {
    items: root.news-items;
    footer: root.news-footer;
    offline: root.news-offline;
    open(url) => { root.open-news(url); }
}
```

And expose the properties up to `AppWindow` in `ui/app.slint`:

```slint
export { NewsItem } from "widgets/news-column.slint";

// in AppWindow:
in property <[NewsItem]> news-items;
in property <string> news-footer;
in property <bool> news-offline;
callback open-news(string);
```

Pass them down into `Dashboard { … }` the same way as the clock properties.

---

## 6. Rust glue (`crates/app/src/news_task.rs`)

Fetches on a background Tokio task, pushes results to the UI thread with `slint::invoke_from_event_loop`, and refreshes the "2H AGO" labels every minute without refetching.

```rust
use chrono::{DateTime, Local, Utc};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{AppWindow, NewsItem};

const REFRESH: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Default)]
pub struct NewsState {
    pub stories: Vec<news::Story>,
    pub fetched: Option<DateTime<Utc>>,
    pub offline: bool,
}

pub type Shared = Arc<Mutex<NewsState>>;

/// Push the current state into the UI. Must run on the Slint event loop.
pub fn render(ui: &AppWindow, state: &NewsState) {
    let now = Utc::now();
    let items: Vec<NewsItem> = state
        .stories
        .iter()
        .map(|s| NewsItem {
            title: SharedString::from(s.title.as_str()),
            source: SharedString::from(s.source.to_uppercase()),
            age: SharedString::from(news::age_label(s.published, now)),
            url: SharedString::from(s.url.as_str()),
        })
        .collect();
    ui.set_news_items(ModelRc::new(VecModel::from(items)));

    let footer = match state.fetched {
        Some(t) => format!(
            "UPDATED {} · GOOGLE NEWS",
            t.with_timezone(&Local).format("%-I:%M %p")
        ),
        None => "LOADING · GOOGLE NEWS".into(),
    };
    ui.set_news_footer(footer.into());
    ui.set_news_offline(state.offline);
}

/// Spawn the fetch loop. Call once from main() after the UI exists.
pub fn spawn(rt: &tokio::runtime::Handle, ui: Weak<AppWindow>, state: Shared) {
    rt.spawn(async move {
        let client = news::client();
        let mut tick = tokio::time::interval(REFRESH);
        loop {
            tick.tick().await; // first tick fires immediately
            let result = news::fetch_top(&client, 3).await;

            let snapshot = {
                let mut s = state.lock().unwrap();
                match result {
                    Ok(stories) => {
                        s.stories = stories;
                        s.fetched = Some(Utc::now());
                        s.offline = false;
                        // TODO: save to SQLite (section 4) via your db handle
                    }
                    Err(e) => {
                        tracing::warn!("news fetch failed: {e}");
                        s.offline = true; // keep showing cached stories
                    }
                }
                s.clone()
            };

            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui.upgrade() {
                    render(&ui, &snapshot);
                }
            });
        }
    });
}
```

### 6.1 In `main.rs`

```rust
mod news_task;

// after `let ui = AppWindow::new()?;` and after the Tokio runtime is created:
let news_state: news_task::Shared = Default::default();

// 1. Load the SQLite cache first so the column is filled immediately
//    (convert core::news_cache::CachedStory → news::Story, set fetched + offline=true
//    until the first live fetch succeeds), then:
news_task::render(&ui, &news_state.lock().unwrap());

// 2. Start fetching
news_task::spawn(rt.handle(), ui.as_weak(), news_state.clone());

// 3. Recompute "2H AGO" labels every minute (no network)
let age_timer = slint::Timer::default();
{
    let ui = ui.as_weak();
    let st = news_state.clone();
    age_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(60), move || {
        if let Some(ui) = ui.upgrade() {
            news_task::render(&ui, &st.lock().unwrap());
        }
    });
}

// 4. Open links in the default browser
ui.on_open_news(|url| {
    if let Err(e) = open::that_detached(url.as_str()) {
        tracing::warn!("couldn't open {url}: {e}");
    }
});
```

> **Keep `age_timer` alive:** a `slint::Timer` stops when it's dropped, so declare it in `main()` before `ui.run()`, not inside a block that ends early.

> **Resume from sleep:** when the logind `PrepareForSleep(false)` handler exists (Phase 6 in `DEVELOPMENT.md`), trigger an immediate fetch, for example with a `tokio::sync::Notify` that the loop `select!`s on alongside `tick.tick()`.

---

## 7. Remove the Briefing

Search the codebase and docs for anything left over:

```bash
grep -rn -i "briefing\|brief me\|gmail" crates/ ui/ DEVELOPMENT.md
```

| Remove | Where |
|---|---|
| Briefing column component | `ui/dashboard.slint`, any `briefing.slint` |
| PLAY BRIEFING on the dismissed-alarm screen | `ui/takeover.slint` → keep "Good morning" + dashboard link |
| "brief me" rule | Command parser in `crates/core` (and its tests) |
| Briefing summary task | AI routing table in `crates/ai` / Settings → AI & Models |
| Gmail scope and client | `crates/google` (don't request `gmail.readonly` or similar) |
| Gmail row | Settings → Accounts access checklist |
| `DEVELOPMENT.md` | Phase checklists that mention the briefing or Gmail; point column 04 at this file |

---

## 8. Checklist

- [ ] `curl` the feed and confirm you get stories (section 2)
- [ ] `crates/news` created; `cargo test -p news` passes (section 3)
- [ ] `news_cache` table and `save` / `load` in `core` (section 4)
- [ ] `NewsColumn` widget renders three stories in dark and light (section 5)
- [ ] Fetch loop, minute age refresh and click-to-open wired up (section 6)
- [ ] Cache loads on start; unplug network → footer shows `OFFLINE · UPDATED …` and stories stay (sections 4, 6)
- [ ] Briefing, "brief me" and Gmail removed (section 7)
- [ ] Canvas updated with the News column and the new dismissed-alarm screen (open item in `HANDOFF.md`)

**Done when:** the dashboard's 04 column shows the current top 3 Google News stories with publisher and age, clicking one opens it in the browser, it refreshes every 15 minutes, and it still shows the last stories with the network off.
