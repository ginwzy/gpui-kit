---
title: Root View
description: Use the Root view to enable themes, notifications, dialogs, and other GPUI Component features in a window.
example: false
---

# Root View

[Root] is the Base-owned root view of every GPUI Kit window. `gpui_base::open_window` always creates this type; `gpui_kit::open_window` re-exports the same function, and `component::Root` re-exports the Base type.

Base owns the content and overlay host, keyboard traversal and selection copying. Calling `gpui_component::init` explicitly registers the styled window extension: dialogs, sheets, notifications, tooltips, menus, touch selection, theme and window chrome. Initialize it before creating windows. A Base-only application calls `gpui_base::init` and needs no Component or Kit dependency. Cargo feature unification does not change the root type.

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

## Closing windows and quitting

Applications define their own quit and close-window actions and key bindings. `gpui_kit::init` does not install them. Handle unsaved changes and any confirmation in the application before closing a window or quitting.

## Overlays

`Root` always mounts the dialog, sheet and notification layers above application content. Applications only call `window.open_dialog`, `window.open_sheet` or `window.push_notification`; no manual mounting or configuration is needed. Child view caching does not affect overlay rendering.

### Migrating to 0.7.0

`Root::render_dialog_layer`, `Root::render_sheet_layer` and `Root::render_notification_layer` have been removed. Delete their calls and the corresponding `.children(...)` expressions from application views. Previously customized layer positions now use the window-level Root's overlay placement.

[Root]: https://docs.rs/gpui-base/latest/gpui_base/struct.Root.html
