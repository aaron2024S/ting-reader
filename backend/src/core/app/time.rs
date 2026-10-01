//! Time-zone preference helpers.
//!
//! The backend always generates and stores timestamps in UTC. This module only
//! validates a display preference and, for the first FPK installation, reads a
//! host OS IANA time-zone name directly. It does not change the process clock,
//! SQLite functions, or log timestamp generation.

use chrono_tz::Tz;

pub const DEFAULT_TIME_ZONE: &str = "UTC";
pub const FPK_INITIAL_TIME_ZONE_ENV: &str = "FNNAS_FPK_INITIAL_TIME_ZONE";

pub fn parse_time_zone(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Time zone cannot be empty".to_string());
    }

    value
        .parse::<Tz>()
        .map(|time_zone| time_zone.name().to_string())
        .map_err(|_| format!("Unsupported IANA time zone: {value}"))
}

/// Read the host OS time zone only when the FPK launcher explicitly enables it.
/// Docker FPK exposes `/etc/localtime` and `/etc/timezone` read-only so the
/// `iana-time-zone` crate can resolve the host's IANA name without fnOS APIs.
pub fn initial_time_zone_from_os() -> Option<String> {
    if !std::env::var(FPK_INITIAL_TIME_ZONE_ENV)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
    {
        return None;
    }

    let candidates = std::iter::once(std::env::var("TZ").ok())
        .chain(std::iter::once(iana_time_zone::get_timezone().ok()))
        .flatten();

    candidates
        .filter_map(|candidate| parse_time_zone(&candidate).ok())
        .next()
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_TIME_ZONE, parse_time_zone};

    #[test]
    fn accepts_iana_time_zones() {
        assert_eq!(parse_time_zone("Asia/Shanghai").unwrap(), "Asia/Shanghai");
    }

    #[test]
    fn rejects_unknown_time_zones() {
        assert!(parse_time_zone("Mars/Olympus").is_err());
    }

    #[test]
    fn default_is_utc() {
        assert_eq!(DEFAULT_TIME_ZONE, "UTC");
    }
}
