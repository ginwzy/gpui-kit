use super::Root;
use gpui::{AppContext, TestApp, TextRun, font, px};
use gpui_base::document::{
    Affinity, DocumentInlineStyle, DocumentPosition, DocumentRegion, DocumentState, EditPolicy,
    TextRows,
};
use std::{cell::RefCell, rc::Rc};
use unicode_segmentation::UnicodeSegmentation as _;

pub(super) fn check(app: &mut TestApp) {
    check_modes(app);

    let code = "cargo test 中文 e\u{301} != -> ".repeat(12);
    let source = format!("Before {code} after 中文");
    let code_range = "Before ".len().."Before ".len() + code.len();
    let rows = Rc::new(RefCell::new(Vec::new()));
    let captured = rows.clone();
    let mut window = app.open_window(|_, cx| {
        let document = cx.new(|cx| {
            let mut doc = DocumentState::new(
                source.clone(),
                vec![DocumentRegion::new(
                    0,
                    0..source.len(),
                    EditPolicy::Readonly,
                )],
                cx,
            )
            .unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
            doc.set_node_inline_styles(
                &0,
                vec![
                    DocumentInlineStyle::new(code_range.clone(), Default::default())
                        .font_family("JetBrains Mono")
                        .font_scale(90),
                ],
                cx,
            )
            .unwrap();
            doc.set_text_renderer(
                move |context, element, _, _| {
                    captured.borrow_mut().push(context.source());
                    element
                },
                cx,
            );
            doc.set_selection(code_range.clone(), false, cx);
            doc
        });
        Root {
            document,
            family: "JetBrains Mono",
            width: px(320.),
            height: px(8000.),
        }
    });
    for scale in [90, 75, 90] {
        window.update(|root, _, cx| {
            root.document.update(cx, |doc, cx| {
                doc.set_node_inline_styles(
                    &0,
                    vec![
                        DocumentInlineStyle::new(code_range.clone(), Default::default())
                            .font_family("JetBrains Mono")
                            .font_scale(scale),
                    ],
                    cx,
                )
                .unwrap();
            });
        });
        for width in [320., 70., 200., 320.] {
            window.update(|root, window, cx| {
                root.width = px(width);
                window.resize(gpui::size(px(width), px(8000.)));
                cx.notify();
            });
            for _ in 0..3 {
                rows.borrow_mut().clear();
                window.update(|_, window, _| window.refresh());
                window.draw();
            }
            let mut rendered = rows.borrow().clone();
            rendered.sort_by_key(|row| row.start);
            rendered.dedup();
            assert_eq!(rendered.first().unwrap().start, 0);
            assert_eq!(rendered.last().unwrap().end, source.len());
            for pair in rendered.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
            }
            window.update(|root, window, cx| {
                let doc = root.document.read(cx);
                assert_eq!(doc.text(), source);
                assert_eq!(doc.selected_range(), code_range);
                for row in rendered {
                    let mut expected = px(0.);
                    for (span, size) in [
                        (0..code_range.start, 14.),
                        (code_range.clone(), 14. * f32::from(scale) / 100.),
                        (code_range.end..source.len(), 14.),
                    ] {
                        let start = span.start.max(row.start);
                        let end = span.end.min(row.end);
                        if start >= end {
                            continue;
                        }
                        let text = &source[start..end];
                        expected += window
                            .text_system()
                            .shape_line(
                                text.to_owned().into(),
                                px(size),
                                &[TextRun {
                                    len: text.len(),
                                    font: font("JetBrains Mono"),
                                    ..Default::default()
                                }],
                                None,
                            )
                            .width;
                    }
                    let start = doc
                        .position_bounds(&DocumentPosition::new(0, row.start, Affinity::After))
                        .unwrap();
                    let end = doc
                        .position_bounds(&DocumentPosition::new(0, row.end, Affinity::Before))
                        .unwrap();
                    assert_eq!(start.top(), end.top());
                    let actual = end.left() - start.left();
                    assert!(
                        (actual - expected).abs() < px(0.1),
                        "{row:?}: {actual:?} != {expected:?}"
                    );
                    assert!(
                        actual <= px(width + 0.1)
                            || source[row.clone()].graphemes(true).count() == 1
                    );
                    assert!(
                        row.end == source.len()
                            || source.grapheme_indices(true).any(|(ix, _)| ix == row.end)
                    );
                }
            });
        }
    }
}

fn check_modes(app: &mut TestApp) {
    let source = "Before cargo test after 中文";
    let code = 7..17;
    let mut window = app.open_window(|_, cx| {
        let document = cx.new(|cx| {
            let mut doc = DocumentState::new(
                source,
                vec![DocumentRegion::new(
                    0,
                    0..source.len(),
                    EditPolicy::Readonly,
                )],
                cx,
            )
            .unwrap();
            doc.set_node_inline_styles(
                &0,
                vec![
                    DocumentInlineStyle::new(code.clone(), Default::default())
                        .font_family("JetBrains Mono")
                        .font_scale(90),
                ],
                cx,
            )
            .unwrap();
            doc.set_selection(code.clone(), false, cx);
            doc
        });
        Root {
            document,
            family: "JetBrains Mono",
            width: px(400.),
            height: px(8000.),
        }
    });
    for rows in [false, true, false, true] {
        window.update(|root, _, cx| {
            root.document.update(cx, |doc, cx| {
                doc.set_text_rows(rows.then(TextRows::new), cx);
            });
        });
        for _ in 0..2 {
            window.update(|_, window, _| window.refresh());
            window.draw();
        }
        window.update(|root, window, cx| {
            let doc = root.document.read(cx);
            assert_eq!(doc.text(), source);
            assert_eq!(doc.selected_range(), code);
            let start = doc
                .position_bounds(&DocumentPosition::new(0, code.start, Affinity::After))
                .unwrap();
            let end = doc
                .position_bounds(&DocumentPosition::new(0, code.end, Affinity::Before))
                .unwrap();
            let text = &source[code.clone()];
            let expected = window
                .text_system()
                .shape_line(
                    text.to_owned().into(),
                    px(if rows { 12.6 } else { 14. }),
                    &[TextRun {
                        len: text.len(),
                        font: font("JetBrains Mono"),
                        ..Default::default()
                    }],
                    None,
                )
                .width;
            assert_eq!(start.top(), end.top());
            assert!((end.left() - start.left() - expected).abs() < px(0.1));
        });
    }
}
