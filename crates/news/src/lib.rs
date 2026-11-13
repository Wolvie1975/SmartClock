use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
use rss::Channel;
use std::{io::Cursor, time::Duration};

const FEED_URL: &str = "https://news.google.com/rss";
const USER_AGENT: &str = concat!("smart-clock/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub title: String,
    pub source: String,
    pub url: String,
    pub published_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum NewsError {
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("RSS feed could not be parsed: {0}")]
    Rss(#[from] rss::Error),
    #[error("the RSS feed contained no usable stories")]
    NoStories,
}

pub fn client() -> Result<Client, NewsError> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?)
}

pub async fn fetch_top(client: &Client, limit: usize) -> Result<Vec<Story>, NewsError> {
    let response = client.get(FEED_URL).send().await?.error_for_status()?;
    parse_top(&response.bytes().await?, limit)
}

pub fn parse_top(xml: &[u8], limit: usize) -> Result<Vec<Story>, NewsError> {
    let channel = Channel::read_from(Cursor::new(xml))?;
    let stories = channel
        .items()
        .iter()
        .filter_map(|item| {
            let raw_title = item.title()?.trim();
            let url = item.link()?.trim();
            if raw_title.is_empty() || !is_safe_story_url(url) {
                return None;
            }

            let (title, source) = raw_title
                .rsplit_once(" - ")
                .filter(|(title, source)| !title.trim().is_empty() && !source.trim().is_empty())
                .map(|(title, source)| (title.trim(), source.trim()))
                .unwrap_or((raw_title, "GOOGLE NEWS"));

            Some(Story {
                title: title.to_owned(),
                source: source.to_owned(),
                url: url.to_owned(),
                published_utc: item
                    .pub_date()
                    .and_then(|date| DateTime::parse_from_rfc2822(date).ok())
                    .map(|date| date.with_timezone(&Utc)),
            })
        })
        .take(limit)
        .collect::<Vec<_>>();

    if stories.is_empty() {
        return Err(NewsError::NoStories);
    }

    Ok(stories)
}

pub fn is_safe_story_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub fn age_label(published: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(published) = published else {
        return "DATE UNKNOWN".into();
    };
    let elapsed = (now - published).to_std().unwrap_or_default();
    let minutes = elapsed.as_secs() / 60;
    if minutes < 1 {
        "JUST NOW".into()
    } else if minutes < 60 {
        format!("{minutes}M AGO")
    } else if minutes < 24 * 60 {
        format!("{}H AGO", minutes / 60)
    } else {
        format!("{}D AGO", minutes / (24 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const FEED: &str = r#"
        <rss version="2.0"><channel><title>News</title>
        <item><title>First headline - Example News</title>
            <link>https://news.google.com/articles/first</link>
            <pubDate>Fri, 13 Nov 2026 12:00:00 GMT</pubDate>
        </item>
        <item><title>Second headline - Second News</title>
            <link>https://publisher.example/story</link>
        </item>
        <item><title>Unsafe item - Example</title><link>javascript:alert(1)</link></item>
        </channel></rss>
    "#;

    #[test]
    fn parses_stories_and_skips_unsafe_links() {
        let stories = parse_top(FEED.as_bytes(), 3).unwrap();
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].title, "First headline");
        assert_eq!(stories[0].source, "Example News");
        assert_eq!(stories[0].url, "https://news.google.com/articles/first");
        assert_eq!(
            stories[0].published_utc,
            Some(Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap())
        );
        assert_eq!(stories[1].source, "Second News");
    }

    #[test]
    fn limits_story_count_and_rejects_empty_results() {
        assert_eq!(parse_top(FEED.as_bytes(), 1).unwrap().len(), 1);
        assert!(matches!(
            parse_top(FEED.as_bytes(), 0),
            Err(NewsError::NoStories)
        ));
        assert!(matches!(
            parse_top(b"<rss><channel/></rss>", 3),
            Err(NewsError::NoStories)
        ));
    }

    #[test]
    fn accepts_only_https_urls_without_credentials() {
        assert!(is_safe_story_url("https://news.example/story"));
        assert!(!is_safe_story_url("http://news.example/story"));
        assert!(!is_safe_story_url("javascript:alert(1)"));
        assert!(!is_safe_story_url(
            "https://user:password@news.example/story"
        ));
    }

    #[test]
    fn formats_story_age() {
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        assert_eq!(age_label(None, now), "DATE UNKNOWN");
        assert_eq!(age_label(Some(now), now), "JUST NOW");
        assert_eq!(
            age_label(Some(now - chrono::Duration::minutes(14)), now),
            "14M AGO"
        );
        assert_eq!(
            age_label(Some(now - chrono::Duration::hours(3)), now),
            "3H AGO"
        );
        assert_eq!(
            age_label(Some(now - chrono::Duration::days(2)), now),
            "2D AGO"
        );
    }
}
