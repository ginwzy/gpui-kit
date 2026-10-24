use super::*;
use crate::document::{InlineSize, ProjectionSpan, TextEdit};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, size};

fn fixture() -> DocumentSnapshot<&'static str> {
    let text = "header\n中🙂<alpha><beta>tail\n";
    let a = text.find('<').unwrap();
    let b = a + 7;
    let end = b + 6;
    let blocks = vec![
        DocumentBlock::new("a", a..b)
            .with_inline_size(InlineSize::new(px(64.), px(32.), px(24.)))
            .replaceable(true),
        DocumentBlock::new("b", b..end)
            .with_inline_size(InlineSize::new(px(80.), px(40.), px(20.)))
            .replaceable(true),
    ];
    DocumentSnapshot::new(
        text,
        vec![
            DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
            DocumentRegion::new("lead", 7..a, EditPolicy::Editable),
            DocumentRegion::new("a", a..b, EditPolicy::Atomic),
            DocumentRegion::new("between", b..b, EditPolicy::Editable),
            DocumentRegion::new("b", b..end, EditPolicy::Atomic),
            DocumentRegion::new("tail", end..text.len(), EditPolicy::Editable),
        ],
        DocumentProjection::new(
            text.len(),
            blocks
                .iter()
                .map(|block| ProjectionSpan::replace(block.source(), "\u{fffc}"))
                .collect(),
        )
        .unwrap(),
        blocks,
        DocumentStyles::default(),
    )
}

fn setup(cx: &mut TestAppContext) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
    let (document, mut view) = super::super::tests::document_view(cx);
    document.update(&mut view, |document, cx| {
        document.set_text_rows(Some(TextRows::new()), cx);
        document.set_block_renderer(|_, _, _| div().size_full().into_any_element(), cx);
        document.reset(fixture(), cx).unwrap();
    });
    view.simulate_resize(size(px(500.), px(300.)));
    settle(&document, &mut view);
    (document, view)
}

fn settle(document: &Entity<DocumentState<&'static str>>, view: &mut VisualTestContext) {
    for _ in 0..20 {
        view.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.run_until_parked();
        if !document.read_with(view, |document, _| document.is_laying_out_text()) {
            return;
        }
    }
    panic!("mixed row layout did not settle");
}

#[gpui::test]
fn constrained_objects_share_wrap_and_paint_width_after_resize(cx: &mut TestAppContext) {
    let (document, mut view) = setup(cx);
    document.update(&mut view, |document, cx| {
        let mut snapshot = fixture();
        for block in &mut snapshot.blocks {
            *block = block.clone().with_inline_size(
                InlineSize::new(px(600.), px(32.), px(24.)).constrain_width(true),
            );
        }
        document.reset(snapshot, cx).unwrap();
    });
    for width in [300., 100., 400.] {
        view.simulate_resize(size(px(width), px(300.)));
        settle(&document, &mut view);
        document.read_with(&view, |document, _| {
            let mut count = 0;
            for row in &document.text_layouts {
                if let LineLayout::Inline { parts, .. } = &row.layout {
                    for part in parts.iter().filter(|part| part.text.is_none()) {
                        count += 1;
                        assert_eq!(part.bounds.size.width, px(width));
                    }
                }
            }
            assert_eq!(count, 2);
            assert_eq!(document.text(), "header\n中🙂<alpha><beta>tail\n");
        });
    }
}

#[gpui::test]
fn paragraph_style_spans_inline_objects_but_inline_marks_still_reject_them(
    cx: &mut TestAppContext,
) {
    let (document, mut view) = setup(cx);
    document.update(&mut view, |document, cx| {
        let mut snapshot = fixture();
        snapshot.regions = snapshot
            .regions
            .into_iter()
            .map(|region| {
                let policy = if region.policy() == EditPolicy::Editable {
                    EditPolicy::Readonly
                } else {
                    region.policy()
                };
                DocumentRegion::new(*region.id(), region.range(), policy)
            })
            .collect();
        snapshot.styles = DocumentStyles::new(
            vec![crate::document::DocumentParagraphStyle::new(
                7..snapshot.text.len(),
                Default::default(),
            )],
            vec![],
        );
        document.reset(snapshot.clone(), cx).unwrap();
        snapshot.styles = DocumentStyles::new(
            vec![],
            vec![crate::document::DocumentInlineStyle::new(
                7..snapshot.text.len(),
                Default::default(),
            )],
        );
        assert!(matches!(
            document.reset(snapshot, cx),
            Err(PresentationError::Styles(DocumentStyleError::AtomicRange(
                _
            )))
        ));
    });
    settle(&document, &mut view);
}

#[gpui::test]
fn mixed_geometry_drives_hit_testing_selection_ime_and_reflow(cx: &mut TestAppContext) {
    let (document, mut view) = setup(cx);
    let (a, b, start, end) = document.read_with(&view, |document, _| {
        assert!(
            document
                .layout_items
                .iter()
                .all(|item| matches!(item, DocumentLayoutItem::Text { .. }))
        );
        let row = document
            .text_layouts
            .iter()
            .find(|row| matches!(row.layout, LineLayout::Inline { .. }))
            .unwrap();
        let LineLayout::Inline { parts, bounds } = &row.layout else {
            unreachable!()
        };
        let objects: Vec<_> = parts.iter().filter(|part| part.text.is_none()).collect();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].bounds.size, size(px(64.), px(32.)));
        assert_eq!(objects[1].bounds.size, size(px(80.), px(40.)));
        assert_eq!(
            objects[0].bounds.top() + px(24.),
            objects[1].bounds.top() + px(20.)
        );
        assert_eq!(bounds.size.height, px(44.));
        let a = document.region(&"a").unwrap().range();
        let b = document.region(&"b").unwrap().range();
        for (source, object) in [(&a, objects[0]), (&b, objects[1])] {
            for (x, expected) in [
                (object.bounds.left() + px(1.), source.start),
                (object.bounds.right() - px(1.), source.end),
            ] {
                let position = document
                    .position_for_point(point(x, object.bounds.center().y))
                    .unwrap();
                assert_eq!(document.resolve_position(&position).unwrap(), expected);
            }
        }
        (
            a,
            b,
            objects[0].bounds.origin + point(px(1.), px(5.)),
            objects[1].bounds.bottom_right() - point(px(1.), px(5.)),
        )
    });
    view.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    view.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
    view.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert_eq!(
        document.read_with(&view, |document, _| document.selected_range()),
        a.start..b.end
    );
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            let utf16 = document.range_to_utf16(&(a.start..a.start)).start;
            let bounds = document
                .bounds_for_range(
                    utf16..utf16,
                    document.list_state.viewport_bounds(),
                    window,
                    cx,
                )
                .unwrap();
            let expected = document
                .screen_position_for_source(a.start, Affinity::After)
                .unwrap();
            assert_eq!(bounds.origin, expected);
            assert_eq!(bounds.size.height, px(44.));
        })
    });
    view.simulate_resize(size(px(100.), px(300.)));
    settle(&document, &mut view);
    document.read_with(&view, |document, _| {
        let objects: Vec<_> = document
            .text_layouts
            .iter()
            .flat_map(|row| match &row.layout {
                LineLayout::Inline { parts, .. } => {
                    parts.iter().filter(|part| part.text.is_none()).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(objects.len(), 2);
        assert!(objects[0].bounds.top() < objects[1].bounds.top());
        assert_eq!(objects[0].bounds.size.width, px(64.));
        assert_eq!(objects[1].bounds.size.width, px(80.));
        for (ix, item) in document.layout_items.iter().enumerate() {
            let DocumentLayoutItem::Text {
                display,
                line,
                frame,
                ..
            } = item
            else {
                unreachable!()
            };
            assert!(
                (document.list_state.bounds_for_item(ix).unwrap().size.height
                    - frame
                        .0
                        .unwrap()
                        .row_height(display, line, item.presentation()))
                .abs()
                    < px(0.001)
            );
        }
    });
    view.simulate_resize(size(px(100.), px(60.)));
    document.update(&mut view, |document, cx| {
        document
            .reveal_position(&DocumentPosition::new("tail", 4, Affinity::Before), cx)
            .unwrap();
    });
    settle(&document, &mut view);
    document.read_with(&view, |document, _| {
        let (point, height, _, _) = document
            .text_position_for_position(&DocumentPosition::new("tail", 4, Affinity::Before))
            .unwrap();
        let viewport = document.list_state.viewport_bounds();
        assert!(point.y >= viewport.top() && point.y + height <= viewport.bottom() + px(0.01));
    });
}

#[gpui::test]
fn inline_navigation_and_replacement_policy_are_atomic(cx: &mut TestAppContext) {
    let (document, mut view) = setup(cx);
    view.update(|window, cx| {
        document.update(cx, |document, cx| {
            let a = document.region(&"a").unwrap().range();
            let b = document.region(&"b").unwrap().range();
            document.set_selection(a.start..a.start, false, cx);
            document.move_right(&MoveRight, window, cx);
            assert_eq!(document.selected_range(), a.end..a.end);
            document.move_right(&MoveRight, window, cx);
            assert_eq!(document.selected_range(), b.end..b.end);
            document.select_left(&SelectLeft, window, cx);
            assert_eq!(document.selected_range(), b.clone());
            document.select_left(&SelectLeft, window, cx);
            assert_eq!(document.selected_range(), a.start..b.end);
            let request = |range| {
                EditTransaction::new(
                    document.revision(),
                    EditOrigin::Paste,
                    vec![TextEdit::new(range, "中文🙂")],
                    document.model.text.len(),
                )
                .unwrap()
            };
            assert!(
                document
                    .inline_replacement(&request(a.start..b.end))
                    .is_some()
            );
            assert!(
                document
                    .inline_replacement(&request(a.start + 1..b.end))
                    .is_none()
            );
            assert!(document.inline_replacement(&request(0..b.end)).is_none());
            let old = document.text();
            document.set_selection(0..b.end, false, cx);
            document.replace_selection("blocked", window, cx);
            assert_eq!(document.text(), old);
            document.set_selection(a.start..a.start, false, cx);
            document.replace_and_mark_text_in_range(None, "中", Some(1..1), window, cx);
            assert!(
                document.model.marked.is_some(),
                "composition before an inline object belongs to editable text"
            );
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::User,
                vec![TextEdit::new(a.start + 3..b.end + 3, "x")],
                document.model.text.len(),
            )
            .unwrap();
            assert!(document.inline_replacement(&transaction).is_none());
            document.unmark_text(window, cx);
            let mut protected = fixture();
            protected.blocks[0] = protected.blocks[0].clone().replaceable(false);
            document.reset(protected, cx).unwrap();
            let before = document.text();
            document.set_selection(a.clone(), false, cx);
            document.replace_selection("blocked", window, cx);
            assert_eq!(
                document.text(),
                before,
                "inline appearance alone does not enable deletion"
            );
        })
    });
}

#[gpui::test]
fn pending_mixed_long_line_stays_bounded_and_can_be_superseded(cx: &mut TestAppContext) {
    let (document, mut view) = setup(cx);
    let prefix = "word ".repeat(100_000);
    let text = format!("{prefix}<object>\n");
    document.update(&mut view, |document, cx| {
        document
            .reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![
                        DocumentRegion::new("lead", 0..prefix.len(), EditPolicy::Editable),
                        DocumentRegion::new("a", prefix.len()..text.len() - 1, EditPolicy::Atomic),
                        DocumentRegion::new(
                            "tail",
                            text.len() - 1..text.len(),
                            EditPolicy::Editable,
                        ),
                    ],
                    DocumentProjection::new(
                        text.len(),
                        vec![ProjectionSpan::replace(
                            prefix.len()..text.len() - 1,
                            "\u{fffc}",
                        )],
                    )
                    .unwrap(),
                    vec![
                        DocumentBlock::new("a", prefix.len()..text.len() - 1)
                            .with_inline_size(InlineSize::new(px(80.), px(40.), px(20.))),
                    ],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
    view.update(|window, cx| {
        let _ = window.draw(cx);
    });
    document.read_with(&view, |document, _| {
        assert!(document.is_laying_out_text());
        assert!(
            document
                .text_layouts
                .iter()
                .all(|row| row.display.len() <= 2048)
        );
    });
    document.update(&mut view, |document, cx| {
        document.reset(fixture(), cx).unwrap()
    });
    settle(&document, &mut view);
    assert_eq!(
        document.read_with(&view, |document, _| document.text()),
        fixture().text
    );
}

#[gpui::test]
fn sized_text_after_wrapping_uses_the_same_painted_baseline(cx: &mut TestAppContext) {
    use crate::document::DocumentInlineStyle;
    let (document, mut view) = super::super::tests::document_view(cx);
    let prefix = "正文 English prose before the link. ".repeat(8);
    let source = format!("{prefix}文件README.md normal 中文");
    let code = prefix.len()..prefix.len() + "文件README.md".len();
    document.update(&mut view, |document, cx| {
        document.set_text_rows(Some(TextRows::new()), cx);
        document
            .reset(
                DocumentSnapshot::new(
                    source.clone(),
                    vec![DocumentRegion::new(
                        "history",
                        0..source.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::identity(source.len()),
                    vec![],
                    DocumentStyles::new(
                        vec![],
                        vec![
                            DocumentInlineStyle::new(code.clone(), Default::default())
                                .font_scale(90),
                        ],
                    ),
                ),
                cx,
            )
            .unwrap();
        document.set_selection(code.clone(), false, cx);
    });
    let mut checked = 0;
    for width in [550., 240., 550.] {
        view.simulate_resize(size(px(width), px(1200.)));
        settle(&document, &mut view);
        document.read_with(&view, |document, _| {
            assert_eq!(document.selected_range(), code);
            assert_eq!(document.text(), source);
            for record in &document.text_layouts {
                let LineLayout::Inline { parts, bounds } = &record.layout else {
                    continue;
                };
                let baselines: Vec<_> = parts
                    .iter()
                    .filter_map(|part| {
                        let text = part.text.as_ref()?;
                        let line = text.line_layout_for_index(0)?;
                        Some(
                            part.bounds.top()
                                + (text.line_height() - line.ascent() - line.descent()) / 2.
                                + line.ascent(),
                        )
                    })
                    .collect();
                for pair in baselines.windows(2) {
                    assert!(
                        (pair[0] - pair[1]).abs() < px(0.1),
                        "text baselines differ at width {width}: {baselines:?}, row {bounds:?}"
                    );
                    checked += 1;
                }
                for part in parts {
                    assert!(
                        part.bounds.bottom() <= bounds.bottom() + px(1.),
                        "text fell below its row: {:?} vs {bounds:?}",
                        part.bounds
                    );
                }
            }
        });
    }
    assert!(
        checked > 0,
        "must exercise normal and sized text on the same wrapped row"
    );
}
