use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use clock_core::{model::CachedNewsStory, Store};
use reqwest::Client;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::runtime::{Builder, Runtime};

use crate::{AppWindow, NewsItem};

const STORY_LIMIT: usize = 3;
const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
const AGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

enum Event {
    FetchFinished(std::result::Result<Vec<news::Story>, String>),
}

struct NewsState {
    stories: Vec<CachedNewsStory>,
    fetched_utc: Option<DateTime<Utc>>,
    offline: bool,
    cache_error: bool,
}

pub struct NewsController {
    store: Rc<Store>,
    client: Client,
    runtime: Runtime,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    state: RefCell<NewsState>,
    fetching: RefCell<bool>,
    next_refresh: RefCell<Instant>,
    next_age_refresh: RefCell<Instant>,
}

impl NewsController {
    pub fn install(ui: &AppWindow, store: Rc<Store>) -> Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("news")
            .enable_all()
            .build()
            .context("building News runtime")?;
        let client = news::client().context("building News HTTP client")?;
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        let (stories, cache_error) = match store.news_cache() {
            Ok(stories) => (stories, false),
            Err(error) => {
                tracing::error!("loading cached news failed: {error}");
                (Vec::new(), true)
            }
        };
        let fetched_utc = stories.first().map(|story| story.fetched_utc);
        let offline = !stories.is_empty();
        let controller = Self {
            store,
            client,
            runtime,
            tx,
            rx,
            state: RefCell::new(NewsState {
                stories,
                fetched_utc,
                offline,
                cache_error,
            }),
            fetching: RefCell::new(false),
            next_refresh: RefCell::new(now),
            next_age_refresh: RefCell::new(now + AGE_REFRESH_INTERVAL),
        };
        controller.render(ui, Utc::now());

        let weak = ui.as_weak();
        ui.on_open_news(move |url| {
            if !news::is_safe_story_url(url.as_str()) {
                tracing::warn!("blocked an unsafe news story URL");
                return;
            }
            if let Err(error) = open::that_detached(url.as_str()) {
                tracing::warn!("opening a news story failed: {error}");
                if let Some(ui) = weak.upgrade() {
                    ui.set_news_footer("COULD NOT OPEN STORY · GOOGLE NEWS".into());
                }
            }
        });

        Ok(controller)
    }

    pub fn tick(&self, ui: &AppWindow) {
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            *self.fetching.borrow_mut() = false;
            let mut state = self.state.borrow_mut();
            match event {
                Event::FetchFinished(Ok(stories)) => {
                    let fetched_utc = Utc::now();
                    let cached = stories
                        .into_iter()
                        .map(|story| CachedNewsStory {
                            title: story.title,
                            source: story.source,
                            url: story.url,
                            published_utc: story.published_utc,
                            fetched_utc,
                        })
                        .collect::<Vec<_>>();
                    state.cache_error = if let Err(error) = self.store.save_news_cache(&cached) {
                        tracing::error!("saving cached news failed: {error}");
                        true
                    } else {
                        false
                    };
                    state.stories = cached;
                    state.fetched_utc = Some(fetched_utc);
                    state.offline = false;
                }
                Event::FetchFinished(Err(error)) => {
                    tracing::warn!("Google News fetch failed: {error}");
                    state.offline = true;
                }
            }
            changed = true;
        }

        let now = Instant::now();
        if now >= *self.next_age_refresh.borrow() {
            *self.next_age_refresh.borrow_mut() = now + AGE_REFRESH_INTERVAL;
            changed = true;
        }
        if changed {
            self.render(ui, Utc::now());
        }
        if !*self.fetching.borrow() && now >= *self.next_refresh.borrow() {
            *self.next_refresh.borrow_mut() = now + REFRESH_INTERVAL;
            *self.fetching.borrow_mut() = true;
            self.start_fetch();
        }
    }

    fn start_fetch(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let result = news::fetch_top(&client, STORY_LIMIT)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(Event::FetchFinished(result));
        });
    }

    fn render(&self, ui: &AppWindow, now: DateTime<Utc>) {
        let state = self.state.borrow();
        let items = state
            .stories
            .iter()
            .map(|story| NewsItem {
                title: story.title.clone().into(),
                source: story.source.clone().into(),
                age: news::age_label(story.published_utc, now).into(),
                url: story.url.clone().into(),
            })
            .collect::<Vec<_>>();
        ui.set_news_items(ModelRc::from(Rc::new(VecModel::from(items))));
        ui.set_news_offline(state.offline);

        let prefix = if state.cache_error {
            "CACHE ERROR · "
        } else if state.offline {
            "OFFLINE · "
        } else {
            ""
        };
        let status = match state.fetched_utc {
            Some(fetched) => format!(
                "UPDATED {} · GOOGLE NEWS",
                fetched.with_timezone(&Local).format("%-I:%M %p")
            ),
            None if state.offline => "NO STORIES CACHED · GOOGLE NEWS".into(),
            None => "LOADING · GOOGLE NEWS".into(),
        };
        ui.set_news_footer(format!("{prefix}{status}").into());
    }
}
