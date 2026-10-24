use super::Root;
use gpui::{AppContext, FontStyle, FontWeight, HighlightStyle, TestApp, px};
use gpui_base::document::{
    DocumentInlineStyle, DocumentProjection, DocumentRegion, DocumentState, DocumentStyles,
    EditPolicy, ProjectionMapping, ProjectionSpan, TextRows,
};

pub(super) fn check(app: &mut TestApp) {
    let source = "`ab`） 清理影响";
    // The two displayed code scalars share their whole source event, exactly
    // as Markdown's inline-code projection. A bold closing bracket and plain
    // CJK follow it: double-counting the code puts a run edge inside 清.
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::mapped(
            0..source.len(),
            "ab） 清理影响",
            vec![
                ProjectionMapping::new(0..1, 0..4),
                ProjectionMapping::new(1..2, 0..4),
                ProjectionMapping::new(2..18, 4..source.len()),
            ],
        )],
    )
    .unwrap();
    for rows in [false, true] {
        let projection = projection.clone();
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
                doc.set_rich_presentation(
                    projection,
                    vec![],
                    DocumentStyles::new(
                        vec![],
                        vec![
                            DocumentInlineStyle::new(
                                0..4,
                                HighlightStyle {
                                    font_style: Some(FontStyle::Italic),
                                    ..Default::default()
                                },
                            )
                            .font_family("JetBrains Mono"),
                            DocumentInlineStyle::new(
                                4..7,
                                HighlightStyle {
                                    font_weight: Some(FontWeight::BOLD),
                                    ..Default::default()
                                },
                            ),
                        ],
                    ),
                    cx,
                )
                .unwrap();
                if rows {
                    doc.set_text_rows(Some(TextRows::new()), cx);
                }
                doc
            });
            Root {
                document,
                family: "JetBrains Mono",
                width: px(320.),
            }
        });
        for width in [320., 31., 120., 320.] {
            window.update(|root, window, cx| {
                root.width = px(width);
                window.resize(gpui::size(px(width), px(8000.)));
                cx.notify();
            });
            for _ in 0..2 {
                window.update(|_, window, _| window.refresh());
                window.draw();
            }
        }
    }
}
