//! Google Workspace OAuth and REST APIs used by Smart Clock.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};
use url::Url;

pub const CALENDAR_READ_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const TASKS_SCOPE: &str = "https://www.googleapis.com/auth/tasks";
pub const GMAIL_METADATA_SCOPE: &str = "https://www.googleapis.com/auth/gmail.metadata";

const API_ROOT: &str = "https://www.googleapis.com";
const USER_AGENT: &str = concat!("smart-clock/", env!("CARGO_PKG_VERSION"));

fn api_url(path: &[&str]) -> Url {
    let mut url = Url::parse(API_ROOT).expect("constant Google API URL is valid");
    url.path_segments_mut()
        .expect("Google API URL supports path segments")
        .clear()
        .extend(path);
    url
}

pub mod oauth;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub start: EventStart,
    pub birthday: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventStart {
    Timed(DateTime<Utc>),
    AllDay(NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskList {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub updated: Option<DateTime<Utc>>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub hidden: bool,
}

impl Task {
    pub fn due_date(&self) -> Result<Option<NaiveDate>> {
        self.due
            .as_deref()
            .map(|date| {
                NaiveDate::parse_from_str(
                    date.get(..10).context("invalid task due date")?,
                    "%Y-%m-%d",
                )
                .context("invalid task due date")
            })
            .transpose()
    }

    pub fn done(&self) -> bool {
        self.status == "completed"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailUnread {
    pub count: usize,
    pub history_id: String,
    pub used_inbox_fallback: bool,
}

/// Authenticated Google Workspace REST client.
#[derive(Clone)]
pub struct GoogleApi {
    http: Client,
    access_token: String,
}

impl GoogleApi {
    pub fn new(access_token: &str) -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .context("building Google API HTTP client")?;
        Ok(Self {
            http,
            access_token: access_token.to_owned(),
        })
    }

    pub async fn calendar_today(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            items: Vec<RawEvent>,
            #[serde(default)]
            next_page_token: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawEvent {
            id: String,
            #[serde(default)]
            summary: String,
            #[serde(default)]
            event_type: String,
            #[serde(default)]
            status: String,
            start: Option<RawEventTime>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawEventTime {
            #[serde(default)]
            date_time: Option<DateTime<Utc>>,
            #[serde(default)]
            date: Option<NaiveDate>,
        }

        let mut events = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .http
                .get(api_url(&[
                    "calendar",
                    "v3",
                    "calendars",
                    "primary",
                    "events",
                ]))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("timeMin", start.to_rfc3339()),
                    ("timeMax", end.to_rfc3339()),
                    ("singleEvents", "true".into()),
                    ("orderBy", "startTime".into()),
                    ("maxResults", "2500".into()),
                    ("eventTypes", "default".into()),
                    ("eventTypes", "birthday".into()),
                ]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let page: Page = self.json(request).await?;
            events.extend(page.items.into_iter().filter_map(|event| {
                if event.status == "cancelled" {
                    return None;
                }
                let start = event.start?;
                let start = match (start.date_time, start.date) {
                    (Some(at), _) => EventStart::Timed(at),
                    (_, Some(day)) => EventStart::AllDay(day),
                    _ => return None,
                };
                Some(CalendarEvent {
                    id: event.id,
                    title: event.summary,
                    start,
                    birthday: event.event_type == "birthday",
                })
            }));
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(events)
    }

    pub async fn task_lists(&self) -> Result<Vec<TaskList>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            items: Vec<TaskList>,
            #[serde(default)]
            next_page_token: Option<String>,
        }
        let mut lists = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .http
                .get(api_url(&["tasks", "v1", "users", "@me", "lists"]))
                .bearer_auth(&self.access_token)
                .query(&[("maxResults", "100".to_string())]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let page: Page = self.json(request).await?;
            lists.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(lists)
    }

    pub async fn tasks(&self, list_id: &str) -> Result<Vec<Task>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            items: Vec<Task>,
            #[serde(default)]
            next_page_token: Option<String>,
        }
        let mut tasks = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .http
                .get(api_url(&["tasks", "v1", "lists", list_id, "tasks"]))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("maxResults", "100".to_string()),
                    ("showCompleted", "true".to_string()),
                    ("showDeleted", "true".to_string()),
                    ("showHidden", "true".to_string()),
                ]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let page: Page = self.json(request).await?;
            tasks.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(tasks)
    }

    pub async fn create_task(&self, list_id: &str, task: &TaskWrite) -> Result<Task> {
        self.json(
            self.http
                .post(api_url(&["tasks", "v1", "lists", list_id, "tasks"]))
                .bearer_auth(&self.access_token)
                .json(task),
        )
        .await
    }

    pub async fn update_task(
        &self,
        list_id: &str,
        task_id: &str,
        etag: Option<&str>,
        task: &TaskWrite,
    ) -> Result<Task> {
        let mut request = self
            .http
            .patch(api_url(&[
                "tasks", "v1", "lists", list_id, "tasks", task_id,
            ]))
            .bearer_auth(&self.access_token)
            .json(task);
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.json(request).await
    }

    pub async fn delete_task(&self, list_id: &str, task_id: &str) -> Result<()> {
        let response = self
            .http
            .delete(api_url(&[
                "tasks", "v1", "lists", list_id, "tasks", task_id,
            ]))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("requesting Google API")?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            bail!("Google API returned HTTP {}", response.status())
        }
    }

    pub async fn unread_mail(&self, previous_history_id: Option<&str>) -> Result<MailUnread> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Profile {
            history_id: String,
        }
        let profile: Profile = self
            .json(
                self.http
                    .get(api_url(&["gmail", "v1", "users", "me", "profile"]))
                    .bearer_auth(&self.access_token),
            )
            .await?;
        let Some(history_id) = previous_history_id else {
            let count = self.inbox_unread_count().await?;
            return Ok(MailUnread {
                count,
                history_id: profile.history_id,
                used_inbox_fallback: true,
            });
        };

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct HistoryPage {
            #[serde(default)]
            history: Vec<HistoryItem>,
            #[serde(default)]
            next_page_token: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct HistoryItem {
            #[serde(default)]
            labels_added: Vec<LabelAdded>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LabelAdded {
            message: Option<MessageId>,
            #[serde(default)]
            label_ids: Vec<String>,
        }
        #[derive(Deserialize)]
        struct MessageId {
            id: String,
        }

        let mut messages = HashSet::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .http
                .get(api_url(&["gmail", "v1", "users", "me", "history"]))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("startHistoryId", history_id.to_string()),
                    ("historyTypes", "labelAdded".to_string()),
                    ("maxResults", "500".to_string()),
                ]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let response = request.send().await.context("requesting Gmail history")?;
            if response.status() == StatusCode::NOT_FOUND {
                let count = self.inbox_unread_count().await?;
                return Ok(MailUnread {
                    count,
                    history_id: profile.history_id,
                    used_inbox_fallback: true,
                });
            }
            let page: HistoryPage = Self::decode(response).await?;
            for added in page.history.into_iter().flat_map(|h| h.labels_added) {
                if added.label_ids.iter().any(|label| label == "UNREAD") {
                    if let Some(message) = added.message {
                        messages.insert(message.id);
                    }
                }
            }
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(MailUnread {
            count: messages.len(),
            history_id: profile.history_id,
            used_inbox_fallback: false,
        })
    }

    async fn inbox_unread_count(&self) -> Result<usize> {
        #[derive(Deserialize)]
        struct Label {
            #[serde(default)]
            messages_unread: usize,
        }
        let label: Label = self
            .json(
                self.http
                    .get(api_url(&["gmail", "v1", "users", "me", "labels", "INBOX"]))
                    .bearer_auth(&self.access_token),
            )
            .await?;
        Ok(label.messages_unread)
    }

    async fn json<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        let response = request.send().await.context("requesting Google API")?;
        Self::decode(response).await
    }

    async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        if !response.status().is_success() {
            bail!("Google API returned HTTP {}", response.status())
        }
        response
            .json()
            .await
            .context("decoding Google API response")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskWrite {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl TaskWrite {
    pub fn from_local(title: impl Into<String>, due: Option<NaiveDate>, done: bool) -> Self {
        Self {
            title: title.into(),
            due: Some(due.map(|day| format!("{day}T00:00:00.000Z"))),
            status: Some(if done { "completed" } else { "needsAction" }.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_date_discards_google_midnight_time() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "abc",
            "title": "Call the bank",
            "status": "needsAction",
            "due": "2026-11-14T00:00:00.000Z"
        }))
        .unwrap();
        assert_eq!(
            task.due_date().unwrap(),
            Some(NaiveDate::from_ymd_opt(2026, 11, 14).unwrap())
        );
        assert!(!task.done());
    }

    #[test]
    fn timed_google_due_date_is_written_as_date_only_midnight() {
        let task = TaskWrite::from_local(
            "Renew registration",
            Some(NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()),
            false,
        );
        assert_eq!(
            task.due.as_ref().and_then(Option::as_deref),
            Some("2026-11-14T00:00:00.000Z")
        );
    }

    #[test]
    fn task_without_due_date_clears_the_remote_date() {
        let task = TaskWrite::from_local("Call the bank", None, false);
        assert_eq!(
            serde_json::to_value(task).unwrap(),
            serde_json::json!({
                "title": "Call the bank",
                "due": null,
                "status": "needsAction"
            })
        );
    }
}
