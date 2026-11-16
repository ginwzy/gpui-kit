use super::{document, draw};
use crate::document::{
    Affinity, AnchoredBlock, DocumentPosition, DocumentProjection, DocumentRegion,
    DocumentSnapshot, DocumentStyles, EditPolicy, ProjectionSpan,
};
use gpui::{InteractiveElement, IntoElement, ListOffset, Styled, TestAppContext, div, px};

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
