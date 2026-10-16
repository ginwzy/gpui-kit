use super::*;
use crate::document::{
    DocumentInlineStyle, DocumentProjection, DocumentSnapshot, DocumentStyles, EditPolicy,
};
use gpui::{FontStyle, FontWeight, HighlightStyle, TestAppContext};

#[test]
fn projected_font_fields_match_ordered_covering_semantics() {
    use super::super::DocumentModel;
    use crate::document::ProjectionSpan;
    let source = "abcdefghijklmnop";
    let mut model = DocumentModel::new(
        source,
        vec![DocumentRegion::new(
            (),
            0..source.len(),
            EditPolicy::Readonly,
        )],
    )
    .unwrap();
    model
        .set_rich_presentation(
            DocumentProjection::new(
                source.len(),
                vec![
                    ProjectionSpan::replace(1..5, "中"),
                    ProjectionSpan::hide(5..7),
                    ProjectionSpan::replace(8..14, "e\u{301}🙂"),
                ],
            )
            .unwrap(),
            vec![],
            DocumentStyles::new(
                vec![],
                (0..16)
                    .map(|ix| {
                        let style = DocumentInlineStyle::new(
                            ix..ix + 1,
                            HighlightStyle {
                                font_weight: (ix % 3 == 0).then_some(if ix % 2 == 0 {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                }),
                                font_style: (ix % 3 == 1).then_some(FontStyle::Italic),
                                background_color: Some(gpui::blue()),
                                ..Default::default()
                            },
                        );
                        if ix % 3 == 2 {
                            style.font_family("mono")
                        } else {
                            style
                        }
                    })
                    .collect(),
            ),
        )
        .unwrap();
    assert_eq!(
        model.resolved_styles.inline.len(),
        14,
        "hidden ranges collapse"
    );
    assert!(
        model
            .resolved_styles
            .inline
            .windows(2)
            .any(|pair| pair[0].display == pair[1].display)
    );
    let text = model.display_text();
    let DocumentLayoutItem::Text {
        presentation: Some(p),
        ..
    } = model.text_layout_item(text, 0..text.len(), 0..source.len(), 0..text.len(), 0)
    else {
        panic!()
    };
    assert_font_fields(&p, text);
    // Partially overlapping monotonic endpoints, simultaneous starts/ends,
    // empty intervals, gaps, and boundaries inside a grapheme cluster.
    let p = super::super::DocumentTextPresentation {
        highlights: vec![
            (
                0..3,
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            ),
            (
                1..3,
                HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
            ),
            (
                1..6,
                HighlightStyle {
                    color: Some(gpui::red()),
                    ..Default::default()
                },
            ),
            (
                3..6,
                HighlightStyle {
                    font_weight: Some(FontWeight::NORMAL),
                    ..Default::default()
                },
            ),
            (
                6..6,
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            ),
        ],
        font_family_overrides: vec![
            (0..1, "first".into()),
            (0..3, "second".into()),
            (3..3, "ignored".into()),
            (3..6, "third".into()),
        ],
        ..Default::default()
    };
    assert_font_fields(&p, "e\u{301}中🙂");
    assert!(font_runs(&p, 0).next().is_none());
}

fn assert_font_fields(p: &super::super::DocumentTextPresentation, text: &str) {
    let runs: Vec<_> = font_runs(p, text.len()).collect();
    assert_eq!(runs.first().unwrap().0.start, 0);
    assert_eq!(runs.last().unwrap().0.end, text.len());
    assert!(
        runs.windows(2)
            .all(|pair| pair[0].0.end == pair[1].0.start && pair[0].1 != pair[1].1)
    );
    // Independent oracle: the former line_spec's ordered covering scan, at
    // every character boundary, including boundaries within a grapheme.
    for (offset, _) in text.char_indices() {
        let mut expected = gpui::font("base");
        for (range, highlight) in &p.highlights {
            if range.contains(&offset) {
                if let Some(weight) = highlight.font_weight {
                    expected.weight = weight;
                }
                if let Some(style) = highlight.font_style {
                    expected.style = style;
                }
            }
        }
        for (range, family) in &p.font_family_overrides {
            if range.contains(&offset) {
                expected.family = family.clone();
            }
        }
        let mut actual = gpui::font("base");
        runs.iter()
            .find(|(range, _)| range.contains(&offset))
            .unwrap()
            .1
            .apply(&mut actual);
        assert_eq!(actual, expected, "at {offset} in {text:?}");
    }
}

fn item(
    len: usize,
    presentation: Option<super::super::DocumentTextPresentation>,
) -> DocumentLayoutItem<()> {
    DocumentLayoutItem::Text {
        fingerprint: 123,
        display: 0..len,
        source: 0..len,
        node_id: Some(()),
        presentation: presentation.map(std::sync::Arc::new),
        line: 0..len,
        line_key: 0,
        frame: Derived::default(),
        wrapped: Derived::default(),
    }
}

#[test]
fn wrap_identity_ignores_paint_and_font_partitioning_but_preserves_inheritance() {
    use super::super::DocumentTextPresentation;
    let plain = line_key(&item(12, None));
    for text_style in [
        None,
        Some(TextStyleRefinement::default()),
        Some(TextStyleRefinement {
            color: Some(gpui::red()),
            ..Default::default()
        }),
    ] {
        let paint = DocumentTextPresentation {
            text_style,
            highlights: vec![(
                2..8,
                HighlightStyle {
                    background_color: Some(gpui::blue()),
                    ..Default::default()
                },
            )],
            ..Default::default()
        };
        assert_eq!(plain, line_key(&item(12, Some(paint))));
    }
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    let mut original = DocumentTextPresentation {
        highlights: vec![(2..10, bold)],
        font_family_overrides: vec![(2..10, "mono".into())],
        ..Default::default()
    };
    let key = line_key(&item(12, Some(original.clone())));
    let split = DocumentTextPresentation {
        highlights: vec![
            (2..4, bold),
            (
                4..7,
                HighlightStyle {
                    background_color: Some(gpui::blue()),
                    ..bold
                },
            ),
            (7..10, bold),
        ],
        font_family_overrides: vec![(2..7, "mono".into()), (7..10, "mono".into())],
        ..Default::default()
    };
    assert_eq!(key, line_key(&item(12, Some(split))));
    original.font_family_overrides[0].1 = "different".into();
    assert_ne!(key, line_key(&item(12, Some(original.clone()))));
    original.font_family_overrides[0].1 = "mono".into();
    original.highlights[0].1.font_style = Some(FontStyle::Italic);
    assert_ne!(key, line_key(&item(12, Some(original.clone()))));
    original.highlights[0].1 = HighlightStyle {
        font_weight: Some(FontWeight::NORMAL),
        ..Default::default()
    };
    original.font_family_overrides.clear();
    assert_ne!(plain, line_key(&item(12, Some(original))));
}

#[gpui::test]
fn paint_restyles_keep_wrapped_geometry_and_refresh_presentation(cx: &mut TestAppContext) {
    let (document, mut cx) = super::super::tests::document_view(cx);
    let text = "中文 e\u{301}🙂 words ".repeat(100);
    document.update(&mut cx, |doc, cx| {
        doc.reset(
            DocumentSnapshot::new(
                text.clone(),
                vec![DocumentRegion::new(
                    "source",
                    0..text.len(),
                    EditPolicy::Readonly,
                )],
                DocumentProjection::identity(text.len()),
                vec![],
                DocumentStyles::default(),
            ),
            cx,
        )
        .unwrap();
        doc.set_text_rows(Some(TextRows::new()), cx);
        doc.set_selection(0..6, false, cx);
    });
    cx.simulate_resize(gpui::size(px(320.), px(400.)));
    super::tests::settle_rows(&document, &mut cx);
    let before = document.read_with(&cx, |doc, _| doc.layout_items.clone());
    assert!(before.len() > 10);
    for styles in [
        vec![DocumentInlineStyle::new(
            0..6,
            HighlightStyle {
                background_color: Some(gpui::blue()),
                ..Default::default()
            },
        )],
        vec![DocumentInlineStyle::new(
            7..10,
            HighlightStyle {
                color: Some(gpui::red()),
                ..Default::default()
            },
        )],
        vec![],
    ] {
        document.update(&mut cx, |doc, cx| {
            doc.set_node_inline_styles(&"source", styles.clone(), cx)
                .unwrap();
            assert_eq!(doc.layout_items.len(), before.len());
            for (old, new) in before.iter().zip(doc.layout_items.iter()) {
                if let (
                    DocumentLayoutItem::Text {
                        display: a,
                        line_key: ak,
                        frame: af,
                        wrapped: aw,
                        ..
                    },
                    DocumentLayoutItem::Text {
                        display: b,
                        line_key: bk,
                        frame: bf,
                        wrapped: bw,
                        ..
                    },
                ) = (old, new)
                {
                    assert_eq!((a, ak, af, aw), (b, bk, bf, bw));
                }
            }
            assert_eq!(doc.selected_range(), 0..6);
            assert_eq!(doc.styles().inline(), styles);
            assert_eq!(doc.text(), text);
            assert_eq!(
                doc.layout_items[0].same_layout_identity(&before[0]),
                styles.is_empty()
            );
        });
        super::tests::settle_rows(&document, &mut cx);
        super::tests::assert_declared_geometry(&document, &mut cx);
    }
    document.update(&mut cx, |doc, cx| {
        doc.set_node_inline_styles(&"source", vec![DocumentInlineStyle::new(0..text.len(), HighlightStyle {
            font_weight: Some(FontWeight::BOLD), font_style: Some(FontStyle::Italic), ..Default::default()
        }).font_family("Zed Mono")], cx).unwrap();
        assert_ne!(line_key(&doc.layout_items[0]), line_key(&before[0]));
        assert!(matches!(&doc.layout_items[0], DocumentLayoutItem::Text { wrapped, .. } if wrapped.0.is_none()));
    });
    super::tests::settle_rows(&document, &mut cx);
    cx.update(|window, cx| {
        document.read_with(cx, |doc, _| {
            let mut fonts = FontTable::new(window);
            let spec = doc.line_spec(
                &StaleLine {
                    items: doc.line_rows(0),
                    line: 0..text.len(),
                    focused: false,
                    bytes: text.len(),
                    wrapped_before: true,
                    in_view: true,
                    widths: (px(320.), px(320.)),
                },
                &mut fonts,
            );
            assert_eq!(spec.runs.len(), 1);
            let (font, _) = &fonts.fonts[spec.runs[0].1];
            assert_eq!(font.weight, FontWeight::BOLD);
            assert_eq!(font.style, FontStyle::Italic);
            assert_eq!(font.family.as_ref(), "Zed Mono");
        })
    });
    let wide = document.read_with(&cx, |doc, _| doc.layout_items.len());
    cx.simulate_resize(gpui::size(px(160.), px(400.)));
    super::tests::settle_rows(&document, &mut cx);
    let narrow = document.read_with(&cx, |doc, _| doc.layout_items.len());
    assert!(narrow > wide);
    cx.update(|window, _| window.set_rem_size(px(24.)));
    super::tests::settle_rows(&document, &mut cx);
    document.read_with(&cx, |doc, _| {
        assert!(doc.layout_items.len() > narrow);
        assert_eq!(doc.text(), text);
        assert_eq!(doc.selected_range(), 0..6);
        assert!(
            doc.layout_items
                .iter()
                .filter_map(|item| match item {
                    DocumentLayoutItem::Text { display, .. } => Some(display.start),
                    _ => None,
                })
                .all(|offset| text
                    .grapheme_indices(true)
                    .any(|(start, _)| start == offset))
        );
    });
    super::tests::assert_declared_geometry(&document, &mut cx);
}

// Deliberately bypasses the row cache: measure both identity and actual wrap
// input construction for dense paint and dense changes of font, separately.
#[gpui::test]
#[ignore]
fn font_runs_benchmark(cx: &mut TestAppContext) {
    let (document, mut cx) = super::super::tests::document_view(cx);
    for count in [1250, 2500, 5000, 10000] {
        for fonts_change in [false, true] {
            let text = "ab ".repeat(count);
            document.update(&mut cx, |doc, cx| {
                doc.reset(
                    DocumentSnapshot::new(
                        text.clone(),
                        vec![DocumentRegion::new(
                            "source",
                            0..text.len(),
                            EditPolicy::Readonly,
                        )],
                        DocumentProjection::identity(text.len()),
                        vec![],
                        DocumentStyles::new(
                            vec![],
                            (0..count)
                                .map(|ix| {
                                    DocumentInlineStyle::new(
                                        3 * ix..3 * ix + 1,
                                        HighlightStyle {
                                            background_color: Some(gpui::blue()),
                                            font_weight: fonts_change.then_some(FontWeight::BOLD),
                                            ..Default::default()
                                        },
                                    )
                                })
                                .collect(),
                        ),
                    ),
                    cx,
                )
                .unwrap();
            });
            cx.update(|window, cx| document.read_with(cx, |doc, _| {
                for sample in 0..5 {
                    let mut fonts = FontTable::new(window);
                    let start = std::time::Instant::now();
                    std::hint::black_box(line_key(&doc.layout_items[0]));
                    let key_us = start.elapsed().as_secs_f64()*1e6;
                    let start = std::time::Instant::now();
                    let spec = doc.line_spec(&StaleLine { items: 0..1, line: 0..text.len(), focused: false, bytes: text.len(), wrapped_before: false, in_view: true, widths: (px(900.), px(900.)) }, &mut fonts);
                    eprintln!("FONT_PROBE count={count} fonts={fonts_change} sample={sample} key_us={key_us:.3} spec_us={:.3} runs={}", start.elapsed().as_secs_f64()*1e6, spec.runs.len());
                    std::hint::black_box(spec);
                }
            }));
        }
    }
}
