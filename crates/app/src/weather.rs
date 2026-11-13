//! Weather panel from Open-Meteo (no API key).
//!
//! Network calls run on short-lived background threads and report back over a
//! channel that the UI tick polls, so nothing blocks the event loop and no
//! non-`Send` state crosses threads. The last forecast is cached in settings so
//! the panel has something to show at startup or offline.
//!
//! Settings: `weather_place`, `weather_lat`, `weather_lon`, `weather_cache`,
//! `weather_cache_utc`, `temp_unit` (fahrenheit), `wind_unit` (mph).

use crate::{AppWindow, DayCell, HourCell, Weather};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use clock_core::Store;
use serde::Deserialize;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(15 * 60);
const RETRY: Duration = Duration::from_secs(2 * 60);
const USER_AGENT: &str = concat!("smart-clock/", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// Open-Meteo responses

#[derive(Debug, Deserialize)]
pub struct Forecast {
    pub current: Current,
    pub hourly: Hourly,
    pub daily: Daily,
}

#[derive(Debug, Deserialize)]
pub struct Current {
    pub time: String,
    pub temperature_2m: Option<f64>,
    pub apparent_temperature: Option<f64>,
    pub weather_code: Option<i32>,
    pub wind_speed_10m: Option<f64>,
    pub relative_humidity_2m: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Hourly {
    pub time: Vec<String>,
    pub temperature_2m: Vec<Option<f64>>,
    pub precipitation_probability: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
pub struct Daily {
    pub time: Vec<String>,
    pub weather_code: Vec<Option<i32>>,
    pub temperature_2m_max: Vec<Option<f64>>,
    pub temperature_2m_min: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
struct GeoResults {
    #[serde(default)]
    results: Vec<Place>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub admin1: Option<String>,
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

impl Place {
    pub fn label(&self) -> String {
        [
            Some(self.name.clone()),
            self.admin1.clone(),
            self.country_code.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (tested)

/// WMO weather code → short uppercase label.
pub fn condition(code: Option<i32>) -> &'static str {
    match code {
        Some(0) => "CLEAR",
        Some(1) => "MOSTLY CLEAR",
        Some(2) => "PARTLY CLOUDY",
        Some(3) => "OVERCAST",
        Some(45 | 48) => "FOG",
        Some(51 | 53 | 55) => "DRIZZLE",
        Some(56 | 57) => "FREEZING DRIZZLE",
        Some(61 | 63 | 65) => "RAIN",
        Some(66 | 67) => "FREEZING RAIN",
        Some(71 | 73 | 75 | 77) => "SNOW",
        Some(80..=82) => "SHOWERS",
        Some(85 | 86) => "SNOW SHOWERS",
        Some(95) => "THUNDERSTORM",
        Some(96 | 99) => "T-STORM · HAIL",
        _ => "—",
    }
}

fn deg(v: Option<f64>) -> String {
    v.map(|t| format!("{}°", t.round() as i64))
        .unwrap_or_else(|| "—".into())
}

const US_STATES: &[(&str, &str)] = &[
    ("al", "alabama"),
    ("ak", "alaska"),
    ("az", "arizona"),
    ("ar", "arkansas"),
    ("ca", "california"),
    ("co", "colorado"),
    ("ct", "connecticut"),
    ("de", "delaware"),
    ("fl", "florida"),
    ("ga", "georgia"),
    ("hi", "hawaii"),
    ("id", "idaho"),
    ("il", "illinois"),
    ("in", "indiana"),
    ("ia", "iowa"),
    ("ks", "kansas"),
    ("ky", "kentucky"),
    ("la", "louisiana"),
    ("me", "maine"),
    ("md", "maryland"),
    ("ma", "massachusetts"),
    ("mi", "michigan"),
    ("mn", "minnesota"),
    ("ms", "mississippi"),
    ("mo", "missouri"),
    ("mt", "montana"),
    ("ne", "nebraska"),
    ("nv", "nevada"),
    ("nh", "new hampshire"),
    ("nj", "new jersey"),
    ("nm", "new mexico"),
    ("ny", "new york"),
    ("nc", "north carolina"),
    ("nd", "north dakota"),
    ("oh", "ohio"),
    ("ok", "oklahoma"),
    ("or", "oregon"),
    ("pa", "pennsylvania"),
    ("ri", "rhode island"),
    ("sc", "south carolina"),
    ("sd", "south dakota"),
    ("tn", "tennessee"),
    ("tx", "texas"),
    ("ut", "utah"),
    ("vt", "vermont"),
    ("va", "virginia"),
    ("wa", "washington"),
    ("wv", "west virginia"),
    ("wi", "wisconsin"),
    ("wy", "wyoming"),
    ("dc", "district of columbia"),
];

/// "Austin, TX" → ("Austin", Some("tx")).
pub fn split_query(q: &str) -> (String, Option<String>) {
    match q.split_once(',') {
        Some((name, rest)) => {
            let rest = rest.trim().to_lowercase();
            (name.trim().to_string(), (!rest.is_empty()).then_some(rest))
        }
        None => (q.trim().to_string(), None),
    }
}

/// Pick the result that matches the qualifier (state, state abbreviation,
/// country code or country name); otherwise the first, most populous result.
pub fn pick_place(results: &[Place], qualifier: Option<&str>) -> Option<Place> {
    let Some(q) = qualifier else {
        return results.first().cloned();
    };
    let state = US_STATES
        .iter()
        .find(|(abbr, _)| *abbr == q)
        .map(|(_, name)| *name);
    let matches = |p: &&Place| {
        let admin = p.admin1.as_deref().unwrap_or("").to_lowercase();
        let cc = p.country_code.as_deref().unwrap_or("").to_lowercase();
        let country = p.country.as_deref().unwrap_or("").to_lowercase();
        admin == q
            || state.is_some_and(|s| admin == s)
            || cc == q
            || country == q
            || (q.len() > 3 && (admin.starts_with(q) || country.starts_with(q)))
    };
    results.iter().find(matches).or(results.first()).cloned()
}

pub struct View {
    pub temp: String,
    pub condition: String,
    pub details: String,
    pub hilo: String,
    pub hours: Vec<HourCell>,
    pub days: Vec<DayCell>,
}

fn hour_label(t: &NaiveDateTime) -> String {
    t.format("%-I %p").to_string()
}

/// Build the panel from a forecast. `now` is the location's local time (Open-Meteo
/// returns location-local times with `timezone=auto`).
pub fn build_view(f: &Forecast, wind_unit: &str) -> View {
    let parse_hour = |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok();
    let now = parse_hour(&f.current.time);
    let c = &f.current;

    // Next 8 hours after the current one.
    let start = f
        .hourly
        .time
        .iter()
        .position(|t| parse_hour(t).zip(now).is_some_and(|(t, n)| t > n))
        .unwrap_or(0);
    let hours = (start..(start + 8).min(f.hourly.time.len()))
        .map(|i| HourCell {
            hour: parse_hour(&f.hourly.time[i])
                .map(|t| hour_label(&t))
                .unwrap_or_default()
                .into(),
            temp: deg(f.hourly.temperature_2m.get(i).copied().flatten()).into(),
            precip: f
                .hourly
                .precipitation_probability
                .get(i)
                .copied()
                .flatten()
                .filter(|p| *p >= 10.0)
                .map(|p| format!("{}%", p.round() as i64))
                .unwrap_or_default()
                .into(),
        })
        .collect();

    // Five days with range bars scaled to the week's min/max.
    let n = f.daily.time.len().min(5);
    let lows: Vec<Option<f64>> = (0..n)
        .map(|i| f.daily.temperature_2m_min.get(i).copied().flatten())
        .collect();
    let highs: Vec<Option<f64>> = (0..n)
        .map(|i| f.daily.temperature_2m_max.get(i).copied().flatten())
        .collect();
    let lo = lows.iter().flatten().copied().fold(f64::INFINITY, f64::min);
    let hi = highs
        .iter()
        .flatten()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1.0);
    let days = (0..n)
        .map(|i| {
            let date = NaiveDate::parse_from_str(&f.daily.time[i], "%Y-%m-%d").ok();
            let day = match (i, date) {
                (0, _) => "TODAY".to_string(),
                (_, Some(d)) => d.format("%a").to_string().to_uppercase(),
                _ => String::new(),
            };
            let (l, h) = (lows[i], highs[i]);
            DayCell {
                day: day.into(),
                condition: condition(f.daily.weather_code.get(i).copied().flatten()).into(),
                lo: deg(l).into(),
                hi: deg(h).into(),
                start: l.map(|l| ((l - lo) / span) as f32).unwrap_or(0.0),
                end: h.map(|h| ((h - lo) / span) as f32).unwrap_or(0.0),
            }
        })
        .collect();

    let mut details = vec![format!("FEELS {}", deg(c.apparent_temperature))];
    if let Some(w) = c.wind_speed_10m {
        details.push(format!(
            "WIND {} {}",
            w.round() as i64,
            wind_unit.to_uppercase()
        ));
    }
    if let Some(h) = c.relative_humidity_2m {
        details.push(format!("HUM {}%", h.round() as i64));
    }

    View {
        temp: deg(c.temperature_2m),
        condition: condition(c.weather_code).into(),
        details: details.join(" · "),
        hilo: format!(
            "H {}  L {}",
            deg(highs.first().copied().flatten()),
            deg(lows.first().copied().flatten())
        ),
        hours,
        days,
    }
}

// ---------------------------------------------------------------------------
// Network

fn client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
}

fn fetch_forecast(lat: f64, lon: f64, temp_unit: &str, wind_unit: &str) -> anyhow::Result<String> {
    let url = "https://api.open-meteo.com/v1/forecast";
    let body = client()?
        .get(url)
        .query(&[
            ("latitude", lat.to_string()),
            ("longitude", lon.to_string()),
            (
                "current",
                "temperature_2m,apparent_temperature,weather_code,wind_speed_10m,relative_humidity_2m"
                    .into(),
            ),
            ("hourly", "temperature_2m,precipitation_probability".into()),
            ("daily", "weather_code,temperature_2m_max,temperature_2m_min".into()),
            ("temperature_unit", temp_unit.into()),
            ("wind_speed_unit", wind_unit.into()),
            ("timezone", "auto".into()),
            ("forecast_days", "5".into()),
        ])
        .send()?
        .error_for_status()?
        .text()?;
    serde_json::from_str::<Forecast>(&body)?; // validate before caching
    Ok(body)
}

fn geocode(query: &str) -> anyhow::Result<Place> {
    let (name, qualifier) = split_query(query);
    anyhow::ensure!(!name.is_empty(), "empty location");
    let res: GeoResults = client()?
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[
            ("name", name.as_str()),
            ("count", "10"),
            ("language", "en"),
            ("format", "json"),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    pick_place(&res.results, qualifier.as_deref())
        .ok_or_else(|| anyhow::anyhow!("no place called “{name}”"))
}

// ---------------------------------------------------------------------------
// Controller

enum Msg {
    Forecast(anyhow::Result<String>),
    Located(anyhow::Result<Place>),
}

pub struct WeatherController {
    store: Rc<Store>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    in_flight: Cell<bool>,
    next_fetch: Cell<Instant>,
    hours: Rc<VecModel<HourCell>>,
    days: Rc<VecModel<DayCell>>,
    /// Message for the command bar after a location lookup.
    pub located: RefCell<Option<String>>,
}

impl WeatherController {
    pub fn install(ui: &AppWindow, store: Rc<Store>) -> Rc<Self> {
        let (tx, rx) = mpsc::channel();
        let this = Rc::new(Self {
            store,
            tx,
            rx,
            in_flight: Cell::new(false),
            next_fetch: Cell::new(Instant::now()),
            hours: Rc::new(VecModel::default()),
            days: Rc::new(VecModel::default()),
            located: RefCell::new(None),
        });
        let w = ui.global::<Weather>();
        w.set_hours(ModelRc::from(this.hours.clone()));
        w.set_days(ModelRc::from(this.days.clone()));
        this.show_cached(ui);
        this
    }

    fn units(&self) -> (String, String) {
        let t = self
            .store
            .get_setting("temp_unit")
            .ok()
            .flatten()
            .unwrap_or_else(|| "fahrenheit".into());
        let w = self
            .store
            .get_setting("wind_unit")
            .ok()
            .flatten()
            .unwrap_or_else(|| "mph".into());
        (t, w)
    }

    fn coords(&self) -> Option<(f64, f64)> {
        let lat = self
            .store
            .get_setting("weather_lat")
            .ok()
            .flatten()?
            .parse()
            .ok()?;
        let lon = self
            .store
            .get_setting("weather_lon")
            .ok()
            .flatten()?
            .parse()
            .ok()?;
        Some((lat, lon))
    }

    /// Look up a city and make it the weather location (background).
    pub fn set_location(&self, query: &str) {
        let tx = self.tx.clone();
        let q = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Located(geocode(&q)));
        });
    }

    fn fetch(&self) {
        let Some((lat, lon)) = self.coords() else {
            return;
        };
        if self.in_flight.replace(true) {
            return;
        }
        let (t, w) = self.units();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Forecast(fetch_forecast(lat, lon, &t, &w)));
        });
    }

    fn show_cached(&self, ui: &AppWindow) {
        let w = ui.global::<Weather>();
        let place = self.store.get_setting("weather_place").ok().flatten();
        w.set_place(place.clone().unwrap_or_default().to_uppercase().into());
        if place.is_none() {
            w.set_has_data(false);
            w.set_message("SET A LOCATION · TYPE “LOCATION AUSTIN, TX”".into());
            return;
        }
        let cached = self.store.get_setting("weather_cache").ok().flatten();
        let parsed = cached
            .as_deref()
            .and_then(|c| serde_json::from_str::<Forecast>(c).ok());
        let Some(f) = parsed else {
            w.set_has_data(false);
            w.set_message("LOADING…".into());
            return;
        };
        let (_, wind) = self.units();
        let v = build_view(&f, &wind);
        w.set_temp(v.temp.into());
        w.set_condition(v.condition.into());
        w.set_details(v.details.into());
        w.set_hilo(v.hilo.into());
        self.hours.set_vec(v.hours);
        self.days.set_vec(v.days);
        w.set_has_data(true);
        let updated = self
            .store
            .get_setting("weather_cache_utc")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .map(|t| t.with_timezone(&Local).format("%-I:%M %p").to_string())
            .unwrap_or_default();
        w.set_updated(format!("UPDATED {updated}").into());
    }

    /// Called from the UI tick.
    pub fn tick(&self, ui: &AppWindow) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Forecast(res) => {
                    self.in_flight.set(false);
                    match res {
                        Ok(body) => {
                            let _ = self.store.set_setting("weather_cache", &body);
                            let _ = self
                                .store
                                .set_setting("weather_cache_utc", &Utc::now().to_rfc3339());
                            self.next_fetch.set(Instant::now() + REFRESH);
                            self.show_cached(ui);
                        }
                        Err(e) => {
                            tracing::warn!("weather fetch failed: {e:#}");
                            self.next_fetch.set(Instant::now() + RETRY);
                            let w = ui.global::<Weather>();
                            if w.get_has_data() {
                                w.set_updated(format!("{} · OFFLINE", w.get_updated()).into());
                            } else {
                                w.set_message("WEATHER UNAVAILABLE · RETRYING".into());
                            }
                        }
                    }
                }
                Msg::Located(res) => {
                    let note = match res {
                        Ok(p) => {
                            let _ = self.store.set_setting("weather_place", &p.name);
                            let _ = self
                                .store
                                .set_setting("weather_lat", &p.latitude.to_string());
                            let _ = self
                                .store
                                .set_setting("weather_lon", &p.longitude.to_string());
                            let _ = self.store.set_setting("weather_cache", "");
                            self.show_cached(ui);
                            self.next_fetch.set(Instant::now());
                            format!("LOCATION · {}", p.label().to_uppercase())
                        }
                        Err(e) => format!("LOCATION NOT FOUND · {e}").to_uppercase(),
                    };
                    *self.located.borrow_mut() = Some(note);
                }
            }
        }
        if Instant::now() >= self.next_fetch.get() && !self.in_flight.get() {
            self.next_fetch.set(Instant::now() + RETRY); // until a result arrives
            self.fetch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(name: &str, admin1: &str, cc: &str) -> Place {
        Place {
            name: name.into(),
            latitude: 0.0,
            longitude: 0.0,
            admin1: Some(admin1.into()),
            country_code: Some(cc.into()),
            country: None,
        }
    }

    #[test]
    fn query_split_and_pick() {
        assert_eq!(
            split_query("Austin, TX"),
            ("Austin".into(), Some("tx".into()))
        );
        assert_eq!(split_query("Paris"), ("Paris".into(), None));

        let results = [
            place("Paris", "Île-de-France", "FR"),
            place("Paris", "Texas", "US"),
        ];
        assert_eq!(
            pick_place(&results, None).unwrap().country_code.as_deref(),
            Some("FR")
        );
        assert_eq!(
            pick_place(&results, Some("tx")).unwrap().admin1.as_deref(),
            Some("Texas")
        );
        assert_eq!(
            pick_place(&results, Some("texas"))
                .unwrap()
                .admin1
                .as_deref(),
            Some("Texas")
        );
        assert_eq!(
            pick_place(&results, Some("us")).unwrap().admin1.as_deref(),
            Some("Texas")
        );
        assert_eq!(
            pick_place(&results, Some("zz"))
                .unwrap()
                .country_code
                .as_deref(),
            Some("FR")
        );
        assert!(pick_place(&[], None).is_none());
    }

    #[test]
    fn view_from_forecast() {
        let json = r#"{
          "current": {"time": "2026-11-13T14:15", "temperature_2m": 71.6, "apparent_temperature": 69.8,
                      "weather_code": 2, "wind_speed_10m": 8.2, "relative_humidity_2m": 40},
          "hourly": {"time": ["2026-11-13T13:00","2026-11-13T14:00","2026-11-13T15:00","2026-11-13T16:00"],
                     "temperature_2m": [70.0, 71.0, 72.4, null],
                     "weather_code": [2, 2, 3, 61],
                     "precipitation_probability": [0, 5, 20, null]},
          "daily": {"time": ["2026-11-13","2026-11-14"],
                    "weather_code": [2, 61],
                    "temperature_2m_max": [78.0, 70.0],
                    "temperature_2m_min": [61.0, 58.0],
                    "precipitation_probability_max": [10, 80]}
        }"#;
        let f: Forecast = serde_json::from_str(json).unwrap();
        let v = build_view(&f, "mph");
        assert_eq!(v.temp, "72°");
        assert_eq!(v.condition, "PARTLY CLOUDY");
        assert_eq!(v.details, "FEELS 70° · WIND 8 MPH · HUM 40%");
        assert_eq!(v.hilo, "H 78°  L 61°");
        // Starts after the current hour (14:15 → 3 PM).
        assert_eq!(v.hours.len(), 2);
        assert_eq!(v.hours[0].hour, "3 PM");
        assert_eq!(v.hours[0].temp, "72°");
        assert_eq!(v.hours[0].precip, "20%");
        assert_eq!(v.hours[1].temp, "—");
        assert_eq!(v.days[0].day, "TODAY");
        assert_eq!(v.days[1].day, "SAT");
        assert_eq!(v.days[1].condition, "RAIN");
        // Week range 58–78: today 61–78, Saturday 58–70.
        assert!((v.days[0].start - 0.15).abs() < 1e-6 && (v.days[0].end - 1.0).abs() < 1e-6);
        assert!(v.days[1].start.abs() < 1e-6 && (v.days[1].end - 0.6).abs() < 1e-6);
    }
}
