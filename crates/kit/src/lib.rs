//! GPUI Kit: one dependency for building desktop applications with GPUI.
//!
//! GPUI itself is published as a family of `gpui-pre-*` crates that move
//! together. This crate depends on the matching set for you, so an
//! application lists `gpui-kit` alone. `use gpui_kit::*;` is GPUI, and each
//! layer is reachable by name:
//!
//! | Path            | Crate             | Feature          |
//! | --------------- | ----------------- | ---------------- |
//! | `gpui_kit::*`   | `gpui`            | always           |
//! | `platform`      | `gpui_platform`   | desktop / web    |
//! | [`base`]        | `gpui-base`       | always           |
//! | [`component`]   | `gpui-component`  | `component` (on) |
//! | [`assets`]      | `gpui-kit-assets` | `assets` (on)    |
//!
//! On desktop and web, `application` opens the platform. Mobile applications
//! supply their backend to `Application::with_platform`. [`init`] initializes the enabled
//! layers:
//!
//! ```no_run
//! use gpui_kit::*;
//!
//! actions!(hello, [Quit]);
//!
//! struct Hello;
//!
//! impl Render for Hello {
//!     fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
//!         div().child("Hello, World!")
//!     }
//! }
//!
//! fn main() {
//!     gpui_kit::application().run(|cx| {
//!         gpui_kit::init(cx);
//!         gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Hello))
//!             .expect("failed to open window");
//!     });
//! }
//! ```
//!
//! See [`component`] for the same program with the styled component library.

/// Defines unit actions without requiring consumers to depend on GPUI under the
/// crate name `gpui`.
///
/// GPUI's original macro spells its derive as `gpui::Action`, which does not
/// resolve when GPUI is consumed solely through this facade.
#[macro_export]
macro_rules! actions {
    ($namespace:path, [ $( $(#[$attr:meta])* $name:ident),* $(,)? ]) => {
        $(
            #[derive(
                ::std::clone::Clone,
                ::std::cmp::PartialEq,
                ::std::default::Default,
                ::std::fmt::Debug,
                $crate::Action
            )]
            #[action(namespace = $namespace)]
            $(#[$attr])*
            pub struct $name;
        )*
    };
    ([ $( $(#[$attr:meta])* $name:ident),* $(,)? ]) => {
        $(
            #[derive(
                ::std::clone::Clone,
                ::std::cmp::PartialEq,
                ::std::default::Default,
                ::std::fmt::Debug,
                $crate::Action
            )]
            $(#[$attr])*
            pub struct $name;
        )*
    };
}

// Public facade decision — 2026-09-08:
// GPUI Kit is the application-facing entry point. Users should depend on and
// import gpui-kit without needing to know which GPUI crates implement it.
// Keep GPUI APIs available through the Kit root and preserve the published
// #[gpui_kit::test] macro. Do not replace it with Rust's built-in #[test].
// A future switch to official GPUI crates is an internal dependency migration,
// not a reason to steer Kit users toward gpui:: paths or require import changes.
// Keep the existing gpui namespace re-export hidden for source compatibility;
// it is not the recommended application API.
//
// With test-support, the glob below includes GPUI's test macro. Test modules
// should import their Kit types explicitly to avoid shadowing Rust's #[test].
pub use ::gpui::*;

#[doc(hidden)]
pub use ::gpui;

/// UI integration testing: render real components in headless windows, dispatch
/// pointer and keyboard events, and assert state, focus, layout and callbacks.
/// Run tests with `#[gpui_kit::test]`; use this module to interact with their UI.
#[cfg(feature = "test-support")]
pub mod test;

pub use ::gpui_base as base;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use ::gpui_platform as platform;
#[cfg(target_family = "wasm")]
pub use ::gpui_web as web;
pub use gpui_base::is_mobile;

/// The styled component library.
///
/// ```no_run
/// use gpui_kit::component::button::*;
/// use gpui_kit::component::Root;
/// use gpui_kit::*;
///
/// struct Hello;
///
/// impl Render for Hello {
///     fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
///         div().child(Button::new("ok").primary().label("Let's Go!"))
///     }
/// }
///
/// fn main() {
///     gpui_kit::application().run(|cx| {
///         gpui_kit::init(cx);
///         gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Hello))
///             .expect("failed to open window");
///     });
/// }
/// ```
#[cfg(feature = "component")]
pub use ::gpui_component as component;

/// Opens a window ready for GPUI Kit, with the view `build` returns as its
/// content.
///
/// This is `cx.open_window` with the one thing every GPUI Kit window needs
/// already done. With the `component` feature the window's root view is a
/// [`component::Root`] wrapping the view, so dialogs, sheets, notifications,
/// tooltips and menus work in it straight away; without it the view is the
/// root. Either way `init` has already bound the platform's quit shortcut
/// (`cmd-q` on macOS, `alt-f4` elsewhere) and, on macOS, `cmd-w` to close
/// the window.
///
/// ```ignore
/// gpui_kit::application().run(|cx| {
///     gpui_kit::init(cx);
///     gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| MyApp))
///         .expect("failed to open window");
/// });
/// ```
///
/// It returns the window and the view, so a caller that keeps a handle to
/// its view can build the view inside the window — where `InputState` and
/// other window-bound state must be created — and still keep it:
///
/// ```ignore
/// let (window, editor) = gpui_kit::open_window(options, cx, |window, cx| {
///     cx.new(|cx| Editor::new(window, cx))
/// })?;
/// ```
///
/// From an async context, call it inside `cx.update`. Wrap the view yourself
/// with `Root::new` when the window needs a customized `Root` —
/// `bordered(false)` for a layer-shell surface, say.
pub fn open_window<V: Render>(
    options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<(AnyWindowHandle, Entity<V>)> {
    let mut built = None;
    #[cfg(feature = "component")]
    let window = cx
        .open_window(options, |window, cx| {
            let view = build(window, cx);
            built = Some(view.clone());
            cx.new(|cx| component::Root::new(view, window, cx))
        })?
        .into();
    #[cfg(not(feature = "component"))]
    let window = cx
        .open_window(options, |window, cx| {
            let view = build(window, cx);
            built = Some(view.clone());
            view
        })?
        .into();
    let view = built.expect("open_window ran its build closure");
    Ok((window, view))
}
#[cfg(feature = "assets")]
pub use ::gpui_kit_assets as assets;

// Mobile applications provide their platform with `Application::with_platform`.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use ::gpui_platform::application;

/// Initializes every enabled layer. Call it once, before using anything else.
///
/// With the `component` feature (on by default) this is
/// `gpui_component::init`, which also initializes `gpui-base`; otherwise it
/// is `gpui_base::init`.
pub fn init(cx: &mut App) {
    #[cfg(feature = "component")]
    gpui_component::init(cx);
    #[cfg(not(feature = "component"))]
    gpui_base::init(cx);
}

/// Fluent UI test observation, inert unless `test-support` is enabled.
pub use gpui_base::TestSupportExt;
