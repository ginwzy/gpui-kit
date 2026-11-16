use crate::document::{
    Affinity, DocumentPosition, DocumentProjection, DocumentRegion, DocumentSnapshot,
    DocumentStyles, EditPolicy, ProjectionSpan, state::tests::document_view,
};
use gpui::{ListOffset, Modifiers, MouseButton, Pixels, TestAppContext, point, px};
use std::time::Duration;

#[gpui::test]
fn presentation_splice_keeps_drag_endpoints_until_pointer_moves(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    let hidden = "hidden line\n".repeat(8);
    let source = format!("before\n{hidden}after\n");
    let start = "before\n".len();
    let end = start + hidden.len();
    let head = end + 3;
    document.update(&mut cx, |document, cx| {
        document
            .reset(
                DocumentSnapshot::new(
                    source.clone(),
                    vec![
                        DocumentRegion::new("history", 0..source.len(), EditPolicy::Readonly),
                        DocumentRegion::new(
                            "draft",
                            source.len()..source.len(),
                            EditPolicy::Editable,
                        ),
                    ],
                    DocumentProjection::new(source.len(), vec![ProjectionSpan::hide(start..end)])
                        .unwrap(),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let position = |offset| DocumentPosition::new("history", offset, Affinity::After);
    let at = |offset, cx: &gpui::VisualTestContext| {
        document.read_with(cx, |document, _| {
            document.position_bounds(&position(offset)).unwrap().origin + point(px(0.1), px(5.))
        })
    };
    let from = at(0, &cx);
    let to = at(head, &cx);
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    document.read_with(&cx, |document, _| {
        assert_eq!(document.selected_range(), 0..head)
    });
    document.update(&mut cx, |document, cx| {
        document
            .apply_host_node_splice(
                document.revision(),
                Some(&"history"),
                Some(&"draft"),
                DocumentSnapshot::new(
                    source.clone(),
                    vec![DocumentRegion::new(
                        "history",
                        0..source.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::identity(source.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
    for _ in 0..3 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        document.read_with(&cx, |document, _| {
            assert_eq!(
                document.selected_positions().unwrap(),
                (position(0), position(head))
            );
        });
    }
    // Continued real pointer movement still updates the head after the projection switch.
    let next = start + 3;
    let moved = at(next, &cx);
    cx.simulate_mouse_move(moved, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(moved, MouseButton::Left, Modifiers::default());
    document.read_with(&cx, |document, _| {
        assert_eq!(document.selected_range(), 0..next)
    });
}

#[gpui::test]
fn presentation_splice_preserves_active_drag_scrolling(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    let source = "history line\n".repeat(200);
    document.update(&mut cx, |document, cx| {
        document
            .reset(
                DocumentSnapshot::new(
                    source.clone(),
                    vec![
                        DocumentRegion::new("history", 0..source.len(), EditPolicy::Readonly),
                        DocumentRegion::new(
                            "draft",
                            source.len()..source.len(),
                            EditPolicy::Editable,
                        ),
                    ],
                    DocumentProjection::identity(source.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
        document.list_state.scroll_to(ListOffset {
            item_ix: 100,
            offset_in_item: Pixels::ZERO,
        });
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let (start, outside) = document.read_with(&cx, |document, _| {
        let viewport = document.list_state.viewport_bounds();
        (
            viewport.center(),
            point(viewport.left() + px(30.), viewport.bottom() + px(20.)),
        )
    });
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::default());
    let (anchor, head) = document.read_with(&cx, |document, _| document.model.selection_offsets());
    document.update(&mut cx, |document, cx| {
        document
            .apply_host_node_splice(
                document.revision(),
                Some(&"history"),
                Some(&"draft"),
                DocumentSnapshot::new(
                    source.clone(),
                    vec![DocumentRegion::new(
                        "history",
                        0..source.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::new(
                        source.len(),
                        vec![ProjectionSpan::hide(0.."history line\n".len())],
                    )
                    .unwrap(),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
    for _ in 0..5 {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    document.read_with(&cx, |document, _| {
        assert!(document.auto_scroll.is_active());
        assert_eq!(document.model.selection_offsets().0, anchor);
        assert!(document.model.cursor() > head);
    });
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
    document.read_with(&cx, |document, _| {
        assert!(!document.auto_scroll.is_active())
    });
}
