mod scaling;
mod seams;

use crate::document::{
    Affinity, DocumentPosition, DocumentProjection, DocumentRegion, DocumentSnapshot,
    DocumentStyles, EditPolicy,
};
use gpui::TestAppContext;

#[derive(Debug, Default)]
pub(in crate::document::state) struct Work {
    pub source_bytes: usize,
    pub layout_bytes: usize,
    pub remeasured_rows: usize,
    pub wrap_bytes: usize,
    pub anchor_checks: usize,
    pub compacted_anchors: usize,
}

thread_local! {
    static WORK: std::cell::RefCell<Option<Work>> = const { std::cell::RefCell::new(None) };
}

pub(in crate::document::state) fn record(f: impl FnOnce(&mut Work)) {
    WORK.with_borrow_mut(|work| {
        if let Some(work) = work {
            f(work);
        }
    });
}

#[gpui::test]
fn fixed_suffix_work_and_following_frames_do_not_revisit_history(cx: &mut TestAppContext) {
    use crate::document::TextRows;
    let (document, mut view) = super::super::tests::document_view(cx);
    for count in [0, 128, 4096] {
        let prefix = "history\n".repeat(count);
        document.update(&mut view, |document, cx| {
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .reset(
                    snapshot(
                        &format!("{prefix}draft"),
                        vec![
                            DocumentRegion::new("history", 0..prefix.len(), EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                prefix.len()..prefix.len() + 5,
                                EditPolicy::Editable,
                            ),
                        ],
                    ),
                    cx,
                )
                .unwrap();
        });
        for _ in 0..4 {
            view.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            view.run_until_parked();
        }
        WORK.with_borrow_mut(|work| *work = Some(Work::default()));
        document.update(&mut view, |document, cx| {
            document
                .apply_structure_splice(
                    document.revision(),
                    &"draft",
                    snapshot(
                        "draft",
                        vec![
                            DocumentRegion::new("sent", 0..5, EditPolicy::Editable),
                            DocumentRegion::new("next", 5..5, EditPolicy::Editable),
                        ],
                    )
                    .selection(DocumentPosition::new(
                        "next",
                        0,
                        Affinity::After,
                    )),
                    cx,
                )
                .unwrap();
            document
                .set_node_policies(document.revision(), &[("sent", EditPolicy::Readonly)], cx)
                .unwrap();
            document.remeasure_node(&"next", cx);
        });
        for _ in 0..4 {
            view.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            view.run_until_parked();
        }
        let work = WORK.with_borrow_mut(|work| work.take().unwrap());
        assert!(
            work.source_bytes < 128
                && work.layout_bytes < 128
                && work.remeasured_rows < 16
                && work.wrap_bytes < 128,
            "{count} history lines: {work:?}"
        );
        WORK.with_borrow_mut(|work| *work = Some(Work::default()));
        document.update(&mut view, |document, cx| {
            document
                .apply_host_node_splice(
                    document.revision(),
                    Some(&"sent"),
                    Some(&"next"),
                    snapshot("", vec![]),
                    cx,
                )
                .unwrap();
            document.remeasure_node(&"next", cx);
            assert_eq!(document.region_text(&"history").unwrap(), prefix);
        });
        for _ in 0..4 {
            view.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            view.run_until_parked();
        }
        let work = WORK.with_borrow_mut(|work| work.take().unwrap());
        assert!(
            work.source_bytes < 128
                && work.layout_bytes < 128
                && work.remeasured_rows < 16
                && work.wrap_bytes < 128,
            "accept after {count} history lines: {work:?}"
        );
    }
}

fn snapshot(
    text: &str,
    regions: Vec<DocumentRegion<&'static str>>,
) -> DocumentSnapshot<&'static str> {
    DocumentSnapshot::new(
        text,
        regions,
        DocumentProjection::identity(text.len()),
        Vec::new(),
        DocumentStyles::default(),
    )
}

#[gpui::test]
fn suffix_splice_and_acceptance_share_a_line_without_replacing_history(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document
            .apply_structure_splice(
                document.revision(),
                &"draft",
                snapshot(
                    "中🙂",
                    vec![
                        DocumentRegion::new("sent", 0..7, EditPolicy::Editable),
                        DocumentRegion::new("next", 7..7, EditPolicy::Editable),
                    ],
                )
                .selection(DocumentPosition::new("next", 0, Affinity::After)),
                cx,
            )
            .unwrap();
        assert_eq!(document.text(), "history中🙂");
        assert_eq!(document.region_text(&"history").unwrap(), "history");
        assert_eq!(document.region_text(&"sent").unwrap(), "中🙂");
        assert_eq!(document.selected_positions().unwrap().1.node_id(), &"next");
        document
            .set_node_policies(document.revision(), &[("sent", EditPolicy::Readonly)], cx)
            .unwrap();
        let checkpoint = document.undo_checkpoint();
        document
            .apply_host_node_splice(
                document.revision(),
                Some(&"sent"),
                Some(&"next"),
                snapshot("", vec![]),
                cx,
            )
            .unwrap();
        assert_eq!(document.text(), "history");
        assert!(document.region(&"sent").is_none());
        assert_eq!(document.selected_positions().unwrap().1.node_id(), &"next");
        assert_eq!(document.undo_checkpoint(), checkpoint);
        assert_eq!(
            document.model.layout_items(None),
            document.layout_items.iter().cloned().collect::<Vec<_>>()
        );
    });
}

#[gpui::test]
fn rejected_local_structure_leaves_text_selection_revision_and_undo_unchanged(
    cx: &mut TestAppContext,
) {
    let (document, mut view) = super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        let revision = document.revision();
        let selection = document.selected_positions().unwrap();
        let checkpoint = document.undo_checkpoint();
        let duplicate = snapshot(
            "x",
            vec![DocumentRegion::new("history", 0..1, EditPolicy::Editable)],
        )
        .selection(DocumentPosition::new("history", 1, Affinity::After));
        assert!(
            document
                .apply_structure_splice(revision, &"draft", duplicate, cx)
                .is_err()
        );
        assert_eq!(document.text(), "history");
        assert_eq!(document.revision(), revision);
        assert_eq!(document.selected_positions().unwrap(), selection);
        assert_eq!(document.undo_checkpoint(), checkpoint);
        let invalid_selection = snapshot(
            "x",
            vec![DocumentRegion::new("new", 0..1, EditPolicy::Editable)],
        )
        .selection(DocumentPosition::new("absent", 0, Affinity::After));
        assert!(
            document
                .apply_structure_splice(revision, &"draft", invalid_selection, cx)
                .is_err()
        );
        assert_eq!(document.text(), "history");
        assert_eq!(document.undo_checkpoint(), checkpoint);
        assert!(
            document
                .apply_host_splice(
                    revision,
                    None,
                    None,
                    snapshot(
                        "x",
                        vec![DocumentRegion::new("later", 0..1, EditPolicy::Readonly),]
                    ),
                    cx
                )
                .is_err()
        );
        assert_eq!(document.text(), "history");
        assert_eq!(document.revision(), revision);
    });
}

#[gpui::test]
fn permissions_reject_invalid_batches_before_changing_any_node(cx: &mut TestAppContext) {
    let (document, mut view) = super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        let revision = document.revision();
        assert!(
            document
                .set_node_policies(
                    revision,
                    &[
                        ("draft", EditPolicy::Readonly),
                        ("absent", EditPolicy::Editable)
                    ],
                    cx
                )
                .is_err()
        );
        assert_eq!(
            document.region(&"draft").unwrap().policy(),
            EditPolicy::Editable
        );
        assert!(
            document
                .set_node_policies(
                    revision,
                    &[
                        ("draft", EditPolicy::Readonly),
                        ("draft", EditPolicy::Editable),
                    ],
                    cx
                )
                .is_err()
        );
        assert_eq!(
            document.region(&"draft").unwrap().policy(),
            EditPolicy::Editable
        );
        document
            .set_node_policies(revision, &[("draft", EditPolicy::Readonly)], cx)
            .unwrap();
        document
            .set_node_policies(revision, &[("draft", EditPolicy::Editable)], cx)
            .unwrap();
        assert_eq!(document.revision(), revision);
        assert_eq!(
            document.region(&"draft").unwrap().policy(),
            EditPolicy::Editable
        );
    });
}

#[gpui::test]
fn accepting_a_same_line_node_preserves_later_composition_and_undo(cx: &mut TestAppContext) {
    use crate::input::{Redo, Undo};
    use gpui::EntityInputHandler;
    let (document, mut view) = super::super::tests::document_view(cx);
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            document
                .apply_structure_splice(
                    document.revision(),
                    &"draft",
                    snapshot(
                        "sent",
                        vec![
                            DocumentRegion::new("sent", 0..4, EditPolicy::Editable),
                            DocumentRegion::new("next", 4..4, EditPolicy::Editable),
                        ],
                    )
                    .selection(DocumentPosition::new(
                        "next",
                        0,
                        Affinity::After,
                    )),
                    cx,
                )
                .unwrap();
            document
                .set_node_policies(document.revision(), &[("sent", EditPolicy::Readonly)], cx)
                .unwrap();
            document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            let sequence = document.undo.next_sequence;
            document
                .apply_host_node_splice(
                    document.revision(),
                    Some(&"sent"),
                    Some(&"next"),
                    snapshot("", vec![]),
                    cx,
                )
                .unwrap();
            assert_eq!(document.text(), "historyni");
            assert_eq!(document.marked_text_range(window, cx), Some(7..9));
            assert_eq!(document.selected_range(), 9..9);
            assert_eq!(document.undo.next_sequence, sequence);
            document.replace_text_in_range(None, "你", window, cx);
            assert_eq!(document.text(), "history你");
            document.undo(&Undo, window, cx);
            assert_eq!(document.text(), "history");
            document.redo(&Redo, window, cx);
            assert_eq!(document.text(), "history你");
        })
    });
}
