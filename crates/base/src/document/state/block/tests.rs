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
    let text_len = text(15).text().len();
    assert!(
        rewrapped
            .iter()
            .all(|bytes| *bytes <= super::super::text_rows::REWRAP_IN_FRAME_BYTES + text_len),
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
