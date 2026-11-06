use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use super::{OutputStream, ProcessError, ProcessExit, ProcessSpec, SpawnedProcess};
use crate::BoxFuture;

struct Lines(Vec<&'static str>);

impl OutputStream for Lines {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, ProcessError>> {
        let next = (!self.0.is_empty()).then(|| Bytes::from_static(self.0.remove(0).as_bytes()));
        Box::pin(async move { Ok(next) })
    }
}

fn scripted(out: Vec<&'static str>) -> SpawnedProcess {
    SpawnedProcess::new(
        Some(7),
        Some(Box::new(Lines(out))),
        Some(Box::new(Lines(vec!["warn\n"]))),
        Box::pin(async {
            Ok(ProcessExit {
                code: Some(0),
                signal: None,
            })
        }),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn captures_both_streams() {
    let output = scripted(vec!["a", "b"]).capture(16).await.unwrap();
    assert!(output.exit.success());
    assert_eq!(&output.stdout[..], b"ab");
    assert_eq!(&output.stderr[..], b"warn\n");
}

#[tokio::test]
async fn stops_at_the_output_limit() {
    let error = scripted(vec!["aaaa", "bbbb"]).capture(6).await.unwrap_err();
    assert!(matches!(error, ProcessError::OutputTooLarge { limit: 6 }));
}

#[test]
fn builds_specs() {
    let spec = ProcessSpec::new("java")
        .arg("-version")
        .args(["-Xmx1G"])
        .env("A", "1")
        .cwd("/tmp");
    assert_eq!(spec.args.len(), 2);
    assert_eq!(spec.env.len(), 1);
    assert_eq!(
        ProcessExit {
            code: None,
            signal: Some(9)
        }
        .to_string(),
        "signal 9"
    );
}
