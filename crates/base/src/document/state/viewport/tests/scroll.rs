use super::{document, draw};
use crate::{
    ScrollbarHandle,
    document::{Affinity, DocumentEvent, DocumentPosition, DocumentState, Scroll, ScrollDirection},
};
use gpui::{
    Context, Entity, Modifiers, ScrollDelta, ScrollWheelEvent, Subscription, TestAppContext,
    TouchPhase, VisualTestContext, point, px,
};
use std::{cell::Cell, rc::Rc};

type Doc = Entity<DocumentState<&'static str>>;
type Position = DocumentPosition<&'static str>;

fn prepared(cx: &mut TestAppContext) -> (Doc, VisualTestContext, Position, Position) {
    let (document, mut cx) = document(1, cx);
    let offset = (0..40).map(|ix| format!("line {ix}\n").len()).sum();
    let target = Position::new("history", offset, Affinity::After);
    let draft = Position::new("draft", 5, Affinity::After);
    document.update(&mut cx, |document, cx| {
        document
            .pin_scroll_forward_above("run", &target, &draft, px(0.), 0.5..=0.5, cx)
            .unwrap();
    });
    draw(&mut cx);
    document.update(&mut cx, |document, cx| {
        document.unpin_scroll(&"run", cx);
    });
    draw(&mut cx);
    (document, cx, target, draft)
}

fn observe(document: &Doc, cx: &mut VisualTestContext) -> (Rc<Cell<Option<Scroll>>>, Subscription) {
    let last = Rc::new(Cell::new(None));
    let observed = last.clone();
    let subscription = cx.update(|_, cx| {
        cx.subscribe(document, move |_, event, _| {
            if let DocumentEvent::ViewportScrolled(input) = event {
                observed.set(Some(*input));
            }
        })
    });
    (last, subscription)
}

fn wheel(dy: f32, cx: &mut VisualTestContext) {
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(300.), px(150.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        ..Default::default()
    });
    // simulate_event runs the notified frame. Another draw would expire its result.
}

fn rejoin(
    document: &mut DocumentState<&'static str>,
    input: &Scroll,
    target: &Position,
    draft: &Position,
    cx: &mut Context<DocumentState<&'static str>>,
) -> bool {
    document
        .rejoin_scroll_forward_above(input, "run", target, draft, px(0.), 0.5..=0.5, cx)
        .unwrap()
}

#[gpui::test]
fn only_a_resolved_forward_input_can_rejoin_without_movement(cx: &mut TestAppContext) {
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    let backward = last.get().unwrap();
    assert_eq!(backward.direction(), Some(ScrollDirection::Backward));
    assert_eq!(backward.movement(), Some(ScrollDirection::Backward));
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &backward, &target, &draft, cx
    )));
    wheel(-120., &mut cx);
    let forward = last.get().unwrap();
    assert_eq!(forward.direction(), Some(ScrollDirection::Forward));
    assert_eq!(forward.movement(), Some(ScrollDirection::Forward));
    assert_eq!(forward.phase(), Some(TouchPhase::Moved));
    let before = document.read_with(&cx, |document, _| {
        document.list_state().logical_scroll_top()
    });
    assert!(document.update(&mut cx, |document, cx| rejoin(
        document, &forward, &target, &draft, cx
    )));
    // An input result can be consumed only once, even before the next layout.
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &forward, &target, &draft, cx
    )));
    draw(&mut cx);
    document.read_with(&cx, |document, _| {
        assert_eq!(document.pinned_scroll_id(), Some(&"run"));
        let after = document.list_state().logical_scroll_top();
        assert_eq!(
            (after.item_ix, after.offset_in_item),
            (before.item_ix, before.offset_in_item)
        );
    });
}

#[gpui::test]
fn returning_to_the_edge_survives_a_zero_movement_packet_in_the_same_batch(
    cx: &mut TestAppContext,
) {
    let (document, mut cx, _, draft) = prepared(cx);
    let target = Position::new(
        "history",
        (0..50).map(|ix| format!("line {ix}\n").len()).sum(),
        Affinity::After,
    );
    document.update(&mut cx, |document, cx| {
        document.set_dynamic_trailer(false, cx)
    });
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    cx.update(|window, cx| {
        for _ in 0..2 {
            window.dispatch_event(
                gpui::PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: point(px(300.), px(150.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
                    ..Default::default()
                }),
                cx,
            );
        }
    });
    cx.run_until_parked();
    let input = last.get().unwrap();
    assert_eq!(input.direction(), Some(ScrollDirection::Forward));
    assert_eq!(
        input.movement(),
        None,
        "the last packet is clamped at the edge"
    );
    let before = document.read_with(&cx, |document, _| {
        document.list_state().logical_scroll_top()
    });
    assert!(document.update(&mut cx, |document, cx| rejoin(
        document, &input, &target, &draft, cx
    )));
    draw(&mut cx);
    document.read_with(&cx, |document, _| {
        let after = document.list_state().logical_scroll_top();
        assert_eq!(
            (after.item_ix, after.offset_in_item),
            (before.item_ix, before.offset_in_item)
        );
        assert_eq!(document.pinned_scroll_id(), Some(&"run"));
    });
}

#[gpui::test]
fn inspecting_history_revokes_a_result_before_the_next_layout(cx: &mut TestAppContext) {
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    wheel(-120., &mut cx);
    let input = last.get().unwrap();
    cx.simulate_click(point(px(100.), px(100.)), Modifiers::default());
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &input, &target, &draft, cx
    )));
    document.read_with(&cx, |document, _| {
        assert!(document.pinned_scroll_id().is_none())
    });
}

#[gpui::test]
fn another_layout_cannot_reuse_a_scroll_result(cx: &mut TestAppContext) {
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    wheel(-120., &mut cx);
    let input = last.get().unwrap();
    draw(&mut cx);
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &input, &target, &draft, cx
    )));
}

#[gpui::test]
fn retargeting_cannot_remove_a_rejoins_first_layout_check(cx: &mut TestAppContext) {
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    wheel(-120., &mut cx);
    let input = last.get().unwrap();
    let before = document.read_with(&cx, |document, _| {
        document.list_state().logical_scroll_top()
    });
    let later = Position::new(
        "history",
        (0..55).map(|ix| format!("line {ix}\n").len()).sum(),
        Affinity::After,
    );
    // Keep both host operations in one update: VisualTestContext may draw
    // between updates, which would already complete the conditional rejoin.
    document.update(&mut cx, |document, cx| {
        assert!(rejoin(document, &input, &target, &draft, cx));
        document
            .pin_scroll_forward_above("run", &later, &draft, px(0.), 0.5..=0.5, cx)
            .unwrap();
    });
    draw(&mut cx);
    document.read_with(&cx, |document, _| {
        assert!(document.pinned_scroll_id().is_none());
        let after = document.list_state().logical_scroll_top();
        assert_eq!(
            (after.item_ix, after.offset_in_item),
            (before.item_ix, before.offset_in_item)
        );
    });
}

#[gpui::test]
fn new_input_revokes_a_rejoin_before_its_first_layout(cx: &mut TestAppContext) {
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    wheel(-120., &mut cx);
    let input = last.get().unwrap();
    assert!(document.update(&mut cx, |document, cx| rejoin(
        document, &input, &target, &draft, cx
    )));
    wheel(24., &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(document.pinned_scroll_id().is_none())
    });
}

#[gpui::test]
fn scrollbar_return_waits_for_release_and_attaches_without_moving(cx: &mut TestAppContext) {
    use super::super::super::DocumentScrollbarHandle;
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    let handle = document.read_with(&cx, |document, _| DocumentScrollbarHandle {
        list: document.list_state(),
        input: document.scrollbar_input.clone(),
    });
    let initial = handle.offset();
    handle.start_drag();
    handle.set_offset(initial + point(px(0.), px(100.)));
    draw(&mut cx);
    handle.set_offset(initial - point(px(0.), px(20.)));
    draw(&mut cx);
    let dragging = last.get().unwrap();
    assert!(dragging.is_dragging());
    assert_eq!(dragging.direction(), Some(ScrollDirection::Forward));
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &dragging, &target, &draft, cx
    )));
    let returned = handle.offset();
    handle.end_drag();
    draw(&mut cx);
    let released = last.get().unwrap();
    assert!(!released.is_dragging());
    assert_eq!(released.phase(), Some(TouchPhase::Ended));
    assert!(document.update(&mut cx, |document, cx| rejoin(
        document, &released, &target, &draft, cx
    )));
    draw(&mut cx);
    assert_eq!(handle.offset(), returned);
    document.read_with(&cx, |document, _| {
        assert_eq!(document.pinned_scroll_id(), Some(&"run"))
    });
}

#[gpui::test]
fn cancelling_a_scrollbar_return_does_not_rejoin(cx: &mut TestAppContext) {
    use super::super::super::DocumentScrollbarHandle;
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    let handle = document.read_with(&cx, |document, _| DocumentScrollbarHandle {
        list: document.list_state(),
        input: document.scrollbar_input.clone(),
    });
    let initial = handle.offset();
    handle.start_drag();
    handle.set_offset(initial + point(px(0.), px(100.)));
    draw(&mut cx);
    handle.set_offset(initial - point(px(0.), px(20.)));
    draw(&mut cx);
    handle.cancel_drag();
    draw(&mut cx);
    let cancelled = last.get().unwrap();
    assert_eq!(cancelled.phase(), Some(TouchPhase::Cancelled));
    assert!(!cancelled.is_dragging());
    assert!(!document.update(&mut cx, |document, cx| rejoin(
        document, &cancelled, &target, &draft, cx
    )));
    document.read_with(&cx, |document, _| {
        assert!(document.pinned_scroll_id().is_none());
        assert!(!document.list_state().is_scrollbar_dragging());
    });
}

#[gpui::test]
fn a_history_update_cannot_reuse_the_previous_inputs_layout(cx: &mut TestAppContext) {
    use crate::document::{
        DocumentProjection, DocumentRegion, DocumentSnapshot, DocumentStyles, EditPolicy,
    };
    let (document, mut cx, target, draft) = prepared(cx);
    let (last, _subscription) = observe(&document, &mut cx);
    wheel(100., &mut cx);
    wheel(-120., &mut cx);
    let input = last.get().unwrap();
    document.update(&mut cx, |document, cx| {
        let fragment = DocumentSnapshot::new(
            "new\n",
            vec![DocumentRegion::new("later", 0..4, EditPolicy::Readonly)],
            DocumentProjection::identity(4),
            vec![],
            DocumentStyles::default(),
        );
        document
            .apply_host_splice(
                document.revision(),
                Some(&"draft"),
                Some(&"draft"),
                fragment,
                cx,
            )
            .unwrap();
        assert!(!rejoin(document, &input, &target, &draft, cx));
    });
}

#[gpui::test]
fn a_scroll_result_belongs_to_one_document(cx: &mut TestAppContext) {
    let (first, mut first_cx, _, _) = prepared(cx);
    let (first_input, _first_subscription) = observe(&first, &mut first_cx);
    wheel(100., &mut first_cx);
    wheel(-120., &mut first_cx);
    let other = first_input.get().unwrap();
    let (second, mut second_cx, target, draft) = prepared(cx);
    let (second_input, _second_subscription) = observe(&second, &mut second_cx);
    wheel(100., &mut second_cx);
    wheel(-120., &mut second_cx);
    let own = second_input.get().unwrap();
    assert!(!second.update(&mut second_cx, |document, cx| rejoin(
        document, &other, &target, &draft, cx
    )));
    assert!(second.update(&mut second_cx, |document, cx| rejoin(
        document, &own, &target, &draft, cx
    )));
}
