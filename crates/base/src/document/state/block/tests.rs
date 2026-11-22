use std::{cell::RefCell, rc::Rc};

use gpui::{
    Entity, IntoElement, ParentElement as _, Styled as _, TestAppContext, VisualTestContext, div,
    font, size,
};

use super::{
    super::{DocumentLayoutItem, tests::document_view, text_rows::BlockHeight},
    *,
};
use crate::document::{
    DocumentBlock, DocumentProjection, DocumentRegion, DocumentSnapshot, DocumentState,
    DocumentStyles, EditPolicy, ProjectionSpan, TextRows,
};

const OBJECT: &str = "\u{fffc}";

/// Blocks `ids`, in order, each an object hidden from the text, then a draft.
fn open(
    document: &Entity<DocumentState<&'static str>>,
    ids: &[&'static str],
    cx: &mut VisualTestContext,
) {
    let source = format!("{}draft", OBJECT.repeat(ids.len()));
    let mut regions = Vec::new();
    let mut hidden = Vec::new();
    let mut blocks = Vec::new();
    for (ix, id) in ids.iter().enumerate() {
        let range = ix * OBJECT.len()..(ix + 1) * OBJECT.len();
        regions.push(DocumentRegion::new(*id, range.clone(), EditPolicy::Atomic));
        hidden.push(ProjectionSpan::hide(range.clone()));
        blocks.push(DocumentBlock::new(*id, range));
    }
    let draft = ids.len() * OBJECT.len()..source.len();
    regions.push(DocumentRegion::new("draft", draft, EditPolicy::Editable));
    document.update(cx, |document, cx| {
        document.set_text_rows(Some(TextRows::new()), cx);
        document
            .reset(
                DocumentSnapshot::new(
                    source.clone(),
                    regions,
                    DocumentProjection::new(source.len(), hidden).unwrap(),
                    blocks,
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

/// Resizes the window and draws until the width has held long enough not to
/// read as a drag, and every block has rewrapped.
fn settle_at(
    document: &Entity<DocumentState<&'static str>>,
    width: f32,
    cx: &mut VisualTestContext,
) {
    cx.simulate_resize(size(px(width), px(300.)));
    for _ in 0..50 {
        draw(cx);
        if !document.read_with(cx, |document, _| document.is_laying_out_text()) {
            break;
        }
    }
    draw(cx);
    draw(cx);
    assert!(
        wraps(document, cx)
            .iter()
            .all(|(_, wrapped_for, _, _)| *wrapped_for == px(width))
    );
}

fn text(words: usize) -> BlockText {
    BlockText::new(
        "wrapped words ".repeat(words),
        font("Zed Mono"),
        px(10.),
        px(14.),
    )
    .with_frame(
        TextFrame::new()
            .with_space_before(px(4.))
            .with_space_after(px(6.))
            .with_inset_start(px(12.))
            .with_inset_end(px(8.)),
    )
    .with_min_height(px(24.))
}

/// Draws a block as its declared text: the frame's spaces around its rows,
/// each row unwrapped at the text's line height.
fn render(context: &DocumentBlockContext<&'static str>) -> gpui::AnyElement {
    let rows = context.rows().expect("declared text has rows");
    div()
        .pt(px(4.))
        .pb(px(6.))
        .child(div().min_h(px(24.)).children(rows.rows().iter().map(|row| {
            div()
                .h(rows.line_height())
                .whitespace_nowrap()
                .child(rows.text()[row.clone()].to_string())
        })))
        .into_any_element()
}

/// Each block's wrap: the block width its rows were wrapped for, its rows
/// and its height.
fn wraps(
    document: &Entity<DocumentState<&'static str>>,
    cx: &mut VisualTestContext,
) -> Vec<(&'static str, Pixels, BlockRows, Pixels)> {
    document.read_with(cx, |document, _| {
        document
            .layout_items
            .iter()
            .filter_map(|item| match item {
                DocumentLayoutItem::Block { id, height, .. } => match &height.0 {
                    BlockHeight::Wrapped(wrapped) => Some((
                        *id,
                        wrapped.width(),
                        wrapped.rows().clone(),
                        wrapped.height(),
                    )),
                    other => panic!("block {id} is {other:?}"),
                },
                _ => None,
            })
            .collect()
    })
}

#[gpui::test]
fn declared_text_wraps_to_the_block_width_without_measuring(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    let seen = Rc::new(RefCell::new(Vec::new()));
    document.update(&mut cx, |document, cx| {
        document.set_block_geometry_resolver(|_, _, _| BlockGeometry::text(text(8)), cx);
        document.set_block_context_renderer(
            {
                let seen = seen.clone();
                move |context, _, _| {
                    seen.borrow_mut().push(context.rows().cloned());
                    render(&context)
                }
            },
            cx,
        );
    });
    open(&document, &["call"], &mut cx);
    // Measured before the document opened as text rows.
    let measured = document.read_with(&cx, |document, _| document.list_state.measurement().items);

    let mut counts = Vec::new();
    for width in [900., 240., 900.] {
        cx.simulate_resize(size(px(width), px(400.)));
        draw(&mut cx);
        let block = wraps(&document, &mut cx);
        let [(_, wrapped_for, rows, height)] = block.as_slice() else {
            panic!("one block");
        };
        assert_eq!(*wrapped_for, px(width));
        // The rows are the text's, wrapped to the frame's width in the block.
        assert_eq!(rows.width(), px(width) - px(20.));
        let joined: String = rows
            .rows()
            .iter()
            .map(|row| &rows.text()[row.clone()])
            .collect();
        assert_eq!(joined, rows.text().as_ref());
        assert_eq!(
            *height,
            px(4.) + (rows.line_height() * rows.rows().len() as f32).max(px(24.)) + px(6.)
        );
        // The renderer drew the rows Base holds.
        assert_eq!(seen.borrow().last(), Some(&Some(rows.clone())));
        counts.push(rows.rows().len());
    }
    assert_eq!(counts[0], 1);
    assert!(counts[1] > 1, "a narrow block wraps its text: {counts:?}");
    assert_eq!(counts[2], 1);
    document.read_with(&cx, |document, _| {
        assert_eq!(document.list_state.measurement().items, measured);
    });
}

#[gpui::test]
fn a_line_break_starts_a_row_and_fixed_blocks_keep_their_height(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    document.update(&mut cx, |document, cx| {
        document.set_block_geometry_resolver(
            |id, _, _| match *id {
                "fixed" => BlockGeometry::fixed(px(40.)),
                _ => BlockGeometry::text(
                    BlockText::new("one\n\nthree", font("Zed Mono"), px(10.), px(14.)).with_frame(
                        TextFrame::new()
                            .with_space_before(px(4.))
                            .with_space_after(px(6.)),
                    ),
                ),
            },
            cx,
        );
        document.set_block_context_renderer(
            |context, _, _| match context.rows() {
                Some(_) => render(&context),
                None => div().h(px(40.)).into_any_element(),
            },
            cx,
        );
    });
    open(&document, &["fixed", "lines"], &mut cx);
    cx.simulate_resize(size(px(600.), px(400.)));
    draw(&mut cx);
    let rows = document.read_with(&cx, |document, _| {
        let mut rows = None;
        for item in document.layout_items.iter() {
            if let DocumentLayoutItem::Block { id, height, .. } = item {
                match (*id, &height.0) {
                    ("fixed", BlockHeight::Declared(height)) => assert_eq!(*height, px(40.)),
                    ("lines", BlockHeight::Wrapped(wrapped)) => rows = Some(wrapped.rows().clone()),
                    (id, other) => panic!("block {id} is {other:?}"),
                }
            }
        }
        rows.unwrap()
    });
    let text = rows.text().clone();
    let rows: Vec<_> = rows.rows().iter().map(|row| &text[row.clone()]).collect();
    assert_eq!(rows, ["one", "", "three"]);
}

fn open_calls(
    document: &Entity<DocumentState<&'static str>>,
    cx: &mut VisualTestContext,
) -> Rc<RefCell<Vec<(&'static str, Pixels)>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let ids: Vec<&'static str> = (0..300)
        .map(|ix| &*Box::leak(format!("call-{ix}").into_boxed_str()))
        .collect();
    document.update(cx, |doc, cx| {
        doc.set_block_geometry_resolver(|_, _, _| BlockGeometry::text(text(20)), cx);
        doc.set_block_context_renderer(
            {
                let seen = seen.clone();
                move |context, _, _| {
                    seen.borrow_mut()
                        .push((*context.id(), context.rows().unwrap().width()));
                    render(&context)
                }
            },
            cx,
        );
    });
    open(document, &ids, cx);
    settle_at(document, 900., cx);
    seen
}

#[gpui::test]
fn revealing_a_distant_block_during_resize_paints_current_rows_on_arrival(cx: &mut TestAppContext) {
    use crate::document::{Affinity, DocumentPosition};
    let (document, mut cx) = document_view(cx);
    let seen = open_calls(&document, &mut cx);
    seen.borrow_mut().clear();
    cx.simulate_resize(size(px(260.), px(300.)));
    document.update(&mut cx, |doc, cx| {
        doc.reveal_position_at_top(&DocumentPosition::new("call-280", 0, Affinity::After), cx)
            .unwrap();
    });
    draw(&mut cx);
    for target in ["call-280", "call-281"] {
        let seen = seen.borrow();
        let first = seen.iter().find(|(id, _)| *id == target);
        assert_eq!(first, Some(&(target, px(240.))), "first layout of {target}");
    }
}

#[gpui::test]
fn cold_reveal_places_first_frame_before_the_first_wheel(cx: &mut TestAppContext) {
    use crate::document::{Affinity, DocumentPosition};
    use gpui::{ScrollDelta, ScrollWheelEvent, point};

    let (doc, mut cx) = document_view(cx);
    cx.simulate_resize(size(px(450.), px(300.)));
    draw(&mut cx);
    let mut ids: Vec<_> = (0..120)
        .map(|i| &*Box::leak(format!("prior-{i}").into_boxed_str()))
        .collect();
    ids.extend(["previous", "restored"]);
    ids.extend((0..20).map(|i| &*Box::leak(format!("later-{i}").into_boxed_str())));
    doc.update(&mut cx, |doc, cx| {
        doc.set_block_geometry_resolver(|_, _, _| BlockGeometry::text(text(20)), cx);
        doc.set_block_context_renderer(|context, _, _| render(&context), cx);
    });
    open(&doc, &ids, &mut cx);
    doc.update(&mut cx, |doc, cx| {
        doc.reveal_position_at_top(&DocumentPosition::new("restored", 0, Affinity::After), cx)
            .unwrap();
    });
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let top = |cx: &VisualTestContext| {
        doc.read_with(cx, |doc, _| {
            (
                *doc.viewport_top().unwrap().node_id(),
                doc.list_state.logical_scroll_top().offset_in_item,
            )
        })
    };
    assert_eq!(top(&cx), ("restored", px(0.)), "first frame");
    let previous_height = cx.update(|window, cx| {
        wrap_block(
            &Arc::new(text(20)),
            px(450.),
            &mut BlockAdvances::default(),
            window,
            cx,
        )
        .height()
    });
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(225.), px(150.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(7.))),
        ..Default::default()
    });
    let expected = ("previous", previous_height - px(7.));
    assert_eq!(top(&cx), expected, "one wheel");
    settle_at(&doc, 450., &mut cx);
    assert_eq!(top(&cx), expected, "settled");
}

#[gpui::test]
fn reverse_scroll_during_resize_paints_current_rows_on_arrival(cx: &mut TestAppContext) {
    use crate::document::{Affinity, DocumentPosition};
    let (document, mut cx) = document_view(cx);
    let seen = open_calls(&document, &mut cx);
    document.update(&mut cx, |doc, cx| {
        doc.reveal_position_at_top(&DocumentPosition::new("call-280", 0, Affinity::After), cx)
            .unwrap();
    });
    draw(&mut cx);
    let distance = document.read_with(&cx, |doc, _| {
        let list = &doc.list_state;
        let y = |id| {
            let ix = doc
                .layout_items
                .iter()
                .position(
                    |item| matches!(item, DocumentLayoutItem::Block { id: b, .. } if *b == id),
                )
                .unwrap();
            list.item_top(ix).unwrap()
        };
        y("call-185") - y("call-280") + px(7.)
    });
    seen.borrow_mut().clear();
    cx.simulate_resize(size(px(260.), px(300.)));
    document.update(&mut cx, |doc, cx| doc.scroll_by(distance, cx));
    draw(&mut cx);
    let landed = document.read_with(&cx, |doc, _| *doc.viewport_top().unwrap().node_id());
    assert_ne!(landed, "call-280");
    let top = document.read_with(&cx, |doc, _| doc.list_state.logical_scroll_top());
    {
        let observed = seen.borrow();
        let first = observed.iter().position(|(id, _)| *id == landed).unwrap();
        assert!(
            observed.len() > first + 1,
            "expected a neighboring card in view"
        );
        assert!(
            observed[first..]
                .iter()
                .all(|(_, width)| *width == px(240.)),
            "first layout at reverse scroll landing: {:?}",
            &observed[first..]
        );
    }
    settle_at(&document, 260., &mut cx);
    document.read_with(&cx, |doc, _| {
        let settled = doc.list_state.logical_scroll_top();
        assert_eq!(settled.item_ix, top.item_ix);
        assert_eq!(settled.offset_in_item, top.offset_in_item);
    });
}

#[gpui::test]
fn a_scrollbar_drag_onto_an_unwrapped_oversized_block_lands_where_it_was_released(
    cx: &mut TestAppContext,
) {
    use super::super::DocumentScrollbarHandle;
    use crate::ScrollbarHandle;
    use gpui::point;

    let (document, mut cx) = document_view(cx);
    // One logical line larger than a frame's budget rewraps in the background.
    let large = Arc::new(text(2).with_paragraphs(vec![BlockParagraph::new(
        "a long line ".repeat(3000),
        font("Zed Mono"),
        px(11.),
        px(14.),
    )]));
    let heights = cx.update(|window, cx| {
        [800., 400.]
            .map(|w| wrap_block(&large, px(w), &mut BlockAdvances::default(), window, cx).height())
    });
    document.update(&mut cx, |doc, cx| {
        let declaration = large.clone();
        // The other blocks fit one line at both widths, so they keep their heights.
        doc.set_block_geometry_resolver(
            move |id, _, _| match *id {
                "large" => BlockGeometry(Declaration::Text(declaration.clone())),
                _ => BlockGeometry::text(text(2)),
            },
            cx,
        );
        doc.set_block_context_renderer(
            move |context, _, _| {
                if *context.id() != "large" {
                    return render(&context);
                }
                let wide = context.rows().unwrap().width() == px(780.);
                div().h(heights[usize::from(!wide)]).into_any_element()
            },
            cx,
        );
    });
    let filler = |ix: usize| &*Box::leak(format!("filler-{ix}").into_boxed_str());
    let mut ids = (0..80).map(filler).collect::<Vec<_>>();
    ids.push("large");
    ids.extend((80..100).map(filler));
    open(&document, &ids, &mut cx);
    settle_at(&document, 800., &mut cx);
    let large_top = document.read_with(&cx, |doc, _| {
        let ix = doc
            .layout_items
            .iter()
            .position(|item| matches!(item, DocumentLayoutItem::Block { id, .. } if *id == "large"))
            .unwrap();
        doc.list_state.item_top(ix).unwrap()
    });
    let frame = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
    };
    // Frames without running tasks keep the oversized rewrap in flight.
    cx.simulate_resize(size(px(400.), px(300.)));
    frame(&mut cx);
    let large_width = |cx: &mut VisualTestContext| {
        wraps(&document, cx)
            .into_iter()
            .find(|(id, ..)| *id == "large")
            .map(|(_, width, ..)| width)
    };
    assert_eq!(
        large_width(&mut cx),
        Some(px(800.)),
        "rewrap still in flight"
    );

    let handle = document.read_with(&cx, |doc, _| DocumentScrollbarHandle {
        list: doc.list_state(),
        input: doc.scrollbar_input.clone(),
    });
    // Each position puts the oversized block in the destination viewport.
    handle.start_drag();
    handle.set_offset(point(px(0.), px(80.) - large_top));
    frame(&mut cx);
    let waited = document.read_with(&cx, |doc, _| doc.list_state.logical_scroll_top());
    assert_eq!(
        waited.item_ix, 0,
        "the landing waits for the oversized block"
    );
    handle.set_offset(point(px(0.), px(60.) - large_top));
    frame(&mut cx);
    // On release the scrollbar places the list where the pointer is once more.
    handle.set_offset(point(px(0.), px(60.) - large_top));
    let released = document.read_with(&cx, |doc, _| doc.list_state.logical_scroll_top());
    handle.end_drag();

    settle_at(&document, 400., &mut cx);
    document.read_with(&cx, |doc, _| {
        let settled = doc.list_state.logical_scroll_top();
        assert_eq!(
            (settled.item_ix, settled.offset_in_item),
            (released.item_ix, released.offset_in_item)
        );
    });
}

#[gpui::test]
fn blocks_out_of_view_rewrap_within_a_budget_after_a_resize(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    let ids: Vec<&'static str> = (0..300)
        .map(|ix| &*Box::leak(format!("block-{ix}").into_boxed_str()))
        .collect();
    document.update(&mut cx, |document, cx| {
        // 300 blocks of 210 bytes: more than one frame's rewrap.
        document.set_block_geometry_resolver(|_, _, _| BlockGeometry::text(text(15)), cx);
        document.set_block_context_renderer(|context, _, _| render(&context), cx);
    });
    open(&document, &ids, &mut cx);
    settle_at(&document, 900., &mut cx);

    // A lone resize rewraps the view and a bounded amount more in its frame;
    // the rest keep their rows, and heights, for the earlier width until
    // later frames rewrap them.
    REWRAP_PASSES.with_borrow_mut(|passes| *passes = Some(Vec::new()));
    cx.simulate_resize(size(px(260.), px(300.)));
    for _ in 0..20 {
        draw(&mut cx);
    }
    let passes = REWRAP_PASSES.with_borrow_mut(|passes| passes.take().unwrap());
    let rewrapped: Vec<_> = passes.into_iter().filter(|bytes| *bytes > 0).collect();
    assert!(rewrapped.len() > 1, "{rewrapped:?}");
    assert!(
        rewrapped
            .iter()
            .all(|bytes| *bytes <= super::super::text_rows::REWRAP_IN_FRAME_BYTES),
        "{rewrapped:?}"
    );
    assert!(
        wraps(&document, &mut cx)
            .iter()
            .all(|(_, width, rows, height)| *width == px(260.)
                && *height == px(4.) + rows.line_height() * rows.rows().len() as f32 + px(6.))
    );
    assert!(!document.read_with(&cx, |document, _| document.is_laying_out_text()));
}

#[gpui::test]
fn oversized_body_keeps_its_published_height_until_the_latest_wrap_finishes(
    cx: &mut TestAppContext,
) {
    let (document, mut cx) = document_view(cx);
    let part = |s: String| BlockParagraph::new(s, font("Zed Mono"), px(11.), px(14.));
    // Multiple frame-sized groups plus a single logical line bigger than a frame.
    let mut parts = vec![part("short words on a hard line\n".repeat(240)); 12];
    parts.push(part("a long line ".repeat(3000)));
    let declaration = Arc::new(text(2).with_paragraphs(parts));
    let heights = cx.update(|window, cx| {
        [800., 400., 220.].map(|w| {
            wrap_block(
                &declaration,
                px(w),
                &mut BlockAdvances::default(),
                window,
                cx,
            )
            .height()
        })
    });
    document.update(&mut cx, |doc, cx| {
        let large = declaration.clone();
        doc.set_block_geometry_resolver(
            move |id, _, _| {
                if *id == "large" {
                    BlockGeometry(Declaration::Text(large.clone()))
                } else {
                    BlockGeometry::text(text(5))
                }
            },
            cx,
        );
        doc.set_block_context_renderer(
            move |context, _, _| {
                if *context.id() != "large" {
                    return render(&context);
                }
                let width = context.rows().unwrap().width();
                let ix = [780., 380., 200.]
                    .iter()
                    .position(|w| px(*w) == width)
                    .unwrap();
                div().h(heights[ix]).into_any_element()
            },
            cx,
        );
    });
    let mut ids = (0..80)
        .map(|ix| &*Box::leak(format!("filler-{ix}").into_boxed_str()))
        .collect::<Vec<_>>();
    ids.push("large");
    open(&document, &ids, &mut cx);
    settle_at(&document, 800., &mut cx);
    let last = |cx: &mut VisualTestContext| {
        let (_, width, _, height) = wraps(&document, cx).pop().unwrap();
        (width, height)
    };
    let old = last(&mut cx);
    REWRAP_PASSES.with_borrow_mut(|passes| *passes = Some(Vec::new()));
    cx.simulate_resize(size(px(400.), px(300.)));
    draw(&mut cx);
    let intermediate = last(&mut cx);
    assert_eq!(intermediate, old);
    // Change width again and reveal the oversized body before its previous
    // layout completes. No synchronous full wrap may bypass the worker.
    use crate::document::{Affinity, DocumentPosition};
    let expected = heights[2];
    TRACK_LARGE.set(true);
    cx.simulate_resize(size(px(220.), px(300.)));
    document.update(&mut cx, |doc, cx| {
        doc.reveal_position_at_top(&DocumentPosition::new("large", 0, Affinity::After), cx)
            .unwrap();
    });
    for _ in 0..50 {
        draw(&mut cx);
        let actual = last(&mut cx);
        if actual != intermediate {
            assert_eq!(actual, (px(220.), expected));
            break;
        }
    }
    assert_eq!(last(&mut cx), (px(220.), expected));
    draw(&mut cx);
    assert_eq!(
        document.read_with(&cx, |doc, _| *doc.viewport_top().unwrap().node_id()),
        "large"
    );
    // Now the same block is visible, with no reveal to drive the next wrap.
    cx.simulate_resize(size(px(400.), px(300.)));
    for _ in 0..50 {
        draw(&mut cx);
        if last(&mut cx) == (px(400.), heights[1]) {
            break;
        }
    }
    assert_eq!(last(&mut cx), (px(400.), heights[1]));
    TRACK_LARGE.set(false);
    assert!(expected > old.1);
    let passes = REWRAP_PASSES.with_borrow_mut(|passes| passes.take().unwrap());
    assert!(passes.iter().filter(|b| **b > 0).count() >= 3, "{passes:?}");
    assert!(
        passes
            .iter()
            .all(|b| *b <= super::super::text_rows::REWRAP_IN_FRAME_BYTES),
        "{passes:?}"
    );
}

#[gpui::test]
fn native_paragraph_height_tracks_wrapped_styled_text_without_changing_title_rows(
    cx: &mut TestAppContext,
) {
    use gpui::{FontWeight, HighlightStyle, StyledText};
    let (document, mut cx) = document_view(cx);
    let sample = "wide 汉🙂 words with a gutter\n\nmore words here ".repeat(4);
    let highlights = || {
        vec![(
            0..4,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        )]
    };
    document.update(&mut cx, |doc, cx| {
        let source = sample.clone();
        doc.set_block_geometry_resolver(
            move |_, window, _| {
                let style = window.text_style();
                let body = BlockParagraph::styled(
                    source.clone().into(),
                    &style,
                    px(15.),
                    px(20.),
                    highlights(),
                )
                .with_inset(px(16.))
                .with_indent(px(22.));
                let title = text(1).with_paragraphs(vec![body]);
                assert_eq!(title.wrap_bytes(), title.text().len() + source.len());
                BlockGeometry::text(title)
            },
            cx,
        );
        doc.set_block_context_renderer(
            move |context, _, _| {
                div()
                    .child(render(&context))
                    .child(
                        div()
                            .pl(px(16.))
                            .text_size(px(15.))
                            .line_height(px(20.))
                            .child(
                                StyledText::new(sample.clone())
                                    .with_highlights(highlights())
                                    .with_line_indent(gpui::LineIndent::uniform(px(22.))),
                            ),
                    )
                    .into_any_element()
            },
            cx,
        );
    });
    open(&document, &["paragraph"], &mut cx);
    let measured = document.read_with(&cx, |doc, _| doc.list_state().measurement().items);
    settle_at(&document, 800., &mut cx);
    let wide = wraps(&document, &mut cx).remove(0);
    settle_at(&document, 210., &mut cx);
    let narrow = wraps(&document, &mut cx).remove(0);
    assert_eq!(wide.2.rows(), narrow.2.rows());
    assert!(narrow.3 > wide.3);
    assert_eq!(
        document.read_with(&cx, |doc, _| doc.list_state().measurement().items),
        measured
    );
}

#[gpui::test]
fn a_drag_rewraps_only_the_view_until_the_width_holds(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    let ids: Vec<&'static str> = (0..80)
        .map(|ix| &*Box::leak(format!("block-{ix}").into_boxed_str()))
        .collect();
    document.update(&mut cx, |document, cx| {
        document.set_block_geometry_resolver(|_, _, _| BlockGeometry::text(text(4)), cx);
        document.set_block_context_renderer(|context, _, _| render(&context), cx);
    });
    open(&document, &ids, &mut cx);
    settle_at(&document, 700., &mut cx);
    // A lone resize rewraps every block; a second width soon after it is a
    // drag, which rewraps the view while the rest keeps its earlier rows.
    cx.simulate_resize(size(px(500.), px(300.)));
    draw(&mut cx);
    assert!(
        wraps(&document, &mut cx)
            .iter()
            .all(|(_, width, _, _)| *width == px(500.))
    );
    cx.simulate_resize(size(px(450.), px(300.)));
    draw(&mut cx);
    let dragged = wraps(&document, &mut cx);
    assert_eq!(dragged[0].1, px(450.));
    assert_eq!(dragged.last().unwrap().1, px(500.));
    // Once the width holds, the whole document rewraps.
    for _ in 0..3 {
        draw(&mut cx);
    }
    assert!(
        wraps(&document, &mut cx)
            .iter()
            .all(|(_, width, _, _)| *width == px(450.))
    );
}

#[cfg(debug_assertions)]
#[gpui::test]
#[should_panic(expected = "its geometry declares")]
fn a_renderer_that_changes_a_declared_height_fails_its_check(cx: &mut TestAppContext) {
    let (document, mut cx) = document_view(cx);
    document.update(&mut cx, |document, cx| {
        document.set_block_geometry_resolver(|_, _, _| BlockGeometry::fixed(px(40.)), cx);
        document.set_block_context_renderer(|_, _, _| div().h(px(60.)).into_any_element(), cx);
    });
    open(&document, &["block"], &mut cx);
    cx.simulate_resize(size(px(600.), px(400.)));
    draw(&mut cx);
}
