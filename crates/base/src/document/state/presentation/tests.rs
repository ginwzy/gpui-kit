use super::super::{DocumentLayoutItem, DocumentModel, DocumentTextPresentation};
use crate::document::{
    DocumentInlineStyle, DocumentParagraphStyle, DocumentProjection, DocumentRegion,
    DocumentStyles, EditPolicy, ProjectionSpan,
};
use gpui::{FontStyle, FontWeight, HighlightStyle, blue, red};

#[test]
fn empty_rows_keep_styles_only_inside_their_paragraph() {
    for (source, projection) in [
        ("item\n\n", DocumentProjection::identity(6)),
        (
            "x",
            DocumentProjection::new(1, vec![ProjectionSpan::replace(0..1, "item\n\n")]).unwrap(),
        ),
    ] {
        let mut model = DocumentModel::new(
            source,
            vec![
                DocumentRegion::new("history", 0..source.len(), EditPolicy::Readonly),
                DocumentRegion::new("draft", source.len()..source.len(), EditPolicy::Editable),
            ],
        )
        .unwrap();
        model
            .set_rich_presentation(
                projection,
                vec![],
                DocumentStyles::new(
                    vec![
                        DocumentParagraphStyle::new(0..source.len(), Default::default())
                            .with_hanging_marker("• "),
                    ],
                    vec![],
                ),
            )
            .unwrap();
        let styled: Vec<_> = model
            .layout_items(None)
            .into_iter()
            .filter_map(|item| match item {
                DocumentLayoutItem::Text { presentation, .. } => Some(presentation.is_some()),
                _ => None,
            })
            .collect();
        // The paragraph's internal blank line keeps its style; the empty draft does not.
        assert_eq!(styled, [true, true, false]);
    }
}

#[test]
fn projected_styles_merge_fields_and_split_the_same_paint_and_font_intervals() {
    let mut model = DocumentModel::new(
        "abcd",
        vec![DocumentRegion::new((), 0..4, EditPolicy::Readonly)],
    )
    .unwrap();
    model
        .set_rich_presentation(
            DocumentProjection::new(4, vec![ProjectionSpan::replace(0..4, "中🙂")]).unwrap(),
            vec![],
            DocumentStyles::new(
                vec![],
                vec![
                    DocumentInlineStyle::new(
                        0..2,
                        HighlightStyle {
                            color: Some(red()),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        },
                    )
                    .font_family("mono"),
                    DocumentInlineStyle::new(
                        2..4,
                        HighlightStyle {
                            background_color: Some(blue()),
                            font_style: Some(FontStyle::Italic),
                            ..Default::default()
                        },
                    ),
                ],
            ),
        )
        .unwrap();
    let text = model.display_text();
    let DocumentLayoutItem::Text {
        presentation: Some(presentation),
        ..
    } = model.text_layout_item(text, 0..text.len(), 0..4, 0..text.len(), 0)
    else {
        panic!("styled display item")
    };
    assert_eq!(
        presentation.highlights,
        vec![(
            0..7,
            HighlightStyle {
                color: Some(red()),
                background_color: Some(blue()),
                font_weight: Some(FontWeight::BOLD),
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            }
        )]
    );
    assert_eq!(
        presentation.font_family_overrides,
        vec![(0..7, "mono".into())]
    );
}

#[test]
fn family_edges_split_paint_and_nested_layers_restore_inherited_fields() {
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    let italic = HighlightStyle {
        font_style: Some(FontStyle::Italic),
        ..Default::default()
    };
    let mut presentation = DocumentTextPresentation {
        highlights: vec![(0..9, bold), (3..6, italic)],
        font_family_overrides: vec![(0..3, "mono".into()), (6..9, "mono".into())],
        ..Default::default()
    };
    presentation.resolve_inline(12);
    assert_eq!(
        presentation.highlights,
        vec![
            (0..3, bold),
            (
                3..6,
                HighlightStyle {
                    font_style: italic.font_style,
                    ..bold
                }
            ),
            (6..9, bold),
            (9..12, HighlightStyle::default()),
        ]
    );
    assert_eq!(
        presentation.font_family_overrides,
        vec![(0..3, "mono".into()), (6..9, "mono".into()),]
    );
}

#[test]
fn a_line_owns_the_markup_its_text_hides_at_its_end() {
    let source = "**bold**\nnext";
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
                vec![ProjectionSpan::hide(0..2), ProjectionSpan::hide(6..8)],
            )
            .unwrap(),
            vec![],
            DocumentStyles::new(vec![], vec![]),
        )
        .unwrap();
    let text = model.display_text();
    assert_eq!(text, "bold\nnext");
    let source_of = |display| {
        let DocumentLayoutItem::Text { source, .. } =
            model.text_layout_item(text, display, 0..source.len(), 0..4, 0)
        else {
            panic!("text item")
        };
        source
    };
    // The whole line ends after the closing delimiter, so selecting or
    // clicking to its end reaches the source a copy needs.
    assert_eq!(source_of(0..4), 2..8);
    // A soft-wrapped row ends where its text does; the line's last row owns
    // the delimiter.
    assert_eq!(source_of(0..2), 2..4);
    assert_eq!(source_of(2..4), 4..8);
}
