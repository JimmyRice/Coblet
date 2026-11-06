use std::fmt;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use coblet_runtime::{GameLogLine, LogLevel, LogSource};

/// Largest log4j event kept together; longer events are passed on line by line.
const MAX_EVENT_BYTES: usize = 1 << 20;

/// Function that removes secrets from text.
pub type Redact = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Turns raw output lines of one stream into [`GameLogLine`]s.
///
/// log4j XML events (what the version's log configuration produces) can
/// span several lines and come out as one structured line; other lines are
/// parsed as `[time] [thread/LEVEL]: message` when they look like it, and
/// otherwise keep only a guessed level. Every line is redacted.
pub struct LogParser {
    source: LogSource,
    redact: Redact,
    pending: Option<String>,
}

impl fmt::Debug for LogParser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogParser")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl LogParser {
    /// Creates a parser for one stream.
    #[must_use]
    pub fn new(source: LogSource, redact: Redact) -> Self {
        Self {
            source,
            redact,
            pending: None,
        }
    }

    /// Feeds one line without its line break; returns the lines it completes.
    pub fn push(&mut self, line: &str) -> Vec<GameLogLine> {
        let mut out = Vec::new();
        if let Some(pending) = &mut self.pending {
            pending.push('\n');
            pending.push_str(line);
            if line.contains("</log4j:Event>") {
                let event = self.pending.take().unwrap_or_default();
                out.push(self.event(&event));
            } else if pending.len() > MAX_EVENT_BYTES {
                out.extend(self.finish());
            }
            return out;
        }
        if line.trim_start().starts_with("<log4j:Event") {
            if line.contains("</log4j:Event>") {
                out.push(self.event(line));
            } else {
                self.pending = Some(line.to_owned());
            }
            return out;
        }
        out.push(self.plain(line));
        out
    }

    /// Returns what is left of an unfinished event, line by line.
    pub fn finish(&mut self) -> Vec<GameLogLine> {
        self.pending
            .take()
            .map(|pending| pending.lines().map(|line| self.plain(line)).collect())
            .unwrap_or_default()
    }

    fn event(&self, text: &str) -> GameLogLine {
        let raw = (self.redact)(text);
        let head = raw.split('>').next().unwrap_or_default();
        let level = attribute(head, "level")
            .and_then(|level| LogLevel::parse(&level))
            .unwrap_or(LogLevel::Info);
        let time = attribute(head, "timestamp")
            .and_then(|millis| millis.parse::<u64>().ok())
            .map(|millis| UNIX_EPOCH + Duration::from_millis(millis));
        let mut message = element(&raw, "log4j:Message").unwrap_or_default();
        if let Some(throwable) = element(&raw, "log4j:Throwable") {
            message.push('\n');
            message.push_str(throwable.trim_end());
        }
        let mut line = GameLogLine::plain(self.source, level, message);
        line.time = time;
        line.thread = attribute(head, "thread");
        line.logger = attribute(head, "logger");
        line.raw = raw;
        line
    }

    fn plain(&self, text: &str) -> GameLogLine {
        let raw = (self.redact)(text);
        if let Some(line) = bracketed(&raw, self.source).or_else(|| dated(&raw, self.source)) {
            return line;
        }
        GameLogLine::plain(self.source, guess_level(&raw, self.source), raw)
    }
}

/// Parses `[12:34:56] [Render thread/INFO]: message`, optionally with `[logger]` before the colon.
fn bracketed(raw: &str, source: LogSource) -> Option<GameLogLine> {
    let rest = raw.strip_prefix('[')?;
    let (_, rest) = rest.split_once(']')?;
    let rest = rest.trim_start().strip_prefix('[')?;
    let (inner, mut rest) = rest.split_once(']')?;
    let (thread, level) = inner.rsplit_once('/')?;
    let level = LogLevel::parse(level)?;
    let mut logger = None;
    if let Some(after) = rest.trim_start().strip_prefix('[')
        && let Some((name, after)) = after.split_once(']')
    {
        logger = Some(name.to_owned());
        rest = after;
    }
    let message = rest.trim_start().strip_prefix(':')?.trim_start();
    let mut line = GameLogLine::plain(source, level, message);
    line.thread = Some(thread.to_owned());
    line.logger = logger;
    raw.clone_into(&mut line.raw);
    Some(line)
}

/// Parses `2013-09-19 12:34:56 [CLIENT] [INFO] message`, the format of 1.6.
fn dated(raw: &str, source: LogSource) -> Option<GameLogLine> {
    let (date, rest) = raw.split_once(' ')?;
    let (time, rest) = rest.split_once(' ')?;
    let digits = |text: &str, sep: char| {
        !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || c == sep)
    };
    if date.len() != 10 || time.len() != 8 || !digits(date, '-') || !digits(time, ':') {
        return None;
    }
    let (side, rest) = rest.strip_prefix('[')?.split_once(']')?;
    let (level, message) = rest.trim_start().strip_prefix('[')?.split_once(']')?;
    let mut line = GameLogLine::plain(source, LogLevel::parse(level)?, message.trim_start());
    line.thread = Some(side.to_owned());
    raw.clone_into(&mut line.raw);
    Some(line)
}

/// Reports whether `text` starts with a qualified exception class, such as
/// `java.io.FileNotFoundException: x`.
fn exception_head(text: &str) -> bool {
    let word = text.split_whitespace().next().unwrap_or_default();
    let class = word.strip_suffix(':').unwrap_or(word);
    class.contains('.') && (class.ends_with("Exception") || class.ends_with("Error"))
}

fn guess_level(text: &str, source: LogSource) -> LogLevel {
    let trimmed = text.trim_start();
    // java.util.logging prints the level as a prefix, often on stderr.
    for (prefix, level) in [
        ("SEVERE:", LogLevel::Error),
        ("WARNING:", LogLevel::Warn),
        ("INFO:", LogLevel::Info),
        ("CONFIG:", LogLevel::Debug),
        ("FINE:", LogLevel::Debug),
    ] {
        if trimmed.starts_with(prefix) {
            return level;
        }
    }
    let exception = trimmed.starts_with("at ")
        || trimmed.starts_with("Caused by:")
        || text.contains("Exception in thread")
        || trimmed.starts_with("java.lang.")
        || exception_head(trimmed)
        || trimmed.starts_with("Exception");
    if exception || text.contains("ERROR") || text.contains("SEVERE") {
        LogLevel::Error
    } else if text.contains("WARN") || source == LogSource::Stderr {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}

fn attribute(head: &str, name: &str) -> Option<String> {
    let start = head.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = head[start..].find('"')? + start;
    Some(unescape(&head[start..end]))
}

fn element(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))? + start;
    let body = &text[start..end];
    Some(match body.trim().strip_prefix("<![CDATA[") {
        Some(cdata) => cdata
            .strip_suffix("]]>")
            .unwrap_or(cdata)
            .replace("]]>]]&gt;<![CDATA[", "]]>"),
        None => unescape(body),
    })
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
#[path = "../tests/log.rs"]
mod tests;
