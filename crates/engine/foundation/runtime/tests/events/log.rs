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
