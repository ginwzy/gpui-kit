use gpui::{
    AppContext, Axis, Context, Entity, InteractiveElement, IntoElement, Modifiers, MouseButton,
    ParentElement, Pixels, Render, Styled, TestAppContext, VisualTestContext, Window, div, point,
    px,
};

use crate::resizable::{ResizablePanelGroup, ResizableState, resizable_panel};

struct Harness {
    axis: Axis,
    extent: Pixels,
    preferred: Pixels,
    maximum: Pixels,
    visible: bool,
    commits: usize,
    state: Entity<ResizableState>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity();
        let container = match self.axis {
            Axis::Horizontal => div().w(self.extent).h(px(100.)),
            Axis::Vertical => div().w(px(100.)).h(self.extent),
        };
        container.child(
            ResizablePanelGroup::new("controlled")
                .axis(self.axis)
                .with_state(&self.state)
                .controlled()
                .child(
                    resizable_panel()
                        .size(self.preferred)
                        .size_range(px(100.)..self.maximum)
                        .visible(self.visible)
                        .child(div().size_full().debug_selector(|| "fixed".into())),
                )
                .child(
                    resizable_panel()
                        .child(div().size_full().debug_selector(|| "remainder".into())),
                )
                .on_resize(move |state, _, cx| {
                    let width = state.read(cx).sizes()[0];
                    owner.update(cx, |owner, cx| {
                        owner.preferred = width;
                        owner.commits += 1;
                        cx.notify();
                    });
                }),
        )
    }
}

fn harness(cx: &mut TestAppContext, axis: Axis) -> (Entity<Harness>, &mut VisualTestContext) {
    let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
    let (view, cx) = cx.add_window_view(|_, _| Harness {
        axis,
        extent: px(1000.),
        preferred: px(240.),
        maximum: px(400.),
        visible: true,
        commits: 0,
        state,
    });
    settle(cx);
    (view, cx)
}

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn extent(cx: &mut VisualTestContext, id: &'static str, axis: Axis) -> Pixels {
    let bounds = cx.debug_bounds(id).unwrap();
    match axis {
        Axis::Horizontal => bounds.size.width,
        Axis::Vertical => bounds.size.height,
    }
}

fn drag(cx: &mut VisualTestContext, axis: Axis, from: Pixels, to: Pixels) {
    let position = |value| match axis {
        Axis::Horizontal => point(value, px(50.)),
        Axis::Vertical => point(px(50.), value),
    };
    cx.simulate_mouse_down(
        position(from - px(2.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        position(from + px(10.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(position(to), Some(MouseButton::Left), Modifiers::default());
    settle(cx);
    assert!((extent(cx, "fixed", axis) - to).abs() < px(3.));
    cx.simulate_mouse_up(position(to), MouseButton::Left, Modifiers::default());
    settle(cx);
}

#[gpui::test]
fn controlled_size_survives_drag_container_resize_and_visibility(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, Axis::Horizontal);
    drag(cx, Axis::Horizontal, px(240.), px(300.));
    let preferred = view.read_with(cx, |view, _| view.preferred);
    assert!((preferred - px(300.)).abs() < px(3.));
    for width in [600., 1400., 800.] {
        view.update(cx, |view, cx| {
            view.extent = px(width);
            cx.notify();
        });
        settle(cx);
        assert_eq!(extent(cx, "fixed", Axis::Horizontal), preferred);
        assert_eq!(
            extent(cx, "remainder", Axis::Horizontal),
            px(width) - preferred
        );
        view.update(cx, |view, cx| {
            view.visible = false;
            cx.notify();
        });
        settle(cx);
        assert_eq!(extent(cx, "remainder", Axis::Horizontal), px(width));
        view.update(cx, |view, cx| {
            view.visible = true;
            cx.notify();
        });
        settle(cx);
        assert_eq!(extent(cx, "fixed", Axis::Horizontal), preferred);
    }
    view.read_with(cx, |view, _| {
        assert_eq!(view.preferred, preferred);
        assert_eq!(view.commits, 1);
    });
}

#[gpui::test]
fn external_size_and_temporary_constraints_do_not_emit_resize(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, Axis::Horizontal);
    view.update(cx, |view, cx| {
        view.preferred = px(350.);
        view.maximum = px(200.);
        cx.notify();
    });
    settle(cx);
    assert_eq!(extent(cx, "fixed", Axis::Horizontal), px(200.));
    view.read_with(cx, |view, cx| {
        assert_eq!(view.preferred, px(350.));
        assert_eq!(view.state.read(cx).sizes()[0], px(200.));
        assert_eq!(view.commits, 0);
    });
    view.update(cx, |view, cx| {
        view.maximum = px(400.);
        view.visible = false;
        view.preferred = px(320.);
        cx.notify();
    });
    settle(cx);
    view.update(cx, |view, cx| {
        view.visible = true;
        cx.notify();
    });
    settle(cx);
    assert_eq!(extent(cx, "fixed", Axis::Horizontal), px(320.));
    view.read_with(cx, |view, _| assert_eq!(view.commits, 0));
}

#[gpui::test]
fn controlled_height_keeps_the_remaining_panel_flexible(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, Axis::Vertical);
    drag(cx, Axis::Vertical, px(240.), px(280.));
    let preferred = view.read_with(cx, |view, _| view.preferred);
    view.update(cx, |view, cx| {
        view.extent = px(600.);
        cx.notify();
    });
    settle(cx);
    assert_eq!(extent(cx, "fixed", Axis::Vertical), preferred);
    assert_eq!(
        extent(cx, "remainder", Axis::Vertical),
        px(600.) - preferred
    );
    view.read_with(cx, |view, _| assert_eq!(view.commits, 1));
}
