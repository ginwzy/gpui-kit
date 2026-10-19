//! Run with `cargo run -p gpui-base --example inline_document`.
//! The document owns all text, selection, geometry and undo. This host only
//! supplies object presentation and translates structure requests into nodes.
use gpui::{
    App, AppContext, Bounds, Context, Entity, Focusable, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, WindowBounds, WindowOptions, div, px, rems, size,
};
use gpui_base::{Button, Theme, document::*};

struct InlineDocument {
    document: Entity<DocumentState<&'static str>>,
    font_size: gpui::Pixels,
    _events: Subscription,
}

fn object_size(id: &str) -> InlineSize {
    match id {
        "a" => InlineSize::new(rems(5.), rems(1.5), rems(1.1)),
        "b" => InlineSize::new(rems(6.), rems(2.), rems(1.6)),
        _ => InlineSize::new(rems(7.), rems(1.5), rems(1.1)),
    }
}

fn snapshot(
    text: String,
    regions: Vec<DocumentRegion<&'static str>>,
) -> DocumentSnapshot<&'static str> {
    let blocks: Vec<_> = regions
        .iter()
        .filter(|region| region.policy() == EditPolicy::Atomic)
        .map(|region| {
            DocumentBlock::new(*region.id(), region.range())
                .with_inline_size(object_size(region.id()))
                .replaceable(true)
        })
        .collect();
    let projection = DocumentProjection::new(
        text.len(),
        blocks
            .iter()
            .map(|block| ProjectionSpan::replace(block.source(), "\u{fffc}"))
            .collect(),
    )
    .unwrap();
    DocumentSnapshot::new(text, regions, projection, blocks, DocumentStyles::default())
}

fn initial() -> DocumentSnapshot<&'static str> {
    let mut text = String::new();
    let mut regions = Vec::new();
    for (id, value, policy) in [
        ("history", "Readonly heading\n", EditPolicy::Readonly),
        ("lead", "中文🙂 before ", EditPolicy::Editable),
        ("a", "<object-a>", EditPolicy::Atomic),
        ("between", "", EditPolicy::Editable),
        ("b", "<object-b>", EditPolicy::Atomic),
        (
            "middle",
            " after two objects, with text that wraps.\nNext line ",
            EditPolicy::Editable,
        ),
        ("c", "<object-c>", EditPolicy::Atomic),
        ("tail", " and the end.\n", EditPolicy::Editable),
    ] {
        let start = text.len();
        text.push_str(value);
        regions.push(DocumentRegion::new(id, start..text.len(), policy));
    }
    snapshot(text, regions).selection(DocumentPosition::new("lead", 0, Affinity::After))
}

fn replace(
    document: &mut DocumentState<&'static str>,
    request: &StructureReplacement<&'static str>,
    cx: &mut Context<DocumentState<&'static str>>,
) {
    let edit = &request.transaction().edits()[0];
    let range = edit.range();
    let old = document.regions();
    let first = old
        .iter()
        .position(|region| {
            region.policy() == EditPolicy::Editable
                && region.range().start <= range.start
                && range.start <= region.range().end
        })
        .or_else(|| {
            old.iter()
                .position(|region| region.range().start == range.start)
        })
        .unwrap();
    let mut last = first;
    while last + 1 < old.len() && old[last + 1].range().start < range.end {
        last += 1;
    }
    if old[last].policy() == EditPolicy::Atomic
        && last + 1 < old.len()
        && old[last + 1].policy() == EditPolicy::Editable
    {
        last += 1;
    }
    let start = old[first].range().start;
    let delta = edit.replacement().len() as isize - range.len() as isize;
    let end = old[last].range().end.checked_add_signed(delta).unwrap();
    let id = if old[first].policy() == EditPolicy::Editable {
        *old[first].id()
    } else {
        "replacement"
    };
    let mut regions = old[..first].to_vec();
    regions.push(DocumentRegion::new(id, start..end, EditPolicy::Editable));
    regions.extend(old[last + 1..].iter().map(|region| {
        DocumentRegion::new(
            *region.id(),
            region.range().start.checked_add_signed(delta).unwrap()
                ..region.range().end.checked_add_signed(delta).unwrap(),
            region.policy(),
        )
    }));
    let mut text = document.text();
    text.replace_range(range.clone(), edit.replacement());
    let next = snapshot(text, regions).selection(DocumentPosition::new(
        id,
        range.start - start + edit.replacement().len(),
        Affinity::After,
    ));
    document
        .apply_structure_replacement(request, next, start, cx)
        .unwrap();
}

fn replay(
    document: &mut DocumentState<&'static str>,
    request: &StructureReplay<&'static str>,
    cx: &mut Context<DocumentState<&'static str>>,
) {
    let start = document
        .regions()
        .iter()
        .find(|region| region.id() == request.start_node())
        .unwrap()
        .range()
        .start;
    let mut text = document.text()[..start].to_owned();
    text.push_str(request.target().text());
    let mut regions: Vec<_> = document
        .regions()
        .iter()
        .filter(|region| region.range().start < start)
        .cloned()
        .collect();
    regions.extend(request.target().regions().iter().map(|region| {
        DocumentRegion::new(
            *region.id(),
            region.range().start + start..region.range().end + start,
            region.policy(),
        )
    }));
    let mut next = snapshot(text, regions);
    if let Some((anchor, head)) = request.selection() {
        next = next.selection_range(anchor.clone(), head.clone());
    }
    document
        .apply_structure_replay(next, start, request.is_undo(), cx)
        .unwrap();
}

impl InlineDocument {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                "",
                vec![DocumentRegion::new("lead", 0..0, EditPolicy::Editable)],
                cx,
            )
            .unwrap();
            document.set_text_rows(Some(TextRows::new()), cx);
            document.set_block_renderer(
                |id, _, cx| {
                    let theme = Theme::global(cx);
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.tokens.colors.muted)
                        .border_1()
                        .border_color(theme.tokens.colors.border)
                        .rounded(theme.tokens.radius.sm)
                        .child(format!("Object {}", id.to_uppercase()))
                        .into_any_element()
                },
                cx,
            );
            document.reset(initial(), cx).unwrap();
            document
        });
        let events = cx.subscribe(&document, |_, document, event, cx| match event {
            DocumentEvent::ReplaceRequested(request) => {
                document.update(cx, |document, cx| replace(document, request, cx))
            }
            DocumentEvent::StructureReplayRequested(request) => {
                document.update(cx, |document, cx| replay(document, request, cx))
            }
            _ => {}
        });
        document.focus_handle(cx).focus(window, cx);
        Self {
            document,
            font_size: px(16.),
            _events: events,
        }
    }
}

impl Render for InlineDocument {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(self.font_size);
        let theme = Theme::global(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .bg(theme.tokens.colors.background)
            .text_color(theme.tokens.colors.foreground)
            .text_size(rems(1.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("reset")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(theme.tokens.colors.border)
                            .child("Reset")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.document.update(cx, |document, cx| {
                                    document.reset(initial(), cx).unwrap()
                                });
                                this.document.focus_handle(cx).focus(window, cx);
                            })),
                    )
                    .child(
                        Button::new("zoom")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(theme.tokens.colors.border)
                            .child("Toggle text size")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.font_size = if this.font_size == px(16.) {
                                    px(22.)
                                } else {
                                    px(16.)
                                };
                                this.document.focus_handle(cx).focus(window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_color(theme.tokens.colors.muted_foreground)
                    .child("Resize, select across objects, type or paste, then undo and redo."),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.document.clone()),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_base::init(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(760.), px(440.)),
                    cx,
                ))),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Inline document".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| InlineDocument::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{EntityInputHandler, TestAppContext, VisualTestContext};

    #[gpui::test]
    fn native_host_replacement_is_one_undoable_edit(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            gpui_base::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| InlineDocument::new(window, cx))
            })
            .unwrap()
        });
        let document = window
            .update(cx, |host, _, _| host.document.clone())
            .unwrap();
        let mut view = VisualTestContext::from_window(window.into(), cx);
        view.update(|window, cx| {
            window.activate_window();
            let _ = window.draw(cx);
        });
        for (insertion, action) in [("中文🙂\n", 0), ("粘贴😀\n", 1), ("", 2)] {
            document.update(&mut view, |document, cx| {
                document.reset(initial(), cx).unwrap()
            });
            let (original, regions, range) = document.read_with(&view, |document, _| {
                let a = document
                    .regions()
                    .iter()
                    .find(|region| region.id() == &"a")
                    .unwrap()
                    .range();
                let b = document
                    .regions()
                    .iter()
                    .find(|region| region.id() == &"b")
                    .unwrap()
                    .range();
                (
                    document.text(),
                    document.regions().to_vec(),
                    a.start - 7..b.end + 6,
                )
            });
            let mut expected = original.clone();
            expected.replace_range(range.clone(), insertion);
            view.update(|window, cx| {
                document.update(cx, |document, cx| {
                    document.set_selection(range.clone(), false, cx);
                    if action == 0 {
                        document.replace_text_in_range(None, insertion, window, cx);
                    }
                })
            });
            if action == 1 {
                view.update(|_, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(insertion.into()))
                });
                view.simulate_keystrokes(if cfg!(target_os = "macos") {
                    "cmd-v"
                } else {
                    "ctrl-v"
                });
            } else if action == 2 {
                view.simulate_keystrokes("backspace");
            }
            view.run_until_parked();
            document.read_with(&view, |document, _| {
                assert_eq!(document.text(), expected);
                assert!(
                    !document
                        .regions()
                        .iter()
                        .any(|region| region.id() == &"a" || region.id() == &"b")
                );
                assert!(document.regions().iter().any(|region| region.id() == &"c"));
                assert!(document.selected_range().is_empty());
            });
            view.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-z"
            } else {
                "ctrl-z"
            });
            view.run_until_parked();
            document.read_with(&view, |document, _| {
                assert_eq!(document.text(), original);
                assert_eq!(document.regions(), regions);
                assert_eq!(document.selected_range(), range);
            });
            view.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-shift-z"
            } else {
                "ctrl-y"
            });
            view.run_until_parked();
            assert_eq!(
                document.read_with(&view, |document, _| document.text()),
                expected
            );
        }
    }
}
