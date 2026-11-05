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
        if let Some(line) = bracketed(&raw, self.source) {
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
mod tests {
    use std::sync::Arc;

    use coblet_runtime::{LogLevel, LogSource};

    use super::LogParser;

    fn millis(time: std::time::SystemTime) -> u128 {
        time.duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    fn parser(source: LogSource) -> LogParser {
        LogParser::new(
            source,
            Arc::new(|text: &str| text.replace("s3cret", "<redacted>")),
        )
    }

    #[test]
    fn parses_multi_line_log4j_events() {
        let mut parser = parser(LogSource::Stdout);
        assert_eq!(parser
                .push(r#"<log4j:Event logger="net.minecraft.client.Minecraft" timestamp="1700000000123" level="WARN" thread="Render thread">"#).len(), 0);
        assert_eq!(
            parser
                .push("  <log4j:Message><![CDATA[Token s3cret <ok> & done]]></log4j:Message>")
                .len(),
            0
        );
        let lines = parser.push("</log4j:Event>");
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.level, LogLevel::Warn);
        assert_eq!(line.thread.as_deref(), Some("Render thread"));
        assert_eq!(
            line.logger.as_deref(),
            Some("net.minecraft.client.Minecraft")
        );
        assert_eq!(line.message, "Token <redacted> <ok> & done");
        assert_eq!(millis(line.time.unwrap()), 1_700_000_000_123);
        assert!(!line.raw.contains("s3cret"));
    }

    #[test]
    fn appends_throwables_and_unescapes_attributes() {
        let mut parser = parser(LogSource::Stdout);
        let text = concat!(
            r#"<log4j:Event logger="a&amp;b" timestamp="1" level="ERROR" thread="main">"#,
            "<log4j:Message><![CDATA[boom]]></log4j:Message>",
            "<log4j:Throwable><![CDATA[java.lang.RuntimeException: x\n\tat A.b(A.java:1)\n]]></log4j:Throwable>",
            "</log4j:Event>"
        );
        let line = parser.push(text).remove(0);
        assert_eq!(line.logger.as_deref(), Some("a&b"));
        assert_eq!(line.level, LogLevel::Error);
        assert_eq!(
            line.message,
            "boom\njava.lang.RuntimeException: x\n\tat A.b(A.java:1)"
        );
    }

    #[test]
    fn parses_bracketed_and_plain_lines() {
        let mut parser = parser(LogSource::Stdout);
        let line = parser
            .push("[12:34:56] [Render thread/INFO]: Setting user: Steve")
            .remove(0);
        assert_eq!(line.level, LogLevel::Info);
        assert_eq!(line.thread.as_deref(), Some("Render thread"));
        assert_eq!(line.message, "Setting user: Steve");
        let forge = parser
            .push("[12:34:56] [main/DEBUG] [cpw.mods.Loader/]: hi")
            .remove(0);
        assert_eq!(forge.logger.as_deref(), Some("cpw.mods.Loader/"));
        assert_eq!(forge.message, "hi");
        let plain = parser.push("27 achievements").remove(0);
        assert_eq!(plain.level, LogLevel::Info);
        assert_eq!(plain.thread, None);

        let mut stderr = super::LogParser::new(LogSource::Stderr, Arc::new(str::to_owned));
        assert_eq!(
            stderr.push("\tat Foo.bar(Foo.java:3)")[0].level,
            LogLevel::Error
        );
        assert_eq!(stderr.push("LWJGL Version: 2.9.4")[0].level, LogLevel::Warn);
        assert_eq!(stderr.push("INFO: Loading sounds")[0].level, LogLevel::Info);
        assert_eq!(stderr.push("WARNING: no font")[0].level, LogLevel::Warn);
        assert_eq!(stderr.push("SEVERE: gone")[0].level, LogLevel::Error);
    }

    #[test]
    fn flushes_unfinished_events() {
        let mut parser = parser(LogSource::Stdout);
        assert_eq!(parser.push(r#"<log4j:Event level="INFO">"#).len(), 0);
        assert_eq!(parser.push("half").len(), 0);
        let lines = parser.finish();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].message, "half");
    }
}
