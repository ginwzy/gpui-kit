use super::*;
use gpui::{AppContext as _, Context, Render, Styled as _, point, px};
use std::{cell::RefCell, rc::Rc};

/// Element state can disappear when a virtual-list trigger scrolls away or
/// its panel closes. Its deferred registration must end with that state.
#[gpui::test]
fn a_state_dropped_while_open_closes_the_deferred_context(cx: &mut gpui::TestAppContext) {
    let state = cx.update(|cx| {
        GlobalState::init(cx);
        let state = cx.new(|cx| PopoverState::new(false, cx));
        state.update(cx, |state, cx| state.set_open(true, cx));
        assert!(GlobalState::is_in_deferred_context(cx));
        state
    });

    cx.update(|_| drop(state));
    cx.update(|cx| assert!(!GlobalState::is_in_deferred_context(cx)));
}

#[gpui::test]
fn open_state_registers_and_unregisters_deferred_context(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        GlobalState::init(cx);
        let state = cx.new(|cx| PopoverState::new(false, cx));
        state.update(cx, |state, cx| state.set_open(true, cx));
        assert!(state.read(cx).is_open());
        assert!(GlobalState::is_in_deferred_context(cx));
        state.update(cx, |state, cx| state.set_open(false, cx));
        assert!(!state.read(cx).is_open());
        assert!(!GlobalState::is_in_deferred_context(cx));
    });
}

struct PopoverHarness {
    changes: Rc<RefCell<Vec<bool>>>,
    default_open: bool,
}

struct KeyboardPopoverHarness {
    trigger_focus: FocusHandle,
}

impl Render for KeyboardPopoverHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Popover::new("keyboard-popover")
            .trigger(
                crate::Button::new("keyboard-trigger")
                    .track_focus(&self.trigger_focus)
                    .child("Open"),
            )
            .content(|_, _, _| {
                div()
                    .debug_selector(|| "keyboard-popover-content".into())
                    .size(px(40.))
            })
    }
}

impl Render for PopoverHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let changes = self.changes.clone();
        Popover::new("base-popover")
            .default_open(self.default_open)
            .trigger_with(|_, _, _| div().child("Open").into_any_element())
            .content(|_, _, _| {
                div()
                    .debug_selector(|| "base-popover-content".into())
                    .size(px(40.))
            })
            .on_open_change(move |open, _, _| changes.borrow_mut().push(*open))
    }
}

#[gpui::test]
fn unstyled_popover_owns_pointer_open_and_outside_dismiss(cx: &mut gpui::TestAppContext) {
    cx.update(crate::init);
    let changes = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let changes = changes.clone();
        move |_, _| PopoverHarness {
            changes,
            default_open: false,
        }
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_click(point(px(20.), px(10.)), Default::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("base-popover-content").is_some());
    cx.simulate_click(point(px(300.), px(300.)), Default::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("base-popover-content").is_none());
    assert_eq!(&*changes.borrow(), &[true, false]);
}

#[gpui::test]
fn default_open_renders_content_without_activation(cx: &mut gpui::TestAppContext) {
    cx.update(crate::init);
    let (_, cx) = cx.add_window_view(|_, _| PopoverHarness {
        changes: Rc::new(RefCell::new(Vec::new())),
        default_open: true,
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("base-popover-content").is_some());
}

#[gpui::test]
fn keyboard_activation_opens_the_popover(cx: &mut gpui::TestAppContext) {
    cx.update(crate::init);
    let (view, cx) = cx.add_window_view(|_, cx| KeyboardPopoverHarness {
        trigger_focus: cx.focus_handle(),
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        let focus = view.read(cx).trigger_focus.clone();
        focus.focus(window, cx);
    });
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("keyboard-popover-content").is_some());
}

struct RemovalHarness {
    visible: bool,
}

impl Render for RemovalHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().when(self.visible, |root| {
            root.child(
                Popover::new("removable-popover")
                    .trigger_with(|_, _, _| div().child("Open").into_any_element())
                    .content(|_, _, _| div().size(px(40.))),
            )
        })
    }
}

#[gpui::test]
fn removing_an_open_popover_releases_it_and_restores_only_its_own_focus(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(crate::init);
    let (view, cx) = cx.add_window_view(|_, _| RemovalHarness { visible: true });
    let (original, elsewhere) = cx.update(|_, cx| (cx.focus_handle(), cx.focus_handle()));
    for move_focus in [false, true] {
        view.update(cx, |view, cx| {
            view.visible = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            original.focus(window, cx);
        });
        cx.simulate_click(point(px(20.), px(10.)), Default::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(GlobalState::is_in_deferred_context(cx));
            assert!(!original.is_focused(window));
            if move_focus {
                elsewhere.focus(window, cx);
            }
        });
        view.update(cx, |view, cx| {
            view.visible = false;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(!GlobalState::is_in_deferred_context(cx));
            let expected = if move_focus { &elsewhere } else { &original };
            assert!(expected.is_focused(window));
        });
    }
}
