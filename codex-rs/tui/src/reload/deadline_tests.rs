use super::*;
use pretty_assertions::assert_eq;

#[test]
fn stalled_and_rejected_workers_share_the_original_deadline() {
    let start = Instant::now();
    let mut stalled = Deadline::new(start);
    assert_eq!(stalled.poll(start + CANDIDATE), Transition::Rollback);
    assert_eq!(
        stalled.poll(start + CANDIDATE),
        Transition::Wait(TOTAL - CANDIDATE)
    );
    assert_eq!(stalled.poll(start + TOTAL), Transition::Failed);

    let mut rejected = Deadline::new(start);
    assert_eq!(rejected.reject(start), Transition::Rollback);
    assert_eq!(rejected.poll(start), Transition::Wait(TOTAL));
    assert_eq!(rejected.poll(start + TOTAL), Transition::Failed);
    assert_eq!(rejected.reject(start + TOTAL), Transition::Failed);

    // A delayed supervisor must not launch fallback after the entire budget expired.
    assert_eq!(Deadline::new(start).poll(start + TOTAL), Transition::Failed);
}
