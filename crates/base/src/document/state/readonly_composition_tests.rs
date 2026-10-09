//! Consumer-owned atomic title + readonly source, without an application model.
use super::*;
use crate::document::ProjectionSpan;
use gpui::{Modifiers, MouseButton, TestAppContext, point, px};

fn replay(
    document: &mut DocumentState<&'static str>,
    request: &StructureReplay<&'static str>,
    cx: &mut Context<DocumentState<&'static str>>,
) {
    let start = document.region(request.start_node()).unwrap().range().start;
    let mut text = document.text()[..start].to_owned();
    text.push_str(request.target().text());
    let mut regions = document
        .regions()
        .iter()
        .filter(|region| region.range().start < start)
        .cloned()
        .collect::<Vec<_>>();
    regions.extend(request.target().regions().iter().map(|region| {
        let range = region.range();
        DocumentRegion::new(
            *region.id(),
            start + range.start..start + range.end,
            region.policy(),
        )
    }));
    let mut snapshot = presentation(text, regions);
    if let Some((anchor, head)) = request.selection() {
        snapshot = snapshot.selection_range(anchor.clone(), head.clone());
    }
    document
        .apply_structure_replay(snapshot, start, request.is_undo(), cx)
        .unwrap();
}

fn presentation(
    text: String,
    regions: Vec<DocumentRegion<&'static str>>,
) -> DocumentSnapshot<&'static str> {
    let blocks = regions
        .iter()
        .filter(|region| region.policy() == EditPolicy::Atomic)
        .map(|region| DocumentBlock::new(*region.id(), region.range()))
        .collect::<Vec<_>>();
    let projection = DocumentProjection::new(
        text.len(),
        blocks
            .iter()
            .map(|block| ProjectionSpan::hide(block.source()))
            .collect(),
    )
    .unwrap();
    DocumentSnapshot::new(text, regions, projection, blocks, DocumentStyles::default())
}

fn reference(
    history: &str,
    body: &str,
    title_id: &'static str,
    body_id: &'static str,
) -> DocumentSnapshot<&'static str> {
    let title = history.len() + 4;
    let body_start = title + 3;
    let separator = if body.is_empty() || body.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let body_end = body_start + body.len() + separator.len();
    presentation(
        format!("{history}left\u{fffc}{body}{separator}tail"),
        vec![
            DocumentRegion::new("history", 0..history.len(), EditPolicy::Readonly),
            DocumentRegion::new("left", history.len()..title, EditPolicy::Editable),
            DocumentRegion::new(title_id, title..body_start, EditPolicy::Atomic),
            DocumentRegion::new(body_id, body_start..body_end, EditPolicy::Readonly),
            DocumentRegion::new("draft", body_end..body_end + 4, EditPolicy::Editable),
        ],
    )
}

#[gpui::test]
fn readonly_body_participates_in_suffix_structure(cx: &mut TestAppContext) {
    let (document, mut view) = tests::document_view(cx);
    view.update(|_, cx| {
        document.update(cx, |document, cx| {
            document.set_block_renderer(|_, _, _| div().h(px(28.)).into_any_element(), cx);
            document
                .apply_structure_transaction(
                    reference("history", "中🙂\tcode\nlast", "title-a", "body-a")
                        .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                    7,
                    cx,
                )
                .unwrap();
            let UndoKind::Structure { before, after } = &document.undo.undo.last().unwrap().kind
            else {
                panic!("structure record");
            };
            assert_eq!(before.text(), "");
            assert_eq!(after.regions()[2].id(), &"body-a");
            assert_eq!(after.regions()[2].policy(), EditPolicy::Readonly);
            assert!(after.text().contains("中🙂\tcode\nlast\n"));
            assert!(document.retained_structure_ids().contains(&"body-a"));
        })
    });
}

#[gpui::test]
fn host_preview_splice_requires_line_seams_and_preserves_selection_affinity(
    cx: &mut TestAppContext,
) {
    let (document, mut view) = tests::document_view(cx);
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            let fragment = || {
                presentation(
                    "raw\n".into(),
                    vec![DocumentRegion::new("preview", 0..4, EditPolicy::Readonly)],
                )
            };
            assert_eq!(
                document.apply_host_splice(
                    document.revision(),
                    Some(&"draft"),
                    Some(&"draft"),
                    fragment(),
                    cx
                ),
                Err(HostTransactionError::FragmentBoundary)
            );
            document
                .reset(
                    presentation(
                        "history\n@q".into(),
                        vec![
                            DocumentRegion::new("history", 0..8, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 8..10, EditPolicy::Editable),
                        ],
                    )
                    .selection(DocumentPosition::new(
                        "draft",
                        2,
                        Affinity::Before,
                    )),
                    cx,
                )
                .unwrap();
            document.replace_text_in_range(None, "x", window, cx);
            document
                .set_selection_positions(
                    DocumentPosition::new("draft", 0, Affinity::After),
                    DocumentPosition::new("draft", 3, Affinity::Before),
                    cx,
                )
                .unwrap();
            let selection = document.selected_positions().unwrap();
            let checkpoint = document.undo_checkpoint();
            document
                .apply_host_splice(
                    document.revision(),
                    Some(&"draft"),
                    Some(&"draft"),
                    fragment(),
                    cx,
                )
                .unwrap();
            assert_eq!(document.selected_positions().unwrap(), selection);
            assert_eq!(document.undo_checkpoint(), checkpoint);
            document
                .apply_host_splice(
                    document.revision(),
                    Some(&"preview"),
                    Some(&"draft"),
                    presentation(String::new(), vec![]),
                    cx,
                )
                .unwrap();
            assert_eq!(document.selected_positions().unwrap(), selection);
            assert_eq!(document.undo_checkpoint(), checkpoint);
            document.undo(&Undo, window, cx);
            assert_eq!(document.text(), "history\n@q");
        })
    });
}

#[gpui::test]
fn readonly_structure_replays_versions_selection_and_preserves_host_prefix(
    cx: &mut TestAppContext,
) {
    let (document, mut view) = tests::document_view(cx);
    let _replay = view.update(|_, cx| {
        cx.subscribe(&document, |entity, event, cx| {
            if let DocumentEvent::StructureReplayRequested(request) = event {
                entity.update(cx, |document, cx| replay(document, request, cx));
            }
        })
    });
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            document.set_block_renderer(|_, _, _| div().h(px(28.)).into_any_element(), cx);
            document.focus_handle.focus(window, cx);
        })
    });
    view.simulate_keystrokes("q");
    view.update(|_, cx| {
        document.update(cx, |document, cx| {
            document
                .apply_structure_transaction(
                    reference("history", "中🙂\tcode\nlast", "title-a", "body-a")
                        .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                    7,
                    cx,
                )
                .unwrap();
            document
                .set_selection_positions(
                    DocumentPosition::new("body-a", 3, Affinity::After),
                    DocumentPosition::new("body-a", 12, Affinity::Before),
                    cx,
                )
                .unwrap();
            document
                .apply_structure_transaction(
                    reference("history", "🙂\tcode", "title-b", "body-b")
                        .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                    7,
                    cx,
                )
                .unwrap();
            let stale = reference("history", "changed", "title-b", "body-b");
            assert_eq!(
                document.apply_structure_replay(stale, 7, true, cx),
                Err(StructureError::StaleReplay)
            );
            let retained = document.retained_structure_ids();
            for id in ["title-a", "body-a", "title-b", "body-b"] {
                assert!(retained.contains(&id));
            }
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..7, "streamed history")],
                document.text().len(),
            )
            .unwrap();
            let mut regions = document.regions().to_vec();
            regions[0] = DocumentRegion::new("history", 0..16, EditPolicy::Readonly);
            for region in &mut regions[1..] {
                let range = region.range();
                *region = DocumentRegion::new(
                    *region.id(),
                    range.start + 9..range.end + 9,
                    region.policy(),
                );
            }
            let snapshot = presentation(
                document.text().replacen("history", "streamed history", 1),
                regions,
            );
            document
                .apply_host_transaction_with_rich_presentation(
                    transaction,
                    snapshot.regions,
                    snapshot.projection,
                    snapshot.blocks,
                    snapshot.styles,
                    cx,
                )
                .unwrap();
        })
    });
    view.simulate_keystrokes("secondary-z");
    view.run_until_parked();
    document.read_with(&view, |document, _| {
        assert_eq!(
            document.region_text(&"body-a").as_deref(),
            Some("中🙂\tcode\nlast\n")
        );
        assert_eq!(
            document.selected_positions().unwrap(),
            (
                DocumentPosition::new("body-a", 3, Affinity::After),
                DocumentPosition::new("body-a", 12, Affinity::Before)
            )
        );
        assert!(document.text().starts_with("streamed history"));
    });
    view.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    });
    view.run_until_parked();
    document.read_with(&view, |document, _| {
        assert_eq!(
            document.region_text(&"body-b").as_deref(),
            Some("🙂\tcode\n")
        )
    });
    view.simulate_keystrokes("secondary-z");
    view.run_until_parked();
    view.simulate_keystrokes("secondary-z");
    view.run_until_parked();
    document.read_with(&view, |document, _| {
        assert_eq!(document.text(), "streamed historyq")
    });
    view.simulate_keystrokes("secondary-z");
    document.read_with(&view, |document, _| {
        assert_eq!(document.text(), "streamed history")
    });
}

#[gpui::test]
fn readonly_structure_keeps_frozen_prefix_and_input_restrictions(cx: &mut TestAppContext) {
    let (document, mut view) = tests::document_view(cx);
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            document.set_block_renderer(|_, _, _| div().h(px(28.)).into_any_element(), cx);
            document
                .reset(
                    reference("history", "中🙂", "title-a", "body-a")
                        .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                    cx,
                )
                .unwrap();
            let mut regions = document.regions().to_vec();
            regions[1] = DocumentRegion::new("left", regions[1].range(), EditPolicy::Readonly);
            document.set_region_policies(regions, cx).unwrap();
            let start = document.region(&"draft").unwrap().range().start;
            let checkpoint = document.undo_checkpoint();
            let mut snapshot = presentation(document.text(), document.regions().to_vec());
            snapshot.text.push('x');
            snapshot.regions.pop();
            snapshot.regions.push(DocumentRegion::new(
                "draft",
                start..start + 5,
                EditPolicy::Editable,
            ));
            snapshot.projection =
                DocumentProjection::new(snapshot.text.len(), vec![ProjectionSpan::hide(11..14)])
                    .unwrap();
            document
                .apply_structure_transaction(snapshot, start, cx)
                .unwrap();
            let mut changed_prefix = reference("history", "文🙂", "title-a", "body-a");
            changed_prefix.regions[1] = DocumentRegion::new(
                "left",
                changed_prefix.regions[1].range(),
                EditPolicy::Readonly,
            );
            assert_eq!(
                document.apply_structure_transaction(changed_prefix, start, cx),
                Err(StructureError::PrefixChanged)
            );
            let UndoKind::Structure { before, after } = &document.undo.undo.last().unwrap().kind
            else {
                panic!("structure");
            };
            assert_eq!(before.text(), "tail");
            assert_eq!(after.text(), "tailx");
            assert!(!document.retained_structure_ids().contains(&"body-a"));
            document.retire_undo_before(checkpoint);
            let selection = DocumentPosition::new("body-a", 0, Affinity::After);
            document
                .set_selection_positions(selection.clone(), selection, cx)
                .unwrap();
            let original = document.text();
            document.replace_text_in_range(None, "edit", window, cx);
            document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            assert_eq!(document.text(), original);
            assert!(document.model.marked.is_none());
            document
                .set_selection_positions(
                    DocumentPosition::new("draft", 0, Affinity::After),
                    DocumentPosition::new("draft", 0, Affinity::After),
                    cx,
                )
                .unwrap();
            document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            let snapshot = presentation(document.text(), document.regions().to_vec());
            assert_eq!(
                document.apply_structure_transaction(snapshot, start, cx),
                Err(StructureError::MarkedText)
            );
        })
    });
}

#[gpui::test]
fn host_preview_preserves_draft_undo_checkpoint_and_selection_through_stream_and_cancel(
    cx: &mut TestAppContext,
) {
    let (document, mut view) = tests::document_view(cx);
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            document.focus_handle.focus(window, cx);
            document.replace_text_in_range(None, "prefix @q tail", window, cx);
            document
                .set_selection_positions(
                    DocumentPosition::new("draft", 7, Affinity::After),
                    DocumentPosition::new("draft", 9, Affinity::Before),
                    cx,
                )
                .unwrap();
            let selected = document.selected_positions().unwrap();
            let checkpoint = document.undo_checkpoint();
            let stacks = (document.undo.undo.len(), document.undo.redo.len());
            // Insertion within a retained editable node is deliberately prohibited.
            let invalid = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(14..14, "preview")],
                document.text().len(),
            )
            .unwrap();
            assert_eq!(
                document.apply_host_transaction(invalid, document.regions().to_vec(), cx),
                Err(HostTransactionError::TouchesEditableRegion)
            );
            // Place the temporary preview before its owning Text, without splitting it.
            let preview = "preview 中文🙂\t\nlast\n";
            let insert = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(7..7, preview)],
                document.text().len(),
            )
            .unwrap();
            document
                .apply_host_transaction(
                    insert,
                    vec![
                        DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                        DocumentRegion::new("preview", 7..7 + preview.len(), EditPolicy::Readonly),
                        DocumentRegion::new(
                            "draft",
                            7 + preview.len()..21 + preview.len(),
                            EditPolicy::Editable,
                        ),
                    ],
                    cx,
                )
                .unwrap();
            assert_eq!(document.selected_positions().unwrap(), selected);
            assert_eq!(
                document.region_text(&"draft").as_deref(),
                Some("prefix @q tail")
            );
            assert_eq!((document.undo.undo.len(), document.undo.redo.len()), stacks);
            assert_eq!(document.undo_checkpoint(), checkpoint);
            // A selected preview survives unrelated streaming history.
            document
                .set_selection_positions(
                    DocumentPosition::new("preview", 8, Affinity::After),
                    DocumentPosition::new("preview", 18, Affinity::Before),
                    cx,
                )
                .unwrap();
            let selected_preview = document.selected_positions().unwrap();
            let stream = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..7, "history stream")],
                document.text().len(),
            )
            .unwrap();
            document
                .apply_host_transaction(
                    stream,
                    vec![
                        DocumentRegion::new("history", 0..14, EditPolicy::Readonly),
                        DocumentRegion::new(
                            "preview",
                            14..14 + preview.len(),
                            EditPolicy::Readonly,
                        ),
                        DocumentRegion::new(
                            "draft",
                            14 + preview.len()..28 + preview.len(),
                            EditPolicy::Editable,
                        ),
                    ],
                    cx,
                )
                .unwrap();
            assert_eq!(document.selected_positions().unwrap(), selected_preview);
            let remove = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(14..14 + preview.len(), "")],
                document.text().len(),
            )
            .unwrap();
            document
                .apply_host_transaction(
                    remove,
                    vec![
                        DocumentRegion::new("history", 0..14, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 14..28, EditPolicy::Editable),
                    ],
                    cx,
                )
                .unwrap();
            // The preview owner returns to the original query, using Base positions.
            document
                .set_selection_positions(selected.0.clone(), selected.1.clone(), cx)
                .unwrap();
            assert_eq!(document.text(), "history streamprefix @q tail");
            assert_eq!(document.undo_checkpoint(), checkpoint);
            assert_eq!((document.undo.undo.len(), document.undo.redo.len()), stacks);
            document.undo(&Undo, window, cx);
            assert_eq!(document.text(), "history stream");
            document.redo(&Redo, window, cx);
            assert_eq!(document.text(), "history streamprefix @q tail");
            // Confirmation is a single original-draft User transaction, after removal.
            let records = document.undo.undo.len();
            document.set_block_renderer(|_, _, _| div().h(px(28.)).into_any_element(), cx);
            document
                .apply_structure_transaction(
                    reference("history stream", "中文🙂", "title-a", "body-a")
                        .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                    14,
                    cx,
                )
                .unwrap();
            assert_eq!(document.undo.undo.len(), records + 1);
            assert!(!document.retained_structure_ids().contains(&"preview"));
            let UndoKind::Structure { before, .. } = &document.undo.undo.last().unwrap().kind
            else {
                panic!("structure");
            };
            assert_eq!(before.text(), "prefix @q tail");
        })
    });
}

#[gpui::test]
fn readonly_body_pointer_keyboard_copy_gutter_and_hidden_projection(cx: &mut TestAppContext) {
    for body in [
        "",
        "半行🙂\tend",
        "半行🙂\tend\nnext",
        "半行🙂\tend\n",
        "long ".repeat(25).as_str(),
    ] {
        let (document, mut view) = tests::document_view(cx);
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_block_renderer(|_, _, _| div().h(px(28.)).into_any_element(), cx);
                // A gutter is presentation alongside the original DocumentElement text.
                let original = body.to_owned();
                document.set_text_renderer(
                    move |context, text, _, _| {
                        let label = if context.node_id() == &"body-a" {
                            let offset = context.source_in_node().start.min(original.len());
                            (original[..offset]
                                .bytes()
                                .filter(|byte| *byte == b'\n')
                                .count()
                                + 1)
                            .to_string()
                        } else {
                            String::new()
                        };
                        div()
                            .flex()
                            .w(px(220.))
                            .child(div().w(px(32.)).child(label))
                            .child(div().flex_1().min_w_0().child(text))
                            .into_any_element()
                    },
                    cx,
                );
                document
                    .reset(
                        reference("history", body, "title-a", "body-a")
                            .selection(DocumentPosition::new("draft", 0, Affinity::After)),
                        cx,
                    )
                    .unwrap();
                document.focus_handle.focus(window, cx);
            })
        });
        view.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&view, |document, _| {
            if body.is_empty() {
                assert!(!document.layout_items.iter().any(|item| matches!(
                    item,
                    DocumentLayoutItem::Text {
                        node_id: Some("body-a"),
                        ..
                    }
                )));
            }
            if body.len() > 100 {
                let display = document
                    .model
                    .source_to_display(
                        document.region(&"body-a").unwrap().range().start,
                        Affinity::After,
                    )
                    .unwrap();
                let line = document
                    .text_layouts
                    .iter()
                    .find(|record| record.display.start == display)
                    .unwrap();
                assert!(
                    !line
                        .layout
                        .line_layout_for_index(0)
                        .unwrap()
                        .wrap_boundaries
                        .is_empty()
                );
            }
        });
        if !body.is_empty() {
            let (start, end) = document.read_with(&view, |document, _| {
                let region = document.region(&"body-a").unwrap().range();
                let display = document
                    .model
                    .source_to_display(region.start, Affinity::After)
                    .unwrap();
                let line = document
                    .text_layouts
                    .iter()
                    .find(|record| record.display.start == display)
                    .unwrap();
                let origin = line.layout.position_for_index(0).unwrap();
                let end = line.layout.position_for_index(3.min(body.len())).unwrap();
                (
                    point(origin.x, origin.y + line.layout.line_height() / 2.),
                    point(end.x, end.y + line.layout.line_height() / 2.),
                )
            });
            view.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            view.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
            view.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
            document.read_with(&view, |document, _| {
                assert!(!document.selected_range().is_empty())
            });
            view.simulate_keystrokes("shift-right secondary-c");
            view.update(|_, cx| {
                let state = document.read(cx);
                let range = state.selected_range();
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    state.text()[range].to_owned()
                );
            });
        }
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                let source = document.text();
                let title = document.region(&"title-a").unwrap().range();
                let body_range = document.region(&"body-a").unwrap().range();
                document.set_selection(title.start..body_range.end, false, cx);
                document.copy(&Copy, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    source[title.start..body_range.end]
                );
                // Base Copy intentionally exports source, including hidden atomic source.
                assert!(
                    cx.read_from_clipboard()
                        .unwrap()
                        .text()
                        .unwrap()
                        .contains('\u{fffc}')
                );
                let checkpoint = document.undo_checkpoint();
                let visible = DocumentPosition::new("title-a", 0, Affinity::After);
                document
                    .set_selection_positions(visible.clone(), visible, cx)
                    .unwrap();
                let mut spans = vec![ProjectionSpan::hide(title)];
                if !body_range.is_empty() {
                    spans.push(ProjectionSpan::hide(body_range));
                }
                document
                    .set_rich_presentation_with_anchors(
                        DocumentProjection::new(source.len(), spans).unwrap(),
                        document.model.blocks.clone(),
                        vec![],
                        DocumentStyles::default(),
                        cx,
                    )
                    .unwrap();
                assert_eq!(
                    document.selected_positions().unwrap().1.node_id(),
                    &"title-a"
                );
                assert_eq!(document.undo_checkpoint(), checkpoint);
                assert_eq!(
                    document.region_text(&"body-a").unwrap(),
                    if body.is_empty() || body.ends_with('\n') {
                        body.to_owned()
                    } else {
                        format!("{body}\n")
                    }
                );
            })
        });
    }
}
