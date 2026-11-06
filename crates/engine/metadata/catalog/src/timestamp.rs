use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Point in time with second precision, as seconds since the Unix epoch (UTC).
///
/// Parsed from the RFC 3339 timestamps in version metadata, such as
/// `2011-07-07T22:00:00+00:00`. Catalogs sort and filter by it instead of
/// comparing version strings.
///
/// # Examples
///
/// ```
/// use coblet_catalog::Timestamp;
///
/// let beta: Timestamp = "2011-07-07T22:00:00+00:00".parse()?;
/// let release: Timestamp = "2017-09-18T08:39:46Z".parse()?;
/// assert!(beta < release);
/// assert_eq!(beta.to_string(), "2011-07-07T22:00:00Z");
/// # Ok::<(), coblet_catalog::TimestampError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(i64);

/// Reason a timestamp was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an RFC 3339 timestamp")]
pub struct TimestampError(String);

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
///
/// Algorithm from <https://howardhinnant.github.io/date_algorithms.html#days_from_civil>.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl Timestamp {
    /// Wraps seconds since the Unix epoch.
    #[must_use]
    pub const fn from_unix(seconds: i64) -> Self {
        Self(seconds)
    }

    /// Returns seconds since the Unix epoch.
    #[must_use]
    pub const fn unix(self) -> i64 {
        self.0
    }

    /// Parses `YYYY-MM-DDTHH:MM:SS` with an optional fraction and a `Z` or `±HH:MM` offset.
    ///
    /// # Errors
    ///
    /// Returns [`TimestampError`] for anything else.
    pub fn parse(text: &str) -> Result<Self, TimestampError> {
        let error = || TimestampError(text.to_owned());
        let bytes = text.as_bytes();
        if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
            return Err(error());
        }
        if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[16] != b':' {
            return Err(error());
        }
        let number = |range: std::ops::Range<usize>| -> Result<i64, TimestampError> {
            text.get(range)
                .filter(|part| part.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|part| part.parse().ok())
                .ok_or_else(error)
        };
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return Err(error());
        }
        if second > 60 {
            return Err(error());
        }
        let mut rest = &text[19..];
        if let Some(fraction) = rest.strip_prefix('.') {
            let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(error());
            }
            rest = &fraction[digits..];
        }
        let offset = match rest {
            "Z" | "z" => 0,
            _ => {
                let sign = match rest.as_bytes().first() {
                    Some(b'+') => 1,
                    Some(b'-') => -1,
                    _ => return Err(error()),
                };
                let (hours, minutes) = rest[1..].split_once(':').ok_or_else(error)?;
                let parse = |part: &str| -> Result<i64, TimestampError> {
                    (part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| part.parse().ok())
                        .flatten()
                        .ok_or_else(error)
                };
                sign * (parse(hours)? * 3600 + parse(minutes)? * 60)
            }
        };
        let days = days_from_civil(year, month, day);
        Ok(Self(
            days * 86_400 + hour * 3600 + minute * 60 + second - offset,
        ))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (days, seconds) = (self.0.div_euclid(86_400), self.0.rem_euclid(86_400));
        let (year, month, day) = civil_from_days(days);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        )
    }
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Timestamp {
    type Error = TimestampError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
#[path = "../tests/timestamp.rs"]
mod tests;
