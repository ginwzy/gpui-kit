use super::*;
use crate::document::{
    DocumentInlineStyle, DocumentParagraphStyle, ProjectionMapping, ProjectionSpan,
};

// The previous full scan is an oracle for error order, including its treatment
// of empty ranges. Keep it only in tests.
fn validate_reference(
    text: &Rope,
    regions: &DocumentRegions<usize>,
    mut ranges: Vec<Range<usize>>,
    allow_editable: bool,
) -> Result<(), DocumentStyleError> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut previous_end = 0;
    for range in ranges {
        if range.end > text.len() || range.start > range.end {
            return Err(DocumentStyleError::OutOfBounds {
                range,
                source_len: text.len(),
            });
        }
        if text.clip_offset(range.start, Bias::Left) != range.start
            || text.clip_offset(range.end, Bias::Right) != range.end
        {
            return Err(DocumentStyleError::InvalidBoundary(range));
        }
        if range.start < previous_end {
            return Err(DocumentStyleError::Overlap(range));
        }
        for region in regions.as_slice() {
            let other = region.range();
            if range.start < other.end && other.start < range.end {
                match region.policy() {
                    EditPolicy::Editable if !allow_editable => {
                        return Err(DocumentStyleError::EditableRange(range));
                    }
                    EditPolicy::Atomic => return Err(DocumentStyleError::AtomicRange(range)),
                    _ => {}
                }
            }
        }
        previous_end = range.end;
    }
    Ok(())
}

#[test]
fn style_validation_matches_full_scan_at_all_boundaries() {
    let text = Rope::from("a中🙂\nzz");
    // Includes invalid UTF-8, inverted and out-of-bounds ranges, empty ranges,
    // shared endpoints, gaps, spanning ranges and unsorted/overlapping inputs.
    let offsets = [0, 1, 2, 4, 5, 8, 9, text.len(), text.len() + 1];
    let ranges: Vec<_> = offsets
        .iter()
        .flat_map(|start| offsets.iter().map(move |end| *start..*end))
        .collect();
    for policy in [
        EditPolicy::Editable,
        EditPolicy::Readonly,
        EditPolicy::Atomic,
        EditPolicy::Routed,
    ] {
        let regions = DocumentRegions::new(
            vec![
                DocumentRegion::new(4, 9..text.len(), EditPolicy::Readonly),
                DocumentRegion::new(3, 8..8, EditPolicy::Atomic),
                DocumentRegion::new(2, 4..8, policy),
                DocumentRegion::new(1, 1..4, EditPolicy::Editable),
                DocumentRegion::new(0, 0..1, EditPolicy::Routed),
                DocumentRegion::new(5, 4..4, EditPolicy::Editable),
            ],
            text.len(),
        )
        .unwrap();
        for allow_editable in [false, true] {
            for first in &ranges {
                for second in &ranges {
                    let input = vec![first.clone(), second.clone()];
                    assert_eq!(
                        validate_style_ranges(&text, &regions, input.clone(), allow_editable),
                        validate_reference(&text, &regions, input.clone(), allow_editable),
                        "{policy:?}, editable={allow_editable}, ranges={input:?}",
                    );
                }
            }
        }
    }
    let text = Rope::from("abcdefgh");
    let regions = DocumentRegions::new(
        vec![
            DocumentRegion::new(0, 0..4, EditPolicy::Atomic),
            DocumentRegion::new(1, 4..4, EditPolicy::Editable),
            DocumentRegion::new(2, 4..8, EditPolicy::Readonly),
        ],
        text.len(),
    )
    .unwrap();
    assert_eq!(
        validate_style_ranges(&text, &regions, [2..2], true),
        Err(DocumentStyleError::AtomicRange(2..2))
    );
    assert_eq!(
        validate_style_ranges(&text, &regions, [4..4, 8..8], false),
        Ok(())
    );
    assert_eq!(
        validate_style_ranges(&text, &regions, [0..8], false),
        Err(DocumentStyleError::AtomicRange(0..8))
    );
    let empty = DocumentRegions::<usize>::new(vec![], 0).unwrap();
    assert_eq!(
        validate_style_ranges(&Rope::new(), &empty, [0..0], false),
        Ok(())
    );
}

fn paragraph(range: Range<usize>, ix: usize) -> DocumentParagraphStyle {
    DocumentParagraphStyle::new(
        range,
        TextStyleRefinement {
            font_size: Some(px(12. + ix as f32).into()),
            line_height: Some(px(20. + ix as f32).into()),
            ..Default::default()
        },
    )
    .with_hanging_marker(format!("{ix}. "))
}

fn assert_first_match(model: &DocumentModel<&str>) {
    let paragraphs = &model.resolved_styles.paragraphs;
    assert!(paragraphs.windows(2).all(|pair| {
        pair[0].display.start <= pair[1].display.start && pair[0].display.end <= pair[1].display.end
    }));
    let display = model.display_text();
    let offsets: Vec<_> = display
        .char_indices()
        .map(|(ix, _)| ix)
        .chain([display.len()])
        .collect();
    for start in &offsets {
        for end in offsets.iter().filter(|end| *end >= start) {
            let line = *start..*end;
            let expected = paragraphs
                .iter()
                .find(|style| style.display.start <= line.start && line.end <= style.display.end);
            let DocumentLayoutItem::Text { presentation, .. } =
                model.text_layout_item(display, line.clone(), 0..model.text.len())
            else {
                unreachable!()
            };
            let presentation = presentation.as_deref().cloned().unwrap_or_default();
            assert_eq!(
                presentation.text_style,
                expected.map(|style| style.text_style.clone()),
                "{line:?}"
            );
            assert_eq!(
                presentation.indent,
                expected.and_then(|style| Some(DocumentTextIndent {
                    marker: style.hanging_marker.clone()?,
                    hanging: style.display.start == line.start,
                })),
                "{line:?}"
            );
        }
    }
}

#[test]
fn paragraph_first_match_survives_empty_lines_and_collapsed_projection() {
    for (source, spans, ranges) in [
        ("", vec![], vec![]),
        ("", vec![], vec![0..0]),
        ("plain", vec![], vec![]),
        ("\n\nlast\n", vec![], vec![0..1, 1..1, 1..2, 2..7, 7..7]),
        (
            "a\nXXb\nYY",
            vec![ProjectionSpan::hide(2..4), ProjectionSpan::hide(6..8)],
            vec![0..2, 2..4, 4..6, 6..8, 8..8],
        ),
        (
            "abcdefgh",
            vec![ProjectionSpan::replace(0..8, "\n")],
            vec![0..2, 2..4, 4..6, 6..8],
        ),
        (
            "abcdefgh",
            vec![ProjectionSpan::mapped(
                0..8,
                "a\nb\n",
                vec![
                    ProjectionMapping::new(0..1, 0..4),
                    ProjectionMapping::new(1..2, 0..4),
                    ProjectionMapping::new(2..3, 4..8),
                    ProjectionMapping::new(3..4, 4..8),
                ],
            )],
            vec![0..4, 4..8],
        ),
        (
            "# 中文🙂\n- one\n  two\n\nlast\n",
            vec![ProjectionSpan::hide(0..2)],
            vec![0..13, 13..25, 26..31],
        ),
    ] {
        let mut model = DocumentModel::new(
            source,
            vec![DocumentRegion::new(
                "source",
                0..source.len(),
                EditPolicy::Readonly,
            )],
        )
        .unwrap();
        // Reverse input to exercise DocumentStyles' canonical ordering too.
        let styles = ranges
            .into_iter()
            .enumerate()
            .map(|(ix, range)| paragraph(range, ix))
            .rev()
            .collect();
        model
            .set_rich_presentation(
                DocumentProjection::new(source.len(), spans).unwrap(),
                vec![],
                DocumentStyles::new(styles, vec![]),
            )
            .unwrap();
        assert_first_match(&model);
    }
}

fn fragment(id: &'static str, heading: &str) -> DocumentSnapshot<&'static str> {
    let text = format!("# {heading}\n- item🙂\n");
    let line_end = text.find('\n').unwrap() + 1;
    let styles = DocumentStyles::new(
        vec![
            paragraph(0..line_end, 1),
            paragraph(line_end..text.len(), 2),
        ],
        vec![
            DocumentInlineStyle::new(
                2..line_end - 1,
                HighlightStyle {
                    font_weight: Some(gpui::FontWeight::BOLD),
                    ..Default::default()
                },
            )
            .font_family("monospace"),
        ],
    );
    DocumentSnapshot::new(
        text.clone(),
        vec![DocumentRegion::new(id, 0..text.len(), EditPolicy::Readonly)],
        DocumentProjection::new(text.len(), vec![ProjectionSpan::hide(0..2)]).unwrap(),
        vec![],
        styles,
    )
}

#[gpui::test]
fn styled_splices_and_local_updates_match_full_rebuild(cx: &mut gpui::TestAppContext) {
    let (document, mut cx) = super::tests::document_view(cx);
    cx.update(|window, cx| {
        document.update(cx, |doc, cx| {
            doc.reset(
                DocumentSnapshot::new(
                    "first\nlast\n",
                    vec![
                        DocumentRegion::new("first", 0..6, EditPolicy::Readonly),
                        DocumentRegion::new("last", 6..11, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 11..11, EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(11),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
            doc.set_selection(11..11, false, cx);
            doc.replace_text_in_range(None, "草稿🙂", window, cx);
            let selection = doc.selected_positions().unwrap();
            let undo = doc.undo.undo.len();
            for (first, end, replacement) in [
                ("first", "last", fragment("first", "中文🙂")),
                ("last", "last", fragment("middle", "inserted")),
                ("last", "draft", fragment("last", "tail")),
                ("middle", "last", fragment("middle", "changed\nsecond line")),
            ] {
                doc.apply_host_splice(doc.revision(), Some(&first), Some(&end), replacement, cx)
                    .unwrap();
                let mut rebuilt = DocumentModel::new(doc.text(), doc.regions().to_vec()).unwrap();
                rebuilt
                    .set_rich_presentation(
                        doc.model.projection.clone(),
                        vec![],
                        doc.styles().clone(),
                    )
                    .unwrap();
                assert_eq!(doc.model.resolved_styles, rebuilt.resolved_styles);
                assert_eq!(doc.layout_items, rebuilt.layout_items(&[]));
                assert_eq!(doc.selected_positions().unwrap(), selection);
                assert_eq!(doc.undo.undo.len(), undo);
                assert_first_match(&doc.model);
            }
            doc.set_node_inline_styles(&"middle", vec![], cx).unwrap();
            let layout = doc.layout_items.clone();
            doc.set_rich_presentation(
                doc.model.projection.clone(),
                vec![],
                doc.styles().clone(),
                cx,
            )
            .unwrap();
            assert_eq!(doc.layout_items, layout);
            let text = doc.text();
            let revision = doc.revision();
            let styles = doc.styles().clone();
            let mut invalid = fragment("last", "rejected");
            invalid.styles = DocumentStyles::new(vec![paragraph(0..1, 0)], vec![]);
            assert!(matches!(
                doc.apply_host_splice(revision, Some(&"last"), Some(&"draft"), invalid, cx),
                Err(HostTransactionError::InvalidFragment(
                    PresentationError::Styles(_)
                ))
            ));
            assert_eq!(doc.text(), text);
            assert_eq!(doc.revision(), revision);
            assert_eq!(doc.styles(), &styles);
            assert_eq!(doc.layout_items, layout);
            assert_eq!(doc.selected_positions().unwrap(), selection);
            doc.undo(&Undo, window, cx);
            assert!(doc.text().ends_with("- item🙂\n"));
            doc.redo(&Redo, window, cx);
            assert_eq!(doc.text(), text);
            assert_first_match(&doc.model);
        });
        let _ = window.draw(cx);
    });
}
