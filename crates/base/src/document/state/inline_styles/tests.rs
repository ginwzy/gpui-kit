use super::*;
use gpui::{FontWeight, TestAppContext};

fn emphasis(range: Range<usize>) -> DocumentInlineStyle {
    DocumentInlineStyle::new(
        range,
        HighlightStyle {
            font_weight: Some(FontWeight::MEDIUM),
            ..Default::default()
        },
    )
}

#[gpui::test]
fn node_styles_preserve_input_state_and_only_remeasure_changed_text(cx: &mut TestAppContext) {
    let (document, mut cx) = super::super::tests::document_view(cx);
    cx.update(|window, cx| {
        document.update(cx, |doc, cx| {
            let source = "head\n中文 arg";
            doc.reset(
                DocumentSnapshot::new(
                    source,
                    vec![
                        DocumentRegion::new("history", 0..5, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 5..source.len(), EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(source.len()),
                    vec![],
                    DocumentStyles::new(vec![], vec![emphasis(0..4)]),
                ),
                cx,
            )
            .unwrap();
            doc.set_selection(source.len()..source.len(), false, cx);
            doc.replace_text_in_range(None, "!", window, cx);
            let revision = doc.revision();
            let selection = doc.selected_positions().unwrap();
            let undo_count = doc.undo.undo.len();
            let history = doc.layout_items[0].clone();
            assert!(
                doc.set_node_inline_styles(&"draft", vec![emphasis(0..6)], cx)
                    .unwrap()
            );
            assert!(
                !doc.set_node_inline_styles(&"draft", vec![emphasis(0..6)], cx)
                    .unwrap()
            );
            assert_eq!(doc.revision(), revision);
            assert_eq!(doc.selected_positions().unwrap(), selection);
            assert_eq!(doc.undo.undo.len(), undo_count);
            assert_eq!(doc.layout_items[0], history);
            let DocumentLayoutItem::Text { presentation, .. } = &doc.layout_items[1] else {
                panic!("draft text")
            };
            let presentation = presentation.as_deref().unwrap();
            assert_eq!(presentation.highlights[0].0, 0..6);
            doc.replace_and_mark_text_in_range(None, "你", Some(1..1), window, cx);
            let marked = doc.marked_text_range(window, cx);
            let selection = doc.selected_positions().unwrap();
            doc.set_node_inline_styles(&"draft", vec![], cx).unwrap();
            assert_eq!(doc.marked_text_range(window, cx), marked);
            assert_eq!(doc.selected_positions().unwrap(), selection);
            doc.replace_text_in_range(None, "你", window, cx);
            doc.undo(&Undo, window, cx);
            assert_eq!(doc.text(), format!("{source}!"));
            doc.undo(&Undo, window, cx);
            assert_eq!(doc.text(), source);
            assert_eq!(doc.styles().inline(), &[emphasis(0..4)]);
        })
    });
}

#[gpui::test]
fn invalid_node_styles_are_atomic_and_deleted_styles_can_be_cleared(cx: &mut TestAppContext) {
    let (document, mut cx) = super::super::tests::document_view(cx);
    cx.update(|window, cx| {
        document.update(cx, |doc, cx| {
            doc.replace_text_in_range(None, "中文", window, cx);
            doc.set_node_inline_styles(&"draft", vec![emphasis(0..6)], cx)
                .unwrap();
            let styles = doc.styles().clone();
            for invalid in [
                vec![emphasis(0..1)],
                vec![emphasis(0..7)],
                vec![emphasis(0..6), emphasis(3..6)],
            ] {
                assert!(doc.set_node_inline_styles(&"draft", invalid, cx).is_err());
                assert_eq!(doc.styles(), &styles);
            }
            assert_eq!(
                doc.set_node_inline_styles(&"missing", vec![], cx),
                Err(DocumentStyleError::UnknownNode)
            );
            doc.set_selection(7..13, false, cx);
            doc.replace_text_in_range(None, "replacement", window, cx);
            assert_eq!(doc.text(), "historyreplacement");
            assert!(doc.styles().inline().is_empty());
            doc.set_node_inline_styles(&"draft", vec![emphasis(0..11)], cx)
                .unwrap();
            doc.set_selection(7..18, false, cx);
            doc.replace_text_in_range(None, "", window, cx);
            doc.set_node_inline_styles(&"draft", vec![], cx).unwrap();
            doc.replace_text_in_range(None, "plain", window, cx);
            assert!(doc.styles().inline().is_empty());
            assert_eq!(doc.text(), "historyplain");
        })
    });
}

#[gpui::test]
fn range_styles_preserve_other_styles_and_match_full_layout(cx: &mut TestAppContext) {
    let (document, mut cx) = super::super::tests::document_view(cx);
    cx.update(|window, cx| {
        document.update(cx, |doc, cx| {
            doc.replace_text_in_range(None, "甲 alpha\nbeta 🙂", window, cx);
            let original = vec![
                emphasis(0..3),
                emphasis(4..9),
                emphasis(10..14),
                emphasis(15..19),
            ];
            doc.set_node_inline_styles(&"draft", original, cx).unwrap();
            doc.replace_and_mark_text_in_range(None, "你", Some(1..1), window, cx);
            let revision = doc.revision();
            let selection = doc.selected_positions().unwrap();
            let marked = doc.marked_text_range(window, cx);
            let undo = doc.undo.undo.len();
            let mut changed = emphasis(4..9);
            changed = DocumentInlineStyle::new(
                changed.source(),
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            );
            assert!(
                doc.set_node_inline_styles_in_range(&"draft", 4..9, vec![changed.clone()], cx)
                    .unwrap()
            );
            assert!(
                !doc.set_node_inline_styles_in_range(&"draft", 4..9, vec![changed.clone()], cx)
                    .unwrap()
            );
            doc.set_node_inline_styles_in_range(&"draft", 10..14, vec![], cx)
                .unwrap();
            assert_eq!(doc.revision(), revision);
            assert_eq!(doc.selected_positions().unwrap(), selection);
            assert_eq!(doc.marked_text_range(window, cx), marked);
            assert_eq!(doc.undo.undo.len(), undo);
            // Compare the incremental path to the independent full presentation builder.
            let layout = doc.layout_items.clone();
            let styles = doc.styles().clone();
            doc.set_rich_presentation(
                DocumentProjection::identity(doc.text().len()),
                vec![],
                styles.clone(),
                cx,
            )
            .unwrap();
            assert_eq!(doc.layout_items, layout);
            let start = doc.region(&"draft").unwrap().range().start;
            assert_eq!(
                doc.styles().inline(),
                &[
                    emphasis(start..start + 3),
                    changed.shifted(start),
                    emphasis(start + 15..start + 19)
                ]
            );
            // Invalid UTF-8, crossing an existing style, out-of-range replacement,
            // and a style outside its patch all leave the model and layout unchanged.
            for (range, replacement) in [
                (1..3, vec![]),
                (4..8, vec![]),
                (0..100, vec![]),
                (4..9, vec![emphasis(10..14)]),
            ] {
                assert!(
                    doc.set_node_inline_styles_in_range(&"draft", range, replacement, cx)
                        .is_err()
                );
                assert_eq!(doc.styles(), &styles);
                assert_eq!(doc.layout_items, layout);
            }
        })
    });
}

#[gpui::test]
fn range_styles_use_source_identity_when_projection_ranges_overlap(cx: &mut TestAppContext) {
    use crate::document::ProjectionSpan;
    let (document, mut cx) = super::super::tests::document_view(cx);
    document.update(&mut cx, |doc, cx| {
        let projection =
            DocumentProjection::new(8, vec![ProjectionSpan::replace(0..8, "X")]).unwrap();
        doc.reset(
            DocumentSnapshot::new(
                "abcdefgh",
                vec![DocumentRegion::new("source", 0..8, EditPolicy::Readonly)],
                projection.clone(),
                vec![],
                DocumentStyles::new(vec![], vec![emphasis(0..2), emphasis(3..5)]),
            ),
            cx,
        )
        .unwrap();
        assert_eq!(doc.model.resolved_styles.inline.len(), 2);
        assert_eq!(
            doc.model.resolved_styles.inline[0].display,
            doc.model.resolved_styles.inline[1].display
        );
        doc.set_node_inline_styles_in_range(&"source", 3..5, vec![], cx)
            .unwrap();
        let resolved = doc.model.resolved_styles.clone();
        let layout = doc.layout_items.clone();
        assert_eq!(resolved.inline.len(), 1);
        assert_eq!(resolved.inline[0].source, 0..2);
        doc.set_rich_presentation(projection, vec![], doc.styles().clone(), cx)
            .unwrap();
        assert_eq!(doc.model.resolved_styles, resolved);
        assert_eq!(doc.layout_items, layout);
    });
}
