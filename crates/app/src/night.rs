use chrono::NaiveTime;
use clock_core::Store;

pub const DEFAULT_START: &str = "10:00 PM";
pub const DEFAULT_END: &str = "6:00 AM";
pub const DEFAULT_DIM_LEVEL: f32 = 0.65;

pub struct Settings {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub dim_level: f32,
}

impl Settings {
    pub fn load(store: &Store) -> anyhow::Result<Self> {
        let enabled = match store.get_setting("night_enabled")? {
            Some(value) => match value.parse::<bool>() {
                Ok(enabled) => enabled,
                Err(_) => {
                    tracing::warn!("invalid saved night_enabled value; using enabled default");
                    true
                }
            },
            None => true,
        };
        let start = load_time_setting(store, "night_start", DEFAULT_START)?;
        let end = load_time_setting(store, "night_end", DEFAULT_END)?;
        let dim_level = match store
            .get_setting("night_dim_level")?
            .and_then(|value| value.parse::<f32>().ok())
        {
            Some(level) if level.is_finite() && (0.0..=0.9).contains(&level) => level,
            Some(_) => {
                tracing::warn!("invalid saved night_dim_level value; using default");
                DEFAULT_DIM_LEVEL
            }
            None => DEFAULT_DIM_LEVEL,
        };
        Ok(Self {
            enabled,
            start,
            end,
            dim_level,
        })
    }
}

fn load_time_setting(store: &Store, key: &str, default: &str) -> anyhow::Result<String> {
    match store.get_setting(key)? {
        Some(value) if parse_12h(&value).is_some() => Ok(value),
        Some(_) => {
            tracing::warn!(setting = key, "invalid saved night time; using default");
            Ok(default.to_owned())
        }
        None => Ok(default.to_owned()),
    }
}

pub fn parse_12h(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%I:%M %p")
        .or_else(|_| NaiveTime::parse_from_str(value.trim(), "%l:%M %p"))
        .ok()
}

pub fn is_night(enabled: bool, start: &str, end: &str, now: NaiveTime) -> bool {
    if !enabled {
        return false;
    }
    let (Some(start), Some(end)) = (parse_12h(start), parse_12h(end)) else {
        return false;
    };
    if start <= end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    #[test]
    fn night_window_crosses_midnight_and_excludes_end() {
        assert!(is_night(true, "10:00 PM", "6:00 AM", time(23, 30)));
        assert!(is_night(true, "10:00 PM", "6:00 AM", time(2, 0)));
        assert!(!is_night(true, "10:00 PM", "6:00 AM", time(6, 0)));
        assert!(!is_night(true, "10:00 PM", "6:00 AM", time(12, 0)));
    }

    #[test]
    fn same_day_window_and_disabled_schedule() {
        assert!(is_night(true, "1:00 AM", "5:00 AM", time(3, 0)));
        assert!(!is_night(true, "1:00 AM", "5:00 AM", time(23, 0)));
        assert!(!is_night(false, "10:00 PM", "6:00 AM", time(23, 0)));
    }

    #[test]
    fn invalid_time_does_not_dim() {
        assert!(!is_night(true, "25:99", "6:00 AM", time(23, 0)));
        assert_eq!(parse_12h("12:30 AM"), Some(time(0, 30)));
    }
}
