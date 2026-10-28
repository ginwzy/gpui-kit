use crate::document::{
    DocumentParagraphStyle, DocumentProjection, DocumentRegion, DocumentSnapshot, DocumentState,
    DocumentStyles, EditPolicy, TableRow, TextRows, state::tests::document_view,
};
use gpui::{
    AppContext, Bounds, Entity, TestAppContext, TextAlign, VisualTestContext, point, px, size,
};
use std::sync::{Arc, atomic::Ordering};

use super::{Resolved, table_item};

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
            )
        });
    });
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
