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
