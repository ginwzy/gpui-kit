use crate::document::{
    DocumentInlineStyle, DocumentParagraphStyle, DocumentProjection, DocumentRegion,
    DocumentSnapshot, DocumentState, DocumentStyles, EditPolicy, TableRow, TextFrame, TextRows,
    state::{DocumentLayoutItem, row::Layout, tests::document_view},
};
use gpui::{
    AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Pixels, Render, Styled,
    TestAppContext, TextAlign, VisualTestContext, Window, div, point, px, size,
};
use std::sync::{Arc, atomic::Ordering};

use super::{Resolved, table_item};

struct Root {
    document: Entity<DocumentState<&'static str>>,
    family: &'static str,
    font_size: Pixels,
    width: Pixels,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(self.width)
            .h(px(400.))
            .font_family(self.family)
            .text_size(self.font_size)
            .child(self.document.clone())
    }
}

/// Each table row's geometry, in document order.
fn geometries(document: &DocumentState<&'static str>) -> Vec<Arc<Resolved>> {
    document
        .layout_items
        .iter()
        .filter_map(|item| table_item(item)?.geometry.cloned())
        .collect()
}

fn install(
    document: &Entity<DocumentState<&'static str>>,
    cx: &mut VisualTestContext,
    second: &str,
) {
    let first = format!("{}|end", "first words 中文 ".repeat(180));
    let second = format!("{}|end", second.repeat(300));
    let source = format!("{first}\n\n{second}");
    let start = first.len() + 2;
    let paragraphs = [(0, first.as_str()), (start, second.as_str())]
        .into_iter()
        .map(|(at, row)| {
            DocumentParagraphStyle::new(
                at..at + row.len() + usize::from(at == 0),
                Default::default(),
            )
            .with_table_row(TableRow::new(at).with_cells([
                (0..row.len() - 4, TextAlign::Left),
                (row.len() - 3..row.len(), TextAlign::Right),
            ]))
        })
        .collect();
    reset(document, cx, source, paragraphs);
}

/// One table whose rows are `cell|cell`, separated by newlines.
fn install_rows(
    document: &Entity<DocumentState<&'static str>>,
    cx: &mut VisualTestContext,
    rows: &[String],
) {
    let mut start = 0;
    let paragraphs = rows
        .iter()
        .enumerate()
        .map(|(ix, row)| {
            let end = start + row.len() + usize::from(ix + 1 < rows.len());
            let pipe = row.find('|').unwrap();
            let paragraph = DocumentParagraphStyle::new(start..end, Default::default())
                .with_table_row(TableRow::new(0).with_cells([
                    (0..pipe, TextAlign::Left),
                    (pipe + 1..row.len(), TextAlign::Left),
                ]));
            start = end;
            paragraph
        })
        .collect();
    reset(document, cx, rows.join("\n"), paragraphs);
}

fn reset(
    document: &Entity<DocumentState<&'static str>>,
    cx: &mut VisualTestContext,
    source: String,
    paragraphs: Vec<DocumentParagraphStyle>,
) {
    let length = source.len();
    document.update(cx, |document, cx| {
        document
            .reset(
                DocumentSnapshot::new(
                    source,
                    vec![
                        DocumentRegion::new("history", 0..length, EditPolicy::Readonly),
                        DocumentRegion::new("draft", length..length, EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(length),
                    Vec::new(),
                    DocumentStyles::new(paragraphs, Vec::new()),
                ),
                cx,
            )
            .unwrap();
        document.set_text_rows(Some(TextRows::new()), cx);
    });
}

fn isolated_document(
    cx: &mut TestAppContext,
) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
    let (_, window) = document_view(cx);
    let document = cx.new(|cx| DocumentState::new("", Vec::new(), cx).unwrap());
    (document, window)
}

fn prepare(document: &Entity<DocumentState<&'static str>>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        document.update(cx, |document, cx| {
            document.prepare_tables(
                Bounds::new(point(px(0.), px(0.)), size(px(600.), px(400.))),
                false,
                window,
                cx,
            );
            assert_table_layouts(document);
        });
    });
}

fn assert_table_layouts(document: &DocumentState<&'static str>) {
    for (ix, item) in document
        .layout_items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.is_table_row())
    {
        let DocumentLayoutItem::Text {
            display,
            line,
            layout,
            ..
        } = item
        else {
            unreachable!()
        };
        let layout = layout
            .0
            .as_ref()
            .expect("every table row is published during preparation");
        assert!(matches!(layout, Layout::Table(_)));
        let cells = layout.cells().unwrap();
        assert_eq!(layout.frame().line_height(), cells.geometry.line_height);
        assert_eq!(cells.input.viewport, cells.geometry.viewport);
        assert_eq!(
            document.list_state.item_height(ix),
            Some(
                layout
                    .frame()
                    .row_height(display, line, item.presentation())
            )
        );
    }
}

#[gpui::test]
fn table_layouts_stay_complete_through_style_resize_and_background_publication(
    cx: &mut TestAppContext,
) {
    let (document, _) = isolated_document(cx);
    let (root, cx) = cx.add_window_view(|_, _| Root {
        document: document.clone(),
        family: "first",
        font_size: px(14.),
        width: px(600.),
    });
    install_rows(
        &document,
        cx,
        &[format!("{}|value", "words 中文 ".repeat(500))],
    );
    document.update(cx, |document, cx| {
        document
            .set_node_inline_styles(
                &"history",
                vec![DocumentInlineStyle::new(0..5, Default::default()).font_scale(90)],
                cx,
            )
            .unwrap();
    });
    for (family, font_size, rem, width, scale) in [
        ("first", 14., 16., 600., 1.),
        ("second", 14., 16., 600., 1.5),
        ("second", 20., 24., 120., 2.),
        ("first", 14., 16., 400., 1.),
    ] {
        root.update(cx, |root, cx| {
            root.family = family;
            root.font_size = px(font_size);
            root.width = px(width);
            cx.notify();
        });
        cx.simulate_scale_factor_change(scale);
        // Check the placeholder/retained result and the eventual background
        // publication independently, before any unrelated text wrap can help.
        for _ in 0..2 {
            cx.update(|window, cx| {
                window.set_rem_size(px(rem));
                window.refresh();
                window.draw(cx);
                assert_table_layouts(document.read(cx));
            });
            cx.run_until_parked();
        }
        document.read_with(cx, |document, _| {
            assert!(!document.tables.is_pending());
            assert!(geometries(document).iter().all(|row| row.is_measured()));
        });
    }
}

#[gpui::test]
fn unchanged_cells_publish_a_new_host_frame_and_list_height_together(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install_rows(&document, &mut cx, &["cell|value".into()]);
    prepare(&document, &mut cx);
    let (cells, height) = document.read_with(&cx, |document, _| {
        (
            geometries(document)[0].clone(),
            document.list_state.item_height(0).unwrap(),
        )
    });
    document.update(&mut cx, |document, cx| {
        document.set_text_frame_resolver(
            |_, _, _| {
                TextFrame::new()
                    .with_space_before(px(7.))
                    .with_space_after(px(11.))
            },
            cx,
        );
    });
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(
            Arc::ptr_eq(&cells, &geometries(document)[0]),
            "cell geometry should remain reusable"
        );
        assert_eq!(document.list_state.item_height(0), Some(height + px(18.)));
    });
}

#[gpui::test]
fn table_publication_rejects_same_text_with_obsolete_fonts_or_width(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install_rows(&document, &mut cx, &["code|value".into()]);
    prepare(&document, &mut cx);
    let old = document.read_with(&cx, |document, _| geometries(document)[0].clone());
    document.update(&mut cx, |document, cx| {
        document
            .set_node_inline_styles(
                &"history",
                vec![DocumentInlineStyle::new(0..4, Default::default()).font_scale(90)],
                cx,
            )
            .unwrap();
    });
    prepare(&document, &mut cx);
    cx.update(|window, cx| {
        document.update(cx, |document, cx| {
            let current = geometries(document)[0].clone();
            assert_eq!(current.input.text, old.input.text);
            assert_eq!(current.geometry.line_height, old.geometry.line_height);
            document.install_table_rows(vec![(0, old)], px(600.), window, cx);
            assert!(document.tables.stale);
            assert!(Arc::ptr_eq(&current, &geometries(document)[0]));
            // Width is a dependency even when content and fonts still match.
            document.tables.stale = false;
            document.install_table_rows(vec![(0, current.clone())], px(300.), window, cx);
            assert!(document.tables.stale);
            assert!(Arc::ptr_eq(&current, &geometries(document)[0]));
            assert_table_layouts(document);
        });
    });
}

#[gpui::test]
fn changing_a_rows_kind_preserves_text_selection_and_publishes_matching_geometry(
    cx: &mut TestAppContext,
) {
    let (document, mut cx) = document_view(cx);
    let source = "code|value";
    install_rows(&document, &mut cx, &[source.into()]);
    document.update(&mut cx, |document, cx| {
        document.set_selection(1..4, false, cx)
    });
    for tabular in [true, false, true] {
        document.update(&mut cx, |document, cx| {
            let paragraphs = if tabular {
                vec![
                    DocumentParagraphStyle::new(0..source.len(), Default::default())
                        .with_table_row(TableRow::new(0).with_cells([
                            (0..4, TextAlign::Left),
                            (5..source.len(), TextAlign::Right),
                        ])),
                ]
            } else {
                Vec::new()
            };
            document
                .set_rich_presentation(
                    DocumentProjection::identity(source.len()),
                    Vec::new(),
                    DocumentStyles::new(
                        paragraphs,
                        vec![DocumentInlineStyle::new(0..4, Default::default()).font_scale(90)],
                    ),
                    cx,
                )
                .unwrap();
        });
        cx.update(|window, cx| {
            window.draw(cx);
            let document = document.read(cx);
            let DocumentLayoutItem::Text {
                layout,
                display,
                line,
                ..
            } = &document.layout_items[0]
            else {
                unreachable!()
            };
            let layout = layout.0.as_ref().unwrap();
            assert_eq!(matches!(layout, Layout::Table(_)), tabular);
            assert_eq!(document.text(), source);
            assert_eq!(document.selected_range(), 1..4);
            assert_eq!(
                document.list_state.item_height(0),
                Some(layout.frame().row_height(
                    display,
                    line,
                    document.layout_items[0].presentation(),
                ))
            );
        });
    }
}

#[gpui::test]
fn table_results_publish_one_complete_table_per_layout_frame(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install(&document, &mut cx, "second words ");
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(document.tables.pending.is_some());
        assert_eq!(document.tables.work.len(), 1);
        assert!(document.tables.ready.is_none());
    });
    cx.run_until_parked();
    document.read_with(&cx, |document, _| {
        assert!(document.tables.pending.is_none());
        assert_eq!(document.tables.work.len(), 1);
        assert_eq!(document.tables.ready.as_ref().unwrap().rows.len(), 1);
        assert!(
            geometries(document)
                .iter()
                .all(|row| !row.reusable(&row.input))
        );
    });
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(document.tables.work.is_empty());
        assert!(document.tables.pending.is_some());
        assert!(document.tables.ready.is_none());
        let first = &geometries(document)[0];
        assert!(first.reusable(&first.input));
    });
    cx.run_until_parked();
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(!document.tables.is_pending());
        assert!(
            geometries(document)
                .iter()
                .all(|row| row.reusable(&row.input))
        );
    });
}

#[gpui::test]
fn replanning_unfinished_tables_keeps_layout_scheduled(cx: &mut TestAppContext) {
    for completed in [false, true] {
        let (document, mut cx) = isolated_document(cx);
        install(&document, &mut cx, "second words ");
        prepare(&document, &mut cx);
        if completed {
            cx.run_until_parked();
        }
        document.update(&mut cx, |document, _| {
            assert!(geometries(document).iter().all(|row| !row.is_measured()));
            assert_eq!(document.tables.ready.is_some(), completed);
            document.tables.invalidate();
        });
        prepare(&document, &mut cx);
        document.read_with(&cx, |document, _| {
            assert!(
                document.tables.is_pending(),
                "unmeasured placeholders must not settle a table during replanning"
            );
        });
        for _ in 0..3 {
            cx.run_until_parked();
            prepare(&document, &mut cx);
        }
        document.read_with(&cx, |document, _| {
            assert!(!document.tables.is_pending());
            assert!(geometries(document).iter().all(|row| row.is_measured()));
        });
    }
}

#[gpui::test]
fn stale_table_completion_cannot_replace_or_clear_the_current_task(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install(&document, &mut cx, "old words ");
    prepare(&document, &mut cx);
    cx.run_until_parked();
    let old = document.update(&mut cx, |document, _| document.tables.ready.take().unwrap());
    install(&document, &mut cx, "new words ");
    prepare(&document, &mut cx);
    document.update(&mut cx, |document, _| {
        assert_ne!(document.tables.request, old.request);
        assert!(document.tables.pending.is_some());
        assert!(!document.tables.complete(old.request, Some(old.rows)));
        assert!(
            document.tables.pending.is_some(),
            "late completion must not clear the newer task"
        );
        assert!(document.tables.ready.is_none());
    });
    for _ in 0..3 {
        cx.run_until_parked();
        prepare(&document, &mut cx);
    }
    document.read_with(&cx, |document, _| {
        assert!(!document.tables.is_pending());
        assert!(
            geometries(document)
                .iter()
                .all(|row| !row.input.text.contains("old words"))
        );
    });
}

#[gpui::test]
fn leaving_text_rows_discards_a_completed_table_before_publication(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install(&document, &mut cx, "second words ");
    prepare(&document, &mut cx);
    cx.run_until_parked();
    document.update(&mut cx, |document, cx| {
        assert!(
            document.tables.ready.is_some(),
            "the worker must have completed before the mode switch"
        );
        assert_eq!(document.tables.work.len(), 1);
        let cancel = document.tables.cancel.clone();
        document.set_text_rows(None, cx);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(document.tables.ready.is_none());
        assert!(!document.tables.is_pending());
        document.set_text_rows(Some(TextRows::new()), cx);
    });
    for _ in 0..3 {
        prepare(&document, &mut cx);
        cx.run_until_parked();
    }
    document.read_with(&cx, |document, _| assert!(!document.tables.is_pending()));
}

#[gpui::test]
fn unchanged_tables_reuse_geometry_and_do_not_enter_the_work_queue(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    install(&document, &mut cx, "old words ");
    for _ in 0..3 {
        prepare(&document, &mut cx);
        cx.run_until_parked();
    }
    let retained = document.read_with(&cx, |document, _| {
        assert!(!document.tables.is_pending());
        geometries(document)[0].clone()
    });
    install(&document, &mut cx, "new words ");
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(Arc::ptr_eq(&retained, &geometries(document)[0]));
        assert!(
            document.tables.work.is_empty(),
            "only the changed table may be scheduled"
        );
        assert!(document.tables.pending.is_some());
    });
    cx.run_until_parked();
    document.read_with(&cx, |document, _| {
        let ready = document.tables.ready.as_ref().unwrap();
        assert!(ready.rows.iter().all(|(item, _)| *item != 0));
    });
}

#[gpui::test]
fn a_changed_row_keeps_its_text_at_the_previous_column_widths(cx: &mut TestAppContext) {
    let (document, mut cx) = isolated_document(cx);
    let mut rows: Vec<String> = (0..20)
        .map(|ix| format!("cell {ix}|value {ix} words"))
        .collect();
    install_rows(&document, &mut cx, &rows);
    for _ in 0..3 {
        prepare(&document, &mut cx);
        cx.run_until_parked();
    }
    let widths = document.read_with(&cx, |document, _| {
        assert!(!document.tables.is_pending());
        geometries(document)[0]
            .geometry
            .cells
            .iter()
            .map(|cell| cell.width)
            .collect::<Vec<_>>()
    });
    // Streaming extends the last row and starts another.
    rows.last_mut().unwrap().push_str(" streamed");
    rows.push("next|row".into());
    install_rows(&document, &mut cx, &rows);
    prepare(&document, &mut cx);
    document.read_with(&cx, |document, _| {
        assert!(
            document.tables.pending.is_some(),
            "the table exceeds the synchronous budget"
        );
        let geometries = geometries(document);
        assert_eq!(geometries.len(), rows.len());
        let provisional = geometries
            .iter()
            .filter(|row| row.provisional)
            .collect::<Vec<_>>();
        assert_eq!(provisional.len(), 2);
        for row in provisional {
            let cells = &row.geometry.cells;
            assert!(
                cells
                    .iter()
                    .map(|cell| cell.width)
                    .eq(widths.iter().copied())
            );
            assert!(
                cells.iter().all(|cell| !cell.lines[0].range.is_empty()),
                "changed rows must not paint as empty placeholders"
            );
        }
    });
    for _ in 0..3 {
        cx.run_until_parked();
        prepare(&document, &mut cx);
    }
    document.read_with(&cx, |document, _| {
        assert!(!document.tables.is_pending());
        assert!(
            geometries(document)
                .iter()
                .all(|row| !row.provisional && row.reusable(&row.input))
        );
    });
}
