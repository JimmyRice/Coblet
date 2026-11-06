use std::fmt;
use std::time::SystemTime;

/// Severity of a game log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    /// `TRACE`.
    Trace,
    /// `DEBUG`.
    Debug,
    /// `INFO`.
    Info,
    /// `WARN`.
    Warn,
    /// `ERROR`.
    Error,
    /// `FATAL`.
    Fatal,
}

impl LogLevel {
    /// Parses a log4j level name, ignoring case; `WARNING` and `SEVERE` are accepted too.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "TRACE" | "FINEST" | "FINER" => Some(Self::Trace),
            "DEBUG" | "FINE" => Some(Self::Debug),
            "INFO" | "CONFIG" => Some(Self::Info),
            "WARN" | "WARNING" => Some(Self::Warn),
            "ERROR" | "SEVERE" => Some(Self::Error),
            "FATAL" => Some(Self::Fatal),
            _ => None,
        }
    }

    /// Returns the upper-case name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a game log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogSource {
    /// The game's standard output.
    Stdout,
    /// The game's standard error.
    Stderr,
    /// The launcher itself, such as the command line or the exit status.
    Launcher,
}

/// One line or log4j event of a running game, with secrets already redacted.
///
/// Structured fields are filled when the line is a log4j event or follows the
/// usual `[time] [thread/LEVEL]: message` layout; otherwise only `message`
/// and `raw` are set and `level` is guessed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameLogLine {
    /// Event time from a log4j event.
    pub time: Option<SystemTime>,
    /// Severity, parsed or guessed.
    pub level: LogLevel,
    /// Thread name, such as `Render thread`.
    pub thread: Option<String>,
    /// Logger name, such as `net.minecraft.client.Minecraft`.
    pub logger: Option<String>,
    /// The message.
    pub message: String,
    /// Stream the line came from.
    pub source: LogSource,
    /// The text as received, redacted.
    pub raw: String,
}

impl GameLogLine {
    /// Creates a line with only a message; `raw` is the message.
    #[must_use]
    pub fn plain(source: LogSource, level: LogLevel, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            time: None,
            level,
            thread: None,
            logger: None,
            raw: message.clone(),
            message,
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GameLogLine, LogLevel, LogSource};

    #[test]
    fn parses_levels() {
        assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("SEVERE"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("loud"), None);
        assert!(LogLevel::Error > LogLevel::Info);
        let line = GameLogLine::plain(LogSource::Launcher, LogLevel::Info, "started");
        assert_eq!(line.raw, "started");
    }
}
