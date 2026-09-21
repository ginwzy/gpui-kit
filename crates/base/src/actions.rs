//! Common actions shared by keyboard-driven GPUI controls, and the
//! application-wide commands every desktop window answers.

use gpui::{Action, App, KeyBinding, actions};
use serde::Deserialize;

#[derive(Clone, Action, PartialEq, Eq, Deserialize)]
#[action(namespace = ui, no_json)]
pub struct Confirm {
    /// Whether the confirmation uses the secondary action.
    pub secondary: bool,
}

actions!(
    ui,
    [
        Cancel,
        SelectUp,
        SelectDown,
        SelectLeft,
        SelectRight,
        SelectFirst,
        SelectLast,
        SelectPrevColumn,
        SelectNextColumn,
        SelectPageUp,
        SelectPageDown,
        /// Quits the application. [`init`](crate::init) binds it to the
        /// platform's quit shortcut: `cmd-q` on macOS, `alt-f4` on Windows and
        /// Linux. Bind that shortcut to your own action to run a confirmation
        /// first; a binding added later wins.
        Quit,
        /// Closes the active window, the way `File › Close` does on macOS.
        /// [`init`](crate::init) binds it to `cmd-w` on macOS; Windows and
        /// Linux close a window with `alt-f4`, which quits here.
        CloseWindow
    ]
);

/// Binds the platform's quit and close-window shortcuts and handles them.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-w", CloseWindow, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-f4", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        // The key arrives inside that window's own update, so removing it
        // has to wait until the update returns.
        if let Some(window) = cx.active_window() {
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_keeps_its_secondary_payload() {
        assert!(Confirm { secondary: true }.secondary);
        assert!(!Confirm { secondary: false }.secondary);
    }
}
