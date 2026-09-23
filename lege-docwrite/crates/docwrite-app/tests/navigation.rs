//! Exact PageUp/PageDown and snap-at-gesture-end.

use docwrite_app::{Pager, Phase};

#[test]
fn page_down_and_page_up_land_on_page_tops_from_any_scroll() {
    let mut pager = Pager::new(400, false);
    pager.scroll_gesture(2.4, Phase::Moved);
    // Moved without Started treats delta as an absolute gesture from 0 when
    // origin is missing. Set a mid-page position by a started gesture.
    let mut pager = Pager::new(400, false);
    pager.scroll_gesture(0.0, Phase::Started);
    pager.scroll_gesture(2.4, Phase::Moved);
    assert!((pager.scroll() - 2.4).abs() < 1e-9);

    pager.page_down();
    assert_eq!(pager.scroll(), 3.0, "PageDown from 2.4 is the next page top");
    pager.page_up();
    assert_eq!(pager.scroll(), 2.0, "PageUp from a page top is the previous top");

    pager.scroll_gesture(0.0, Phase::Started);
    pager.scroll_gesture(0.2, Phase::Moved);
    assert!((pager.scroll() - 2.2).abs() < 1e-9);
    pager.page_up();
    assert_eq!(pager.scroll(), 2.0, "PageUp from mid-page lands on this page top");
}

#[test]
fn spread_mode_moves_by_spread_and_a_gesture_snaps() {
    let mut pager = Pager::new(20, true);
    pager.page_down();
    assert_eq!(pager.scroll(), 2.0);
    pager.page_down();
    assert_eq!(pager.scroll(), 4.0);
    pager.page_up();
    assert_eq!(pager.scroll(), 2.0);

    pager.scroll_gesture(0.0, Phase::Started);
    pager.scroll_gesture(1.2, Phase::Moved);
    assert!((pager.scroll() - 3.2).abs() < 1e-9, "the gesture stays continuous");
    pager.scroll_gesture(1.2, Phase::Ended);
    assert_eq!(pager.scroll(), 4.0, "3.2 snaps to the nearer spread");

    let mut single = Pager::new(20, false);
    single.scroll_gesture(0.0, Phase::Started);
    single.scroll_gesture(2.4, Phase::Ended);
    assert_eq!(single.scroll(), 2.0, "2.4 snaps to page 2");
    single.scroll_gesture(0.0, Phase::Started);
    single.scroll_gesture(0.6, Phase::Ended);
    assert_eq!(single.scroll(), 3.0, "2.6 snaps to page 3");
}
