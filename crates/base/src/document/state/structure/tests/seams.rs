use super::snapshot;
use crate::document::state::{DocumentModel, DocumentStructure};
use crate::document::{
    Affinity, AnchoredBlock, AnchoredBlockError, DocumentPosition, DocumentRegion, EditPolicy,
    HostTransactionError, PresentationError,
};
use gpui::{IntoElement, TestAppContext, div};

#[gpui::test]
fn node_suffix_rejects_routed_nodes_even_when_they_are_empty(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document
            .reset(
                snapshot(
                    "",
                    vec![DocumentRegion::new("draft", 0..0, EditPolicy::Editable)],
                ),
                cx,
            )
            .unwrap();
        let revision = document.revision();
        let undo = document.undo_checkpoint();
        let replacement = snapshot(
            "",
            vec![
                DocumentRegion::new("external", 0..0, EditPolicy::Routed),
                DocumentRegion::new("next", 0..0, EditPolicy::Editable),
            ],
        );
        assert!(
            document
                .apply_structure_splice(revision, &"draft", replacement, cx)
                .is_err()
        );
        assert_eq!(document.revision(), revision);
        assert_eq!(document.undo_checkpoint(), undo);
        assert_eq!(document.regions().len(), 1);
        assert_eq!(document.regions()[0].id(), &"draft");
    });
}

#[test]
fn legacy_structure_suffix_does_not_retain_an_empty_readonly_prefix() {
    // A send freezes its trailing empty text and creates a new draft at
    // the same offset. Only the latter belongs to subsequent user undo.
    let before = DocumentModel::new(
        "sent",
        vec![
            DocumentRegion::new("sent", 0..4, EditPolicy::Readonly),
            DocumentRegion::new("sent-tail", 4..4, EditPolicy::Readonly),
            DocumentRegion::new("draft", 4..4, EditPolicy::Editable),
        ],
    )
    .unwrap();
    let accepted = DocumentModel::new(
        "",
        vec![DocumentRegion::new("draft", 0..0, EditPolicy::Editable)],
    )
    .unwrap();
    assert_eq!(
        DocumentStructure::suffix(&before, 4),
        DocumentStructure::suffix(&accepted, 0)
    );
}

#[gpui::test]
fn fragment_anchors_must_remain_line_boundaries_after_joining(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
        document
            .reset(
                snapshot(
                    "abc",
                    vec![
                        DocumentRegion::new("prefix", 0..1, EditPolicy::Readonly),
                        DocumentRegion::new("middle", 1..2, EditPolicy::Readonly),
                        DocumentRegion::new("suffix", 2..3, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 3..3, EditPolicy::Editable),
                    ],
                ),
                cx,
            )
            .unwrap();
        let revision = document.revision();
        for offset in [0, 1] {
            let replacement = snapshot(
                "x",
                vec![DocumentRegion::new("new", 0..1, EditPolicy::Readonly)],
            )
            .anchored_blocks(vec![AnchoredBlock::new(
                "annotation",
                DocumentPosition::new("new", offset, Affinity::Before),
            )]);
            assert!(
                document
                    .apply_host_node_splice(
                        revision,
                        Some(&"middle"),
                        Some(&"suffix"),
                        replacement,
                        cx
                    )
                    .is_err()
            );
            assert_eq!(document.text(), "abc");
            assert_eq!(document.revision(), revision);
        }
        let replacement = snapshot(
            "x\n",
            vec![DocumentRegion::new("new", 0..2, EditPolicy::Readonly)],
        )
        .anchored_blocks(vec![AnchoredBlock::new(
            "annotation",
            DocumentPosition::new("new", 2, Affinity::Before),
        )]);
        document
            .apply_host_node_splice(revision, Some(&"middle"), Some(&"suffix"), replacement, cx)
            .unwrap();
        assert_eq!(document.text(), "ax\nc");
        assert_eq!(
            document.model.layout_items(None),
            document.layout_items.iter().cloned().collect::<Vec<_>>()
        );
    });
}

#[gpui::test]
fn appending_text_rejects_a_retained_anchor_at_the_old_document_end(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
        document
            .reset(
                snapshot(
                    "a",
                    vec![DocumentRegion::new("prefix", 0..1, EditPolicy::Readonly)],
                )
                .anchored_blocks(vec![AnchoredBlock::new(
                    "annotation",
                    DocumentPosition::new("prefix", 1, Affinity::Before),
                )]),
                cx,
            )
            .unwrap();
        let revision = document.revision();
        assert!(
            document
                .apply_host_node_splice(
                    revision,
                    None,
                    None,
                    snapshot(
                        "b",
                        vec![DocumentRegion::new("new", 0..1, EditPolicy::Readonly),]
                    ),
                    cx
                )
                .is_err()
        );
        assert_eq!(document.text(), "a");
        assert_eq!(document.revision(), revision);
    });
}

#[gpui::test]
fn appended_nodes_share_the_last_line_layout(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        for regions in [
            Vec::new(),
            vec![DocumentRegion::new("prefix", 0..1, EditPolicy::Readonly)],
        ] {
            document.reset(snapshot("a", regions), cx).unwrap();
            document
                .apply_host_node_splice(
                    document.revision(),
                    None,
                    None,
                    snapshot(
                        "b",
                        vec![DocumentRegion::new("new", 0..1, EditPolicy::Readonly)],
                    ),
                    cx,
                )
                .unwrap();
            assert_eq!(document.text(), "ab");
            assert_eq!(
                document.model.layout_items(None),
                document.layout_items.iter().cloned().collect::<Vec<_>>()
            );
        }
    });
}

#[gpui::test]
fn deleting_a_line_break_rejects_a_retained_anchor_before_mutation(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
        document
            .reset(
                snapshot(
                    "a\nb",
                    vec![
                        DocumentRegion::new("prefix", 0..1, EditPolicy::Readonly),
                        DocumentRegion::new("separator", 1..2, EditPolicy::Readonly),
                        DocumentRegion::new("retained", 2..3, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 3..3, EditPolicy::Editable),
                    ],
                )
                .anchored_blocks(vec![AnchoredBlock::new(
                    "annotation",
                    DocumentPosition::new("retained", 0, Affinity::Before),
                )]),
                cx,
            )
            .unwrap();
        let revision = document.revision();
        let selection = document.selected_positions().unwrap();
        let checkpoint = document.undo_checkpoint();
        let layout = document.layout_items.iter().cloned().collect::<Vec<_>>();
        assert_eq!(
            document.apply_host_node_splice(
                revision,
                Some(&"separator"),
                Some(&"retained"),
                snapshot("", vec![]),
                cx
            ),
            Err(HostTransactionError::InvalidFragment(
                PresentationError::AnchoredBlocks(AnchoredBlockError::NotLineBoundary)
            ))
        );
        assert_eq!(document.text(), "a\nb");
        assert_eq!(document.revision(), revision);
        assert_eq!(document.selected_positions().unwrap(), selection);
        assert_eq!(document.undo_checkpoint(), checkpoint);
        assert_eq!(
            document.layout_items.iter().cloned().collect::<Vec<_>>(),
            layout
        );
    });
}

#[gpui::test]
fn node_suffix_undo_and_redo_preserve_empty_nodes_at_the_same_offset(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document
            .reset(
                snapshot(
                    "",
                    vec![
                        DocumentRegion::new("retained", 0..0, EditPolicy::Editable),
                        DocumentRegion::new("draft", 0..0, EditPolicy::Editable),
                    ],
                )
                .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                cx,
            )
            .unwrap();
        let before = || {
            snapshot(
                "",
                vec![DocumentRegion::new("draft", 0..0, EditPolicy::Editable)],
            )
            .selection(DocumentPosition::new("draft", 0, Affinity::After))
        };
        let after = || {
            snapshot(
                "x",
                vec![DocumentRegion::new("next", 0..1, EditPolicy::Editable)],
            )
            .selection(DocumentPosition::new("next", 1, Affinity::After))
        };
        document
            .apply_structure_splice(document.revision(), &"draft", after(), cx)
            .unwrap();
        for _ in 0..2 {
            document
                .apply_structure_splice_replay(document.revision(), &"next", before(), true, cx)
                .unwrap();
            assert_eq!(document.text(), "");
            assert_eq!(
                document
                    .regions()
                    .iter()
                    .map(|r| *r.id())
                    .collect::<Vec<_>>(),
                vec!["retained", "draft"]
            );
            document
                .apply_structure_splice_replay(document.revision(), &"draft", after(), false, cx)
                .unwrap();
            assert_eq!(document.text(), "x");
            assert_eq!(
                document
                    .regions()
                    .iter()
                    .map(|r| *r.id())
                    .collect::<Vec<_>>(),
                vec!["retained", "next"]
            );
        }
    });
}
