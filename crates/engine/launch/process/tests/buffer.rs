use coblet_runtime::{GameLogLine, LogLevel, LogSource};

use super::{LogBuffer, LogItem};

fn line(text: &str) -> GameLogLine {
    GameLogLine::plain(LogSource::Stdout, LogLevel::Info, text)
}

fn text(item: Option<LogItem>) -> String {
    match item {
        Some(LogItem::Line(line)) => line.message,
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn late_subscribers_get_history_then_live_lines() {
    let buffer = LogBuffer::new(2);
    buffer.push(line("a"));
    buffer.push(line("b"));
    buffer.push(line("c"));
    let mut stream = buffer.subscribe();
    buffer.push(line("d"));
    buffer.close();
    assert_eq!(text(stream.next().await), "b");
    assert_eq!(text(stream.next().await), "c");
    assert_eq!(text(stream.next().await), "d");
    assert_eq!(stream.next().await, None);
    assert_eq!(buffer.snapshot().len(), 2);
    let mut after = buffer.subscribe();
    assert_eq!(text(after.next().await), "c");
    assert_eq!(text(after.next().await), "d");
    assert_eq!(after.next().await, None);
}
