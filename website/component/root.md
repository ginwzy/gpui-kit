---
title: Root View
description: Use the Root view to enable themes, notifications, dialogs, and other GPUI Component features in a window.
example: false
---

# Root View

[Root] is the root view of a GPUI Kit window. It holds the window's dialogs, sheets and notifications and renders them above the view it wraps, and it hosts tooltips and menus. `gpui_kit::open_window` creates it for you; you only meet `Root` directly when a window needs a customized one.

This complete **Tested consumer recipe** is compiled from the isolated `gpui-kit` consumer workspace. It initializes GPUI Kit, then opens a window whose root is a `Root` wrapping the application view.

<!-- recipe:bootstrap:start -->
```rust
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowOptions, div,
};

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            // The window's root view is a `Root` wrapping the view, which
            // renders dialogs, sheets and notifications above it.
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| BootstrapView)
            })
            .expect("failed to open window");
        });
}

struct BootstrapView;

impl Render for BootstrapView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("My application")
    }
}
```
<!-- recipe:bootstrap:end -->

## Customizing the Root

`gpui_kit::open_window` is `cx.open_window` plus the `Root` wrapper. Build the
`Root` yourself when it needs configuring — for example `bordered(false)` for a
layer-shell fullscreen window that should not render GPUI Component's
client-side window border:

```rs
cx.open_window(WindowOptions::default(), |window, cx| {
    let view = cx.new(|_| MyApp);
    cx.new(|cx| Root::new(view, window, cx).bordered(false))
})
```

Whichever way it is built, `Root` must be the window's root view: `window.open_dialog`, `open_sheet` and `push_notification` store their state on it and panic with a pointer to this page when it is missing.

`open_window` returns the window and the view, so a view that must be built inside the window (it owns an `InputState`, say) can still be kept:

```rust
let (window, editor) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
    cx.new(|cx| Editor::new(window, cx))
})?;
```

## Default Keys

`gpui_kit::init` binds the platform's quit shortcut — `cmd-q` on macOS, `alt-f4` on Windows and Linux — to `gpui_kit::base::actions::Quit`, which quits the application, so every window answers it. On macOS `cmd-w` is bound to `gpui_kit::base::actions::CloseWindow`, which closes the active window the way `File › Close` does (Windows and Linux close a window with `alt-f4`). Both live in `gpui-base`: they are window behavior, not styling. To ask before quitting or closing, bind the same shortcut to your own action; a binding added later wins:

```rust
cx.bind_keys([KeyBinding::new("cmd-q", ConfirmQuit, None)]);
```

## Overlays

Dialogs, sheets and notifications render on layers that `Root` places above the view, so a view that never mentions them still shows them. An application that wants a layer somewhere else in its tree — under its own title bar, say, or below a HUD that must stay on top — renders that layer itself and `Root` leaves it out:

- [Root::render_dialog_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_dialog_layer) - the open dialogs.
- [Root::render_sheet_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_sheet_layer) - the open sheet.
- [Root::render_notification_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_notification_layer) - the notification list.

```rust
impl Render for MyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.content.clone())
            // Placed here on purpose; Root will not add a second one.
            .children(Root::render_dialog_layer(window, cx))
            .child(self.hud.clone())
    }
}
```

Each returns `None` while it has nothing to show, which is why the example uses `children`.

[Root]: https://docs.rs/gpui-component/latest/gpui_component/root/struct.Root.html
