use super::super::{DocumentLayoutItem, DocumentModel, DocumentTextPresentation};
use crate::document::{
    DocumentInlineStyle, DocumentProjection, DocumentRegion, DocumentStyles, EditPolicy,
    ProjectionSpan,
};
use gpui::{FontStyle, FontWeight, HighlightStyle, blue, red};

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
