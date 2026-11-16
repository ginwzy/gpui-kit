use crate::{Root, WindowExt};
use gpui::{
    AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, TestAppContext, Window, div,
};

struct Content(FocusHandle);
impl Render for Content {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.0)
            .children(Root::render_dialog_layer(window, cx))
    }
}

#[gpui::test]
fn removing_an_owned_dialog_preserves_newer_layers_without_stale_focus(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let content = cx.new(|cx| Content(cx.focus_handle()));
        content.read(cx).0.clone().focus(window, cx);
        Root::new(content, window, cx)
    });
    let first =
        cx.update(|window, cx| window.open_dialog(cx, |dialog, _, _| dialog.child("first")));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let second =
        cx.update(|window, cx| window.open_dialog(cx, |dialog, _, _| dialog.child("second")));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        Root::update(window, cx, |root, window, cx| {
            root.remove_dialog(&first, window, cx)
        });
        assert!(second.is_focused(window));
        assert_eq!(root.read(cx).active_dialogs.len(), 1);
        assert!(
            root.read(cx).active_dialogs[0]
                .previous_focused_handle
                .is_none()
        );
        // A repeated removal cannot close the next dialog.
        Root::update(window, cx, |root, window, cx| {
            root.remove_dialog(&first, window, cx)
        });
        assert_eq!(root.read(cx).active_dialogs.len(), 1);
        window.close_dialog(cx);
        assert!(!first.is_focused(window));
        assert!(root.read(cx).active_dialogs.is_empty());
    });
}

#[gpui::test]
fn removing_a_dialog_cancels_pending_focus_restoration(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let content = cx.new(|cx| Content(cx.focus_handle()));
        Root::new(content, window, cx)
    });
    let first =
        cx.update(|window, cx| window.open_dialog(cx, |dialog, _, _| dialog.child("first")));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let replacement = cx.update(|window, cx| {
        window.open_dialog(cx, |dialog, _, _| dialog.child("second"));
        Root::update(window, cx, |root, window, cx| {
            root.defer_close_dialog(window, cx);
            root.remove_dialog(&first, window, cx);
        });
        window.open_dialog(cx, |dialog, _, _| dialog.child("replacement"))
    });
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    cx.update(|window, _| assert!(replacement.is_focused(window)));
}

#[gpui::test]
fn removing_before_first_layout_does_not_keep_an_obsolete_focus(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let content = cx.new(|cx| Content(cx.focus_handle()));
        Root::new(content, window, cx)
    });
    cx.update(|window, cx| {
        let focus = window.open_dialog(cx, |dialog, _, _| dialog.child("first"));
        Root::update(window, cx, |root, window, cx| {
            root.remove_dialog(&focus, window, cx)
        });
        assert!(root.read(cx).active_dialogs.is_empty());
        assert!(!focus.is_focused(window));
    });
}
