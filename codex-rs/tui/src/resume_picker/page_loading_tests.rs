//! Checks that a cursor keeps its original checkout filter until the listing restarts.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn cwd_filter_remains_fixed_until_next_listing_cycle() {
    let primary = Path::new("/repo");
    let next = Path::new("/other");
    let single = Some(ThreadListCwdFilter::One("/repo".into()));
    let mut filter = PageCwdFilter::default();
    assert_eq!(filter.for_request(None, Some(primary)), single);
    let cursor = PageCursor::AppServer("next-page".into());
    assert_eq!(filter.for_request(Some(&cursor), Some(next)), single);
    assert_eq!(
        filter.for_request(None, Some(next)),
        Some(ThreadListCwdFilter::One("/other".into()))
    );
    assert_eq!(filter.for_request(None, None), None);
}
