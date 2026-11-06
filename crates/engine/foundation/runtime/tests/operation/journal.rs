use std::collections::HashSet;

use super::parse;
use crate::StepId;

#[test]
fn forgets_steps_before_a_rollback() {
    assert_eq!(
        parse("done 0\ndone 2\n"),
        HashSet::from([StepId(0), StepId(2)])
    );
    assert_eq!(
        parse("done 0\nrolled-back\ndone 1\ngarbage\ndone x\n"),
        HashSet::from([StepId(1)])
    );
}
