use super::{CancellationToken, Cancelled, ensure_not_cancelled};

#[test]
fn cancelling_a_parent_cancels_children() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    assert_eq!(ensure_not_cancelled(&child), Ok(()));
    parent.cancel();
    assert_eq!(ensure_not_cancelled(&child), Err(Cancelled));
}
