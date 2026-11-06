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
    assert_eq!(
        stderr.push("java.io.FileNotFoundException: http://example/")[0].level,
        LogLevel::Error
    );
    let dated = stderr
        .push("2013-09-19 12:34:56 [CLIENT] [INFO] Setting user: Steve")
        .remove(0);
    assert_eq!(dated.level, LogLevel::Info);
    assert_eq!(dated.thread.as_deref(), Some("CLIENT"));
    assert_eq!(dated.message, "Setting user: Steve");
    assert_eq!(
        stderr.push("2013-09-19 [x] [INFO] y")[0].level,
        LogLevel::Warn
    );
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
