//! Native shaping and real DocumentState layout, without physical window input.
//! cargo test -p gpui-base --test text_width
#[path = "text_width/projection.rs"]
mod projection;
#[path = "text_width/scaling.rs"]
mod scaling;

use std::{cell::RefCell, rc::Rc};

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, TestApp, TextRun,
    TruncateFrom, Window, div, font, px,
};
use gpui_base::{
    Theme,
    document::{DocumentRegion, DocumentState, EditPolicy, TextRows},
};
use unicode_segmentation::UnicodeSegmentation as _;

struct Root {
    document: Entity<DocumentState<u8>>,
    family: &'static str,
    width: gpui::Pixels,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(self.width)
            .h(px(8000.))
            .font_family(self.family)
            .text_size(px(14.))
            .child(self.document.clone())
    }
}

fn main() {
    let mut app = TestApp::with_text_system(gpui_platform::current_platform(true).text_system());
    app.update(|cx| {
        cx.set_global(Theme::default());
        gpui_base::init(cx);
    });
    app.text_system()
        .add_fonts(vec![
            include_bytes!("../../story-web/fonts/JetBrainsMono-Regular.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../story-web/fonts/NotoSansSC-Regular-subset.ttf")
                .as_slice()
                .into(),
        ])
        .unwrap();
    projection::check(&mut app);
    scaling::check(&mut app);
    let texts = [
        "\tfn\tmain()\t{\t}\tlet\tx\t=\t1;\t".repeat(12),
        "family 👨\u{200d}👩\u{200d}👧\u{200d}👦 skin 👍🏽 flag 🏳\u{fe0f}\u{200d}🌈 ".repeat(12),
        "e\u{301}a\u{308}n\u{303}o\u{302} Z\u{335}a\u{336}l\u{337}g\u{338}o ".repeat(24),
        "中文的长段落，包含标点；English words 12345 と日本語のテキスト。".repeat(12),
        "指终端网格中对 Unicode 字符的正确处理，例如中文、emoji、组合字符与全角字符。测试会确认这些字符的显示宽度、光标/文本选择范围及滚动历史不会按 UTF-8 字节错误切分或错位。".into(),
        " （中）更多文字，".repeat(12),
        format!("{{{}}}", (0..60).map(|i| format!("\"k{i}\":\"v {i}\"")).collect::<Vec<_>>().join(",")),
    ];
    let families = [
        "JetBrains Mono",
        #[cfg(target_os = "macos")]
        ".SystemUIFont",
    ];
    for family in families {
        for text in &texts {
            let rows = Rc::new(RefCell::new(Vec::new()));
            let captured = rows.clone();
            let mut window = app.open_window(|_, cx| {
                let document = cx.new(|cx| {
                    let mut doc = DocumentState::new(
                        text.clone(),
                        vec![DocumentRegion::new(0, 0..text.len(), EditPolicy::Readonly)],
                        cx,
                    )
                    .unwrap();
                    doc.set_text_rows(Some(TextRows::new()), cx);
                    doc.set_text_renderer(
                        move |context, element, _, _| {
                            captured.borrow_mut().push(context.source());
                            element
                        },
                        cx,
                    );
                    doc
                });
                Root {
                    document,
                    family,
                    width: px(600.),
                }
            });
            // Both shrinking and expanding the same retained document.
            for width in [px(600.), px(31.), px(120.), px(310.), px(644.)] {
                window.update(|root, window, cx| {
                    root.width = width;
                    window.resize(gpui::size(width, px(8000.)));
                    cx.notify();
                });
                // The first frame prepares row breaks, the next publishes layout.
                for _ in 0..2 {
                    rows.borrow_mut().clear();
                    window.update(|_, window, _| window.refresh());
                    window.draw();
                }
                let mut rendered = rows.borrow().clone();
                rendered.sort_by_key(|row| row.start);
                rendered.dedup();
                assert!(
                    rendered.len() > 1,
                    "expected wrapped rows for {family} {width:?}"
                );
                assert_eq!(rendered.first().unwrap().start, 0);
                assert_eq!(rendered.last().unwrap().end, text.len());
                if text.starts_with(" （中）") {
                    assert!(
                        !text[rendered[0].clone()].trim().is_empty(),
                        "indentation became a blank row"
                    );
                }
                for pair in rendered.windows(2) {
                    assert_eq!(pair[0].end, pair[1].start);
                }
                window.update(|_, window, _| {
                    for range in rendered {
                        let row = &text[range.clone()];
                        assert!(
                            range.end == text.len()
                                || text.grapheme_indices(true).any(|(ix, _)| ix == range.end)
                        );
                        let drawn = window
                            .text_system()
                            .shape_line(
                                row.to_owned().into(),
                                px(14.),
                                &[TextRun {
                                    len: row.len(),
                                    font: font(family),
                                    ..Default::default()
                                }],
                                None,
                            )
                            .width;
                        assert!(
                            drawn <= width + px(0.01) || row.graphemes(true).count() == 1,
                            "{family}: {row:?} draws {drawn:?} in {width:?}"
                        );
                    }
                });
            }
        }
    }
    let text = "录会话回放、混合文档回放、提交接受/拒绝后的草稿和撤销语义、终端焦点/选区删除、Unicode 终端选择与滚动历史验证。e\u{301}👩\u{200d}💻🇨🇳";
    let system = gpui::WindowTextSystem::new(app.text_system().clone());
    for family in families {
        let mut wrapper = app.text_system().line_wrapper(font(family), px(12.25));
        let runs = [
            TextRun {
                len: text.find("Unicode").unwrap(),
                font: font(family),
                ..Default::default()
            },
            TextRun {
                len: text.len() - text.find("Unicode").unwrap(),
                font: font(family).bold(),
                ..Default::default()
            },
        ];
        for direction in [TruncateFrom::End, TruncateFrom::Start, TruncateFrom::Middle] {
            for width in [px(40.), px(120.), px(310.), px(600.), px(10000.)] {
                let (shown, styles) =
                    wrapper.truncate_line(text.into(), width, "…", &runs, direction);
                assert_eq!(styles.iter().map(|run| run.len).sum::<usize>(), shown.len());
                let drawn = system
                    .shape_line(shown.clone(), px(12.25), &styles, None)
                    .width;
                assert!(
                    drawn <= width + px(0.01),
                    "{shown:?} draws {drawn:?} in {width:?}"
                );
                if width == px(10000.) {
                    assert_eq!(shown.as_ref(), text);
                    assert!(matches!(styles, std::borrow::Cow::Borrowed(_)));
                } else {
                    let ix = shown.find('…').expect("truncation affix");
                    let prefix = &shown[..ix];
                    let suffix = &shown[ix + '…'.len_utf8()..];
                    assert!(text.starts_with(prefix) && text.ends_with(suffix));
                    for cut in [prefix.len(), text.len() - suffix.len()] {
                        assert!(
                            cut == text.len()
                                || text.grapheme_indices(true).any(|(ix, _)| ix == cut)
                        );
                    }
                }
            }
        }
    }
    println!("PASS: document row widths after resize and styled truncation");
}
