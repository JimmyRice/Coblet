use super::callback_channel;
use crate::AuthError;

#[tokio::test]
async fn first_submission_wins() {
    let (sender, receiver) = callback_channel();
    let clone = sender.clone();
    assert!(sender.is_open());
    sender.submit("app://cb?code=1").unwrap();
    assert!(!clone.is_open());
    assert!(matches!(
        clone.submit("app://cb?code=2"),
        Err(AuthError::CallbackClosed)
    ));
    assert_eq!(receiver.recv().await.unwrap(), "app://cb?code=1");
}

#[tokio::test]
async fn reports_a_closed_login() {
    let (sender, receiver) = callback_channel();
    drop(receiver);
    assert!(!sender.is_open());
    assert!(matches!(sender.submit("x"), Err(AuthError::CallbackClosed)));
    let (sender, receiver) = callback_channel();
    drop(sender);
    assert!(matches!(
        receiver.recv().await,
        Err(AuthError::CallbackClosed)
    ));
}
