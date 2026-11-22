use super::{draw, open_at, render, text};
use crate::document::{
    Affinity, BlockGeometry, BlockParagraph, DocumentPosition, DocumentState,
    state::{
        block::{BlockAdvances, TRACK_LARGE, wrap_block},
        tests::document_view,
    },
};
use gpui::{
    Entity, IntoElement, Pixels, ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext,
    VisualTestContext, div, font, point, px, size,
};
use std::{sync::Arc, time::Instant};

type Doc = Entity<DocumentState<&'static str>>;

fn setup(large: bool, cold: bool, cx: &mut TestAppContext) -> (Doc, VisualTestContext, Instant) {
    let (doc, mut view) = document_view(cx);
    view.simulate_resize(size(px(450.), px(300.)));
    draw(&mut view);
    let mut ids: Vec<_> = (0..120)
        .map(|i| &*Box::leak(format!("prior-{i}").into_boxed_str()))
        .collect();
    ids.extend(["previous", "target", "next"]);
    ids.extend((0..20).map(|i| &*Box::leak(format!("later-{i}").into_boxed_str())));
    let declaration = Arc::new(if large {
        text(2).with_paragraphs(vec![BlockParagraph::new(
            "a long line ".repeat(4096),
            font("Zed Mono"),
            px(11.),
            px(14.),
        )])
    } else {
        text(2)
    });
    let exact = view.update(|window, cx| {
        wrap_block(
            &declaration,
            px(450.),
            &mut BlockAdvances::default(),
            window,
            cx,
        )
        .height()
    });
    doc.update(&mut view, |doc, cx| {
        doc.set_block_geometry_resolver(
            move |id, _, _| match *id {
                "previous" | "next" => BlockGeometry::measured(),
                "target" => BlockGeometry::text(declaration.as_ref().clone()),
                _ => BlockGeometry::text(text(2)),
            },
            cx,
        );
        doc.set_block_context_renderer(
            move |ctx, _, _| match *ctx.id() {
                "previous" => div().h(px(59.)).into_any_element(),
                "next" => div().h(px(83.)).into_any_element(),
                "target" => {
                    assert!(
                        ctx.rows().is_some(),
                        "renderer received an estimated target"
                    );
                    div().h(exact).into_any_element()
                }
                _ => render(&ctx),
            },
            cx,
        );
    });
    TRACK_LARGE.set(large && super::experiment().defer_declarations);
    let start = Instant::now();
    open_at(&doc, &ids, cold.then_some("target"), &mut view);
    if !cold { doc.update(&mut view, |doc, cx| {
        doc.reveal_position_at_top(&DocumentPosition::new("target", 0, Affinity::After), cx)
            .unwrap();
    }); }
    (doc, view, start)
}

fn visible(doc: &Doc, view: &VisualTestContext) -> Option<(&'static str, Pixels)> {
    doc.read_with(view, |doc, _| {
        Some((
            *doc.viewport_top()?.node_id(),
            doc.list_state.logical_scroll_top().offset_in_item,
        ))
    })
}

fn top(doc: &Doc, view: &VisualTestContext) -> (&'static str, Pixels) { visible(doc, view).unwrap() }

fn frame(view: &mut VisualTestContext) {
    view.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

fn wheel(view: &mut VisualTestContext) {
    view.simulate_event(ScrollWheelEvent {
        position: point(px(225.), px(150.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(7.))),
        ..Default::default()
    });
}

fn settle(doc: &Doc, view: &mut VisualTestContext) {
    for _ in 0..100 {
        draw(view);
        if !doc.read_with(view, |doc, _| doc.is_laying_out_text()) {
            return;
        }
    }
    panic!("cold layout did not settle");
}

#[gpui::test]
fn measured_neighbors_restore_then_consume_one_wheel(cx: &mut TestAppContext) {
    let (doc, mut view, _) = setup(false, true, cx);
    frame(&mut view);
    assert_eq!(top(&doc, &view), ("target", px(0.)));
    wheel(&mut view);
    assert_eq!(top(&doc, &view), ("previous", px(52.)));
    settle(&doc, &mut view);
    assert_eq!(top(&doc, &view), ("previous", px(52.)));
}

#[gpui::test]
fn oversized_target_restores_without_early_input(cx: &mut TestAppContext) {
    let (doc, mut view, start) = setup(true, true, cx);
    frame(&mut view);
    let first = visible(&doc, &view);
    let first_time = start.elapsed();
    for _ in 0..100 {
        if visible(&doc, &view).is_some() {
            break;
        }
        draw(&mut view);
    }
    assert_eq!(top(&doc, &view), ("target", px(0.)));
    eprintln!(
        "cold oversized no input first={first:?} elapsed={first_time:?} correct={:?}",
        start.elapsed()
    );
    wheel(&mut view);
    settle(&doc, &mut view);
    assert_eq!(top(&doc, &view), ("previous", px(52.)));
    TRACK_LARGE.set(false);
}

#[gpui::test]
fn oversized_first_frame_and_input(cx: &mut TestAppContext) {
    let (doc, mut view, start) = setup(true, true, cx);
    frame(&mut view);
    let first = visible(&doc, &view);
    if super::experiment().defer_declarations && super::experiment().rewrap_budget.is_some() {
        assert!(first.is_none());
        assert!(doc.read_with(&view, |doc, _| doc.block_layouts.is_empty()));
    } else if let Some(first) = first { assert_eq!(first, ("target", px(0.))); }
    let first_time = start.elapsed();
    wheel(&mut view);
    let after = visible(&doc, &view);
    settle(&doc, &mut view);
    let settled = top(&doc, &view);
    TRACK_LARGE.set(false);
    eprintln!(
        "cold oversized first={first:?} elapsed={first_time:?} wheel={after:?} settled={settled:?} elapsed={:?}",
        start.elapsed()
    );
    assert_eq!(settled, ("previous", px(52.)));
}

#[gpui::test]
fn wheel_cancels_an_unfinished_jump_on_an_already_visible_document(cx: &mut TestAppContext) {
    if !super::experiment().defer_declarations { return; }
    let (doc, mut view, _) = setup(true, false, cx);
    frame(&mut view);
    assert_eq!(top(&doc, &view), ("prior-0", px(0.)));
    wheel(&mut view);
    settle(&doc, &mut view);
    assert_eq!(top(&doc, &view), ("prior-0", px(0.)));
    TRACK_LARGE.set(false);
}
