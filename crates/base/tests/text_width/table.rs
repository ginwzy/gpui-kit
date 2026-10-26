use super::Root;
use gpui::{AppContext, TestApp, TextAlign, px};
use gpui_base::document::{
    Affinity, DocumentInlineStyle, DocumentParagraphStyle, DocumentPosition, DocumentProjection,
    DocumentRegion, DocumentState, DocumentStyles, EditPolicy, TableRow, TextFrame, TextRows,
};

pub(super) fn check(app: &mut TestApp) {
    natural_widths(app);
    long_first_cell_before_a_blank_line(app);
    let rows = [
        "Name | Value".to_owned(),
        "中文 👩‍💻 | ffi code is longer than the header and wraps across several lines".to_owned(),
        "End | 42".to_owned(),
        format!("Long | {}", "long words 中文🙂 ".repeat(240)),
    ];
    let source = rows.join("\n");
    let mut start = 0;
    let mut paragraphs = Vec::new();
    let mut starts = Vec::new();
    for (ix, row) in rows.iter().enumerate() {
        starts.push(start);
        let pipe = row.find('|').unwrap();
        let end = start + row.len() + usize::from(ix + 1 < rows.len());
        paragraphs.push(
            DocumentParagraphStyle::new(
                start..end,
                gpui::TextStyleRefinement {
                    font_weight: (ix == 0).then_some(gpui::FontWeight::BOLD),
                    ..Default::default()
                },
            )
            .with_table_row(TableRow::new(0).with_cells([
                (0..pipe - 1, TextAlign::Left),
                (pipe + 2..row.len(), TextAlign::Right),
            ])),
        );
        start = end;
    }
    let selection = starts[1]..starts[2] + rows[2].len();
    let code = source.find("ffi code").unwrap();
    let mut window = app.open_window(|_, cx| {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                source.clone(),
                vec![DocumentRegion::new(
                    0,
                    0..source.len(),
                    EditPolicy::Readonly,
                )],
                cx,
            )
            .unwrap();
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .set_rich_presentation(
                    DocumentProjection::identity(source.len()),
                    Vec::new(),
                    DocumentStyles::new(
                        paragraphs,
                        vec![
                            DocumentInlineStyle::new(code..code + 8, Default::default())
                                .font_scale(90)
                                .font_family("JetBrains Mono"),
                        ],
                    ),
                    cx,
                )
                .unwrap();
            document.set_selection(selection.clone(), false, cx);
            document
        });
        Root {
            document,
            family: if cfg!(target_os = "macos") {
                ".SystemUIFont"
            } else {
                "JetBrains Mono"
            },
            width: px(600.),
            height: px(8000.),
        }
    });
    for width in [600., 300., 90., 600.] {
        window.update(|root, window, cx| {
            root.width = px(width);
            window.resize(gpui::size(px(width), px(8000.)));
            cx.notify();
        });
        for _ in 0..3 {
            window.update(|_, window, _| window.refresh());
            window.draw();
        }
        window.update(|root, _, cx| {
            let document = root.document.read(cx);
            assert_eq!(document.text(), source);
            assert_eq!(document.selected_range(), selection);
            let position = |offset| {
                document
                    .position_bounds(&DocumentPosition::new(0, offset, Affinity::After))
                    .expect("table glyph position")
            };
            let left = position(starts[0]);
            let body = position(starts[1]);
            let last = position(starts[2]);
            assert!((left.left() - body.left()).abs() < px(0.1));
            assert!((left.left() - last.left()).abs() < px(0.1));
            assert!(left.top() < body.top() && body.top() < last.top());
            let ends: Vec<_> = rows
                .iter()
                .zip(&starts)
                .map(|(row, start)| {
                    document
                        .position_bounds(&DocumentPosition::new(
                            0,
                            start + row.len(),
                            Affinity::Before,
                        ))
                        .unwrap()
                })
                .collect();
            assert!(
                (ends[0].left() - ends[1].left()).abs() < px(0.2),
                "right alignment after wrapping"
            );
            assert!(
                (ends[0].left() - ends[2].left()).abs() < px(0.2),
                "width={width}, row ends={ends:?}"
            );
        });
    }
    let body_end = starts[1] + rows[1].len();
    let before = window.update(|root, _, cx| {
        root.document
            .read(cx)
            .position_bounds(&DocumentPosition::new(0, body_end, Affinity::Before))
            .unwrap()
            .top()
    });
    window.update(|root, _, cx| {
        root.document.update(cx, |document, cx| {
            document
                .set_text_frame_resolver(|_, _, _| TextFrame::new().with_max_width(px(180.)), cx);
        })
    });
    for _ in 0..4 {
        window.draw();
    }
    window.update(|root, _, cx| {
        let after = root
            .document
            .read(cx)
            .position_bounds(&DocumentPosition::new(0, body_end, Affinity::Before))
            .unwrap()
            .top();
        assert!(
            after > before,
            "a changed host frame must rewrap table cells: {before:?} -> {after:?}"
        );
        root.document.update(cx, |document, cx| {
            document.set_text_frame_resolver(|_, _, _| TextFrame::new(), cx)
        });
    });
    for _ in 0..4 {
        window.draw();
    }
    let target = starts[3] + rows[3].match_indices("中文").nth(120).unwrap().0;
    window.update(|root, window, cx| {
        root.width = px(300.);
        root.height = px(480.);
        window.resize(gpui::size(px(300.), px(480.)));
        root.document.update(cx, |document, cx| {
            document
                .reveal_position_at_top(&DocumentPosition::new(0, target, Affinity::After), cx)
                .unwrap();
        });
        cx.notify();
    });
    for _ in 0..4 {
        window.draw();
    }
    window.update(|root, _, cx| {
        let document = root.document.read(cx);
        let viewport = document.list_state().viewport_bounds();
        let position = |at| {
            document
                .position_bounds(&DocumentPosition::new(0, at, Affinity::After))
                .unwrap()
        };
        assert!(
            (position(target).top() - viewport.top()).abs() < px(1.),
            "target={:?}, viewport={viewport:?}, pending={}, scroll={:?}, top={:?}",
            position(target),
            document.is_laying_out_text(),
            document.list_state().logical_scroll_top(),
            document.viewport_top()
        );
        assert!(position(starts[3]).top() < viewport.top());
        assert!(position(source.len()).top() > viewport.bottom());
    });
    for width in [200., 480., 300.] {
        let (anchor, y) = window.update(|root, _, cx| {
            let document = root.document.read(cx);
            let anchor = document.viewport_top().unwrap();
            let y = document.position_bounds(&anchor).unwrap().top();
            (anchor, y)
        });
        window.update(|root, window, cx| {
            root.width = px(width);
            window.resize(gpui::size(px(width), px(480.)));
            cx.notify();
        });
        for _ in 0..4 {
            window.draw();
        }
        window.update(|root, _, cx| {
            let document = root.document.read(cx);
            assert!(
                (document.position_bounds(&anchor).unwrap().top() - y).abs() < px(1.),
                "native table reading anchor moved at {width}"
            );
            assert_eq!(document.text(), source);
            assert_eq!(document.selected_range(), selection);
        });
    }
    for enabled in [false, true] {
        window.update(|root, _, cx| {
            root.document.update(cx, |document, cx| {
                document.set_text_rows(enabled.then(TextRows::new), cx)
            })
        });
        window.draw();
        window.update(|root, _, cx| {
            let document = root.document.read(cx);
            assert_eq!(document.text(), source);
            assert_eq!(document.selected_range(), selection);
        });
    }
}

fn long_first_cell_before_a_blank_line(app: &mut TestApp) {
    let cell = "long words 中文 ".repeat(300);
    let source = format!("{cell}|end\n\n");
    let row_end = source.len() - 1;
    let mut window = app.open_window(|_, cx| {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                source.clone(),
                vec![DocumentRegion::new(
                    0,
                    0..source.len(),
                    EditPolicy::Readonly,
                )],
                cx,
            )
            .unwrap();
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .set_rich_presentation(
                    DocumentProjection::identity(source.len()),
                    Vec::new(),
                    DocumentStyles::new(
                        vec![
                            DocumentParagraphStyle::new(0..row_end, Default::default())
                                .with_table_row(TableRow::new(0).with_cells([
                                    (0..cell.len(), TextAlign::Left),
                                    (cell.len() + 1..row_end - 1, TextAlign::Right),
                                ])),
                        ],
                        Vec::new(),
                    ),
                    cx,
                )
                .unwrap();
            document
        });
        Root {
            document,
            family: "JetBrains Mono",
            width: px(400.),
            height: px(480.),
        }
    });
    for _ in 0..3 {
        window.draw();
        app.run_until_parked();
    }
    window.update(|root, _, cx| {
        let document = root.document.read(cx);
        assert!(!document.is_laying_out_text());
        assert!(
            document
                .position_bounds(&DocumentPosition::new(0, cell.len() + 1, Affinity::After))
                .is_some()
        );
        assert_eq!(document.text(), source);
    });
}

fn natural_widths(app: &mut TestApp) {
    let rows = [
        [
            "远程缓存",
            "模型",
            "版本",
            "构建",
            "测试",
            "平台",
            "内存",
            "延迟",
        ],
        ["README", "GPT", "v6", "128", "42", "macOS", "256", "8"],
        ["cargo test", "Pi", "v3", "16", "1,024", "Linux", "64", "12"],
    ];
    let source = rows.map(|row| row.join(" | ")).join("\n");
    let mut start = 0;
    let paragraphs = rows
        .iter()
        .enumerate()
        .map(|(ix, cells)| {
            let mut at = 0;
            let ranges = cells
                .iter()
                .map(|cell| {
                    let range = at..at + cell.len();
                    at += cell.len() + 3;
                    (range, TextAlign::Left)
                })
                .collect::<Vec<_>>();
            let end = start + at - 3 + usize::from(ix < rows.len() - 1);
            let paragraph = DocumentParagraphStyle::new(
                start..end,
                gpui::TextStyleRefinement {
                    font_size: Some(px(15.).into()),
                    font_weight: (ix == 0).then_some(gpui::FontWeight::BOLD),
                    ..Default::default()
                },
            )
            .with_table_row(TableRow::new(0).with_cells(ranges));
            start = end;
            paragraph
        })
        .collect();
    let offset = source.find("macOS").unwrap();
    let mut window = app.open_window(|_, cx| {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                source.clone(),
                vec![DocumentRegion::new(
                    0,
                    0..source.len(),
                    EditPolicy::Readonly,
                )],
                cx,
            )
            .unwrap();
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .set_rich_presentation(
                    DocumentProjection::identity(source.len()),
                    Vec::new(),
                    DocumentStyles::new(paragraphs, Vec::new()),
                    cx,
                )
                .unwrap();
            document
        });
        Root {
            document,
            family: if cfg!(target_os = "macos") {
                ".SystemUIFont"
            } else {
                "JetBrains Mono"
            },
            width: px(840.),
            height: px(8000.),
        }
    });
    for _ in 0..3 {
        window.draw();
    }
    window.update(|root, _, cx| {
        let document = root.document.read(cx);
        let before = document
            .position_bounds(&DocumentPosition::new(0, offset, Affinity::After))
            .unwrap();
        let after = document
            .position_bounds(&DocumentPosition::new(0, offset + 5, Affinity::Before))
            .unwrap();
        assert_eq!(
            before.top(),
            after.top(),
            "a cell at its natural width must not gain a spurious wrap"
        );
    });
}
