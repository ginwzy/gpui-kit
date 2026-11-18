use super::{document, draw};
use crate::document::{
    Affinity, AnchoredBlock, DocumentPosition, DocumentProjection, DocumentRegion,
    DocumentSnapshot, DocumentStyles, EditPolicy, ProjectionSpan,
};
use gpui::{
    Bounds, Context, Entity, InteractiveElement, IntoElement, ListOffset, Pixels, Styled,
    TestAppContext, VisualTestContext, div, px, size,
};

use super::super::super::DocumentState;

#[gpui::test]
fn reading_inside_an_anchored_block_survives_height_changes(cx: &mut TestAppContext) {
    let (document, mut cx) = document(1, cx);
    let text = "line\n".repeat(60);
    document.update(&mut cx, |document, cx| {
        document.set_block_renderer(
            |_, _, _| {
                div()
                    .h(px(300.))
                    .debug_selector(|| "reading-block".into())
                    .into_any_element()
            },
            cx,
        );
        document
            .reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![DocumentRegion::new(
                        "history",
                        0..text.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::identity(text.len()),
                    vec![],
                    DocumentStyles::default(),
                )
                .anchored_blocks(vec![AnchoredBlock::new(
                    "graphic",
                    DocumentPosition::new("history", 50, Affinity::Before),
                )]),
                cx,
            )
            .unwrap();
    });
    draw(&mut cx);
    draw(&mut cx);
    document.read_with(&cx, |document, _| {
        let row = document
            .layout_items
            .iter()
            .position(|item| {
                matches!(
                    item,
                    super::super::super::DocumentLayoutItem::AnchoredBlock { id: "graphic", .. }
                )
            })
            .unwrap();
        document.list_state().scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(100.),
        });
    });
    draw(&mut cx);
    let before = cx.debug_bounds("reading-block").unwrap().top();
    for height in [600., 250., 450.] {
        document.update(&mut cx, |document, cx| {
            document.set_block_renderer(
                move |_, _, _| {
                    div()
                        .h(px(height))
                        .debug_selector(|| "reading-block".into())
                        .into_any_element()
                },
                cx,
            )
        });
        draw(&mut cx);
        draw(&mut cx);
        assert!(
            (cx.debug_bounds("reading-block").unwrap().top() - before).abs() < px(1.),
            "height={height}"
        );
    }
    document.update(&mut cx, |document, cx| {
        document.set_block_renderer(
            |_, _, _| {
                div()
                    .h(px(50.))
                    .debug_selector(|| "reading-block".into())
                    .into_any_element()
            },
            cx,
        )
    });
    draw(&mut cx);
    draw(&mut cx);
    document.read_with(&cx, |document, _| {
        let row = document
            .layout_items
            .iter()
            .position(|item| {
                matches!(
                    item,
                    super::super::super::DocumentLayoutItem::AnchoredBlock { id: "graphic", .. }
                )
            })
            .unwrap();
        let list = document.list_state();
        // The shortened block ends exactly at the top and is no longer painted.
        assert!(
            (list.item_top(row).unwrap()
                + list.item_height(row).unwrap()
                + list.scroll_px_offset_for_scrollbar().y)
                .abs()
                < px(1.)
        );
    });
}

fn followed_block(
    height: Pixels,
    viewport_height: Pixels,
    cx: &mut TestAppContext,
) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
    let (document, mut cx) = document(1, cx);
    if viewport_height != px(300.) {
        cx.simulate_resize(size(px(600.), viewport_height));
    }
    let history = "line\n".repeat(60);
    let len = history.len();
    document.update(&mut cx, |document, cx| {
        set_block_height(document, height, cx);
        document
            .reset(
                DocumentSnapshot::new(
                    history + "draft",
                    vec![
                        DocumentRegion::new("history", 0..len, EditPolicy::Readonly),
                        DocumentRegion::new("draft", len..len + 5, EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(len + 5),
                    vec![],
                    DocumentStyles::default(),
                )
                .anchored_blocks(vec![AnchoredBlock::new(
                    "graphic",
                    DocumentPosition::new("history", len - 5, Affinity::Before),
                )]),
                cx,
            )
            .unwrap();
    });
    draw(&mut cx);
    document.update(&mut cx, |document, cx| {
        document
            .pin_scroll_forward_above(
                "run",
                &DocumentPosition::new("history", len, Affinity::Before),
                &draft_end(),
                px(20.),
                0.3..=0.85,
                cx,
            )
            .unwrap();
    });
    draw(&mut cx);
    let held = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    assert!(
        (held - (viewport_height - px(20.))).abs() < px(1.),
        "{held:?}"
    );
    (document, cx)
}

fn set_block_height(
    document: &mut DocumentState<&'static str>,
    height: Pixels,
    cx: &mut Context<DocumentState<&'static str>>,
) {
    document.set_block_renderer(move |_, _, _| div().h(height).into_any_element(), cx);
}

fn draft_end() -> DocumentPosition<&'static str> {
    DocumentPosition::new("draft", 5, Affinity::After)
}

fn draft_bounds(document: &DocumentState<&'static str>) -> Bounds<Pixels> {
    document.position_bounds(&draft_end()).unwrap()
}

#[gpui::test]
fn an_established_follow_compensates_for_block_height_changes(cx: &mut TestAppContext) {
    let (document, mut cx) = followed_block(px(200.), px(300.), cx);
    let before = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    for height in [150., 100., 200.] {
        document.update(&mut cx, |document, cx| {
            set_block_height(document, px(height), cx)
        });
        draw(&mut cx);
        let after = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
        assert!((after - before).abs() < px(1.), "height={height}");
    }
}

#[gpui::test]
fn resizing_discards_the_established_follow_checkpoint(cx: &mut TestAppContext) {
    let (document, mut cx) = followed_block(px(200.), px(300.), cx);
    let before = document.read_with(&cx, |document, _| draft_bounds(document).bottom());

    // Shrink the block before the output catches up to the taller viewport's hold.
    cx.simulate_resize(size(px(600.), px(340.)));
    draw(&mut cx);
    let resized = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    assert!((resized - before).abs() < px(1.));
    document.update(&mut cx, |document, cx| {
        set_block_height(document, px(150.), cx)
    });
    draw(&mut cx);
    let after = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    assert!(
        (after - (before - px(50.))).abs() < px(1.),
        "{before:?} -> {after:?}"
    );
}

#[gpui::test]
fn explicit_scroll_takes_priority_over_the_follow_checkpoint(cx: &mut TestAppContext) {
    let (document, mut cx) = followed_block(px(250.), px(340.), cx);
    let held = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    document.update(&mut cx, |document, cx| {
        set_block_height(document, px(200.), cx);
        document.scroll_by(px(20.), cx);
    });
    draw(&mut cx);
    let after = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    assert!(
        (after - (held - px(70.))).abs() < px(1.),
        "{held:?} -> {after:?}"
    );
}

#[gpui::test]
fn text_metrics_invalidate_the_follow_checkpoint_at_fixed_viewport_size(cx: &mut TestAppContext) {
    let (document, mut cx) = followed_block(px(300.), px(340.), cx);
    let held = document.read_with(&cx, |document, _| draft_bounds(document).bottom());
    cx.update(|window, cx| {
        let size = window.viewport_size();
        let scale = window.scale_factor();
        let line_height = draft_bounds(document.read(cx)).size.height;
        window.set_rem_size(window.rem_size() * 0.875);
        document.update(cx, |document, cx| set_block_height(document, px(250.), cx));
        window.draw(cx).clear(cx);
        let bounds = draft_bounds(document.read(cx));
        assert_eq!(window.viewport_size(), size);
        assert_eq!(window.scale_factor(), scale);
        assert!(bounds.size.height < line_height);
        let after = bounds.bottom();
        assert!(after < held - px(1.), "{held:?} -> {after:?}");
    });
}

#[gpui::test]
fn hidden_source_reveals_its_block_and_its_end_tracks_the_block_bottom(cx: &mut TestAppContext) {
    let (document, mut cx) = document(1, cx);
    let text = "line\n".repeat(60);
    document.update(&mut cx, |document, cx| {
        document.set_block_renderer(
            |_, _, _| {
                div()
                    .h(px(300.))
                    .debug_selector(|| "hidden-block".into())
                    .into_any_element()
            },
            cx,
        );
        document
            .reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![DocumentRegion::new(
                        "history",
                        0..text.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::new(
                        text.len(),
                        vec![ProjectionSpan::hide(50..55), ProjectionSpan::hide(55..60)],
                    )
                    .unwrap(),
                    vec![],
                    DocumentStyles::default(),
                )
                .anchored_blocks(vec![AnchoredBlock::new(
                    "graphic",
                    DocumentPosition::new("history", 50, Affinity::Before),
                )]),
                cx,
            )
            .unwrap();
        document
            .reveal_position_at_top(&DocumentPosition::new("history", 52, Affinity::After), cx)
            .unwrap();
    });
    draw(&mut cx);
    draw(&mut cx);
    assert!(cx.debug_bounds("hidden-block").unwrap().top().abs() < px(1.));
    document.read_with(&cx, |document, _| {
        let geometry = document.list_state();
        let (bottom, _) = document
            .content_line(
                &geometry,
                &DocumentPosition::new("history", 55, Affinity::Before),
                true,
            )
            .unwrap();
        let (top, _) = document
            .content_line(
                &geometry,
                &DocumentPosition::new("history", 52, Affinity::After),
                true,
            )
            .unwrap();
        assert_eq!(bottom - top, px(300.));
        let (after_padding, _) = document
            .content_line(
                &geometry,
                &DocumentPosition::new("history", 60, Affinity::Before),
                true,
            )
            .unwrap();
        assert_eq!(after_padding, bottom);
    });
}
