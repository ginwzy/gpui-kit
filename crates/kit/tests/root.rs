//! `Root` owns the window's overlay layers: a view that never mentions them
//! still gets its dialogs, and a view that places them itself gets each once.

#[cfg(target_os = "macos")]
use gpui_kit::AnyWindowHandle;
use gpui_kit::base::actions::Quit;
use gpui_kit::component::{Root, WindowExt as _, notification::Notification};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, Window, WindowOptions, div, px, size,
};
use std::{cell::Cell, rc::Rc};

/// A view that never mentions the layers.
struct PlainView;

impl Render for PlainView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("plain")
    }
}

/// A view that places the layers itself, the way applications did before
/// `Root` rendered them.
struct LayeredView;

impl Render for LayeredView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child("layered")
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

fn open_and_notify(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, _| dialog.title("Hello"));
    window.push_notification(Notification::new().message("Saved").autohide(false), cx);
}

#[gpui_kit::test]
fn a_root_renders_the_layers_a_plain_view_leaves_out(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
        let view = cx.new(|_| PlainView);
        Root::new(view, window, cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none());
        open_and_notify(window, cx);
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("dialog").visible(),
            "the dialog must reach the screen without the view rendering the layer"
        );
        assert!(window.find("notification").visible());
    })
    .unwrap();
}

/// `find` panics on an ambiguous id, so a layer rendered twice fails this
/// test by itself.
#[gpui_kit::test]
fn a_view_that_places_the_layers_gets_each_of_them_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
        let view = cx.new(|_| LayeredView);
        Root::new(view, window, cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_and_notify(window, cx);
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("dialog").visible());
        assert!(window.find("notification").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn open_window_wraps_the_view_in_a_root_and_returns_the_view(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, view) = cx
        .update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| PlainView))
        })
        .expect("open_window");
    let root = handle.downcast::<Root>().expect("the root view is a Root");
    let root_view = root.read_with(cx, |root, _| root.view().clone()).unwrap();
    assert_eq!(root_view.entity_id(), view.entity_id());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        open_and_notify(window, cx);
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("dialog").visible());
        assert!(window.find("notification").visible());
    })
    .unwrap();
}

/// The platform's quit shortcut reaches `Quit` in a window nobody bound keys
/// in. The test platform's `quit` is a no-op, so the test watches the action.
#[gpui_kit::test]
fn the_platform_quit_shortcut_quits_from_any_kit_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let quit = Rc::new(Cell::new(0));
    cx.update({
        let quit = quit.clone();
        move |cx| {
            cx.on_action(move |_: &Quit, _| quit.set(quit.get() + 1));
        }
    });
    let (handle, _) = cx
        .update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| PlainView))
        })
        .expect("open_window");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-q"
            } else {
                "alt-f4"
            },
            cx,
        );
    })
    .unwrap();
    assert_eq!(quit.get(), 1);
}

/// `cmd-w` closes the active window and leaves the others open, as
/// `File › Close` does.
#[cfg(target_os = "macos")]
#[gpui_kit::test]
fn cmd_w_closes_the_active_window_only(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let open = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| PlainView))
        })
        .expect("open_window")
        .0
    };
    let first = open(cx);
    let second = open(cx);
    assert_eq!(cx.windows().len(), 2);

    // The window that receives the key is the active one; the test platform
    // does not activate a window on its own.
    cx.update_window(second, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let left: Vec<AnyWindowHandle> = cx.windows();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].window_id(), first.window_id());
}
