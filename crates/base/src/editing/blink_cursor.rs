use gpui::{Bounds, Context, Pixels, Point, Task, point, px, size};
use instant::Duration;

static INTERVAL: Duration = Duration::from_millis(500);
static PAUSE_DELAY: Duration = Duration::from_millis(300);

// On Windows, Linux, we should use integer to avoid blurry cursor.
#[cfg(not(target_os = "macos"))]
pub(crate) const CURSOR_WIDTH: Pixels = px(2.);
#[cfg(target_os = "macos")]
pub(crate) const CURSOR_WIDTH: Pixels = px(1.5);

pub(crate) fn caret_bounds(origin: Point<Pixels>, line_height: Pixels) -> Bounds<Pixels> {
    let height = line_height * 0.85;
    Bounds::new(
        origin + point(px(0.), (line_height - height) / 2.),
        size(CURSOR_WIDTH, height),
    )
}

/// Cursor visibility shared by inputs, documents and embedded editors.
///
/// Retain this in an Entity and observe it to repaint. Focus starts a visible
/// phase, input restarts the idle delay, and blur cancels the pending timer.
pub struct BlinkCursor {
    visible: bool,
    epoch: usize,

    _task: Task<()>,
}

impl BlinkCursor {
    pub fn new() -> Self {
        Self {
            visible: false,
            epoch: 0,
            _task: Task::ready(()),
        }
    }

    /// Show the cursor immediately, then blink every 500ms while idle.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.show_for(INTERVAL, cx);
    }

    /// Hide the cursor and cancel the pending blink or input delay.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.next_epoch();
        self.visible = false;
        self._task = Task::ready(());
        cx.notify();
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.epoch {
            return;
        }

        self.visible = cx.reduce_motion() || !self.visible;
        cx.notify();
        self.schedule(epoch, INTERVAL, cx);
    }

    fn schedule(&mut self, epoch: usize, delay: Duration, cx: &mut Context<Self>) {
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink(epoch, cx));
            }
        });
    }

    /// Whether the owner should paint its focused cursor.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Show the cursor immediately and restart the idle delay before blinking resumes.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.show_for(PAUSE_DELAY, cx);
    }

    fn show_for(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.visible = true;
        cx.notify();
        let epoch = self.next_epoch();
        self.schedule(epoch, delay, cx);
    }
}

impl Default for BlinkCursor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn repeated_pauses_keep_cursor_visible_until_idle(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        for _ in 0..5 {
            cursor.update(cx, |cursor, cx| cursor.pause(cx));
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(200));
            cx.run_until_parked();
            assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        }
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        cx.executor().advance_clock(INTERVAL);
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn focus_restarts_visible_and_blur_cancels_input_delay(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        cx.run_until_parked();
        cx.executor().advance_clock(INTERVAL);
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));

        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(INTERVAL);
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));

        cursor.update(cx, |cursor, cx| cursor.start(cx));
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn reduced_motion_keeps_focused_cursor_visible(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        for _ in 0..3 {
            cx.run_until_parked();
            cx.executor().advance_clock(INTERVAL);
            cx.run_until_parked();
            assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        }
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
    }
}
