//! Read-only text laid out as display rows of one line height.
//!
//! The document wraps its text from per-character advances, off the main thread
//! for long texts, and lays each display row out as its own unwrapped line. The
//! wrap the rows show is the wrap the geometry counts, so every row is exactly
//! one line tall and the list knows the whole extent without measuring.
use std::{error::Error, fmt, ops::Range, sync::Arc};

use gpui::{
    AbsoluteLength, AppContext as _, Bounds, Context, Font, LineFragment, LineWrapper, Pixels,
    Task, Window, px,
};
use unicode_segmentation::GraphemeCursor;

use super::{DocumentLayoutItem, DocumentState};
use crate::document::{DocumentRevision, EditPolicy};

/// Texts up to this size wrap within the frame that first shows them. Longer
/// ones show unwrapped source lines until a background wrap arrives.
const WRAP_IN_FRAME_BYTES: usize = 256 * 1024;

/// How a read-only document lays its text out as display rows.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRows {
    inset: AbsoluteLength,
}

impl Default for TextRows {
    fn default() -> Self {
        Self {
            inset: px(0.).into(),
        }
    }
}

impl TextRows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Width the row renderer places before the text, such as a gutter. Rows
    /// wrap to the document width less this inset.
    pub fn with_inset(mut self, inset: impl Into<AbsoluteLength>) -> Self {
        self.inset = inset.into();
        self
    }

    pub fn inset(&self) -> AbsoluteLength {
        self.inset
    }
}

/// Why a document cannot lay its text out as display rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRowsError {
    /// Rows are for read-only text; every region must be read-only.
    EditableRegion,
    /// Rows wrap the source text itself; the projection must be the identity.
    Projection,
    /// Blocks and anchored blocks are not text rows.
    Blocks,
    /// Paragraph styles and font overrides change advances per run.
    Styles,
}

impl fmt::Display for TextRowsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EditableRegion => "text rows need every region to be read-only",
            Self::Projection => "text rows need the identity projection",
            Self::Blocks => "text rows cannot contain blocks",
            Self::Styles => "text rows cannot use paragraph styles or font overrides",
        })
    }
}

impl Error for TextRowsError {}

pub(super) struct TextRowsState {
    rows: TextRows,
    /// Display offsets where a row starts inside a line, ascending.
    pub(super) breaks: Arc<Vec<usize>>,
    /// What `breaks` were wrapped for.
    wrapped: Option<RowWrap>,
    /// A wrap computing in the background, with what it wraps for.
    pending: Option<(RowWrap, Task<()>)>,
    line_height: Option<Pixels>,
}

/// Everything a wrap depends on.
#[derive(Clone, Debug, PartialEq)]
struct RowWrap {
    revision: DocumentRevision,
    width: Pixels,
    font: Font,
    font_size: Pixels,
}

impl TextRowsState {
    /// Moves row starts through host edits, given in ascending order as the
    /// replaced range and replacement length. Starts inside a replaced range
    /// are dropped; its lines show unwrapped until the next wrap.
    pub(super) fn apply_edits(&mut self, edits: impl IntoIterator<Item = (Range<usize>, usize)>) {
        let mut breaks = Vec::with_capacity(self.breaks.len());
        let mut old = self.breaks.iter().copied().peekable();
        let mut delta = 0isize;
        for (range, len) in edits {
            while let Some(start) = old.next_if(|start| *start < range.start) {
                breaks.push(start.saturating_add_signed(delta));
            }
            while old.next_if(|start| *start <= range.end).is_some() {}
            delta += len as isize - range.len() as isize;
        }
        breaks.extend(old.map(|start| start.saturating_add_signed(delta)));
        self.breaks = Arc::new(breaks);
        self.wrapped = None;
    }

    /// Forgets every row start, as after the text is replaced wholesale.
    pub(super) fn clear(&mut self) {
        self.breaks = Arc::default();
        self.wrapped = None;
        self.pending = None;
    }
}

impl<I: Clone + Eq + 'static> DocumentState<I> {
    /// Lay read-only text out as display rows of one line height, or `None` to
    /// return to one item per source line.
    ///
    /// The document wraps the text to its width less the inset from character
    /// advances, within the frame for short texts and in the background for
    /// long ones, and draws each display row unwrapped. Rows are therefore
    /// exactly one line tall, the scroll extent is exact without measuring, and
    /// only visible rows are shaped. Row breaks fall between grapheme clusters.
    /// Positions, line movement, selection and copy are unchanged; each row
    /// reaches the text renderer with its own source range, and the renderer
    /// must keep rows one line of text tall.
    pub fn set_text_rows(
        &mut self,
        rows: Option<TextRows>,
        cx: &mut Context<Self>,
    ) -> Result<(), TextRowsError> {
        match (rows, &mut self.text_rows) {
            (Some(rows), Some(state)) => {
                if state.rows != rows {
                    state.rows = rows;
                    state.wrapped = None;
                }
            }
            (Some(rows), None) => {
                self.text_rows_supported()?;
                self.text_rows = Some(TextRowsState {
                    rows,
                    breaks: Arc::default(),
                    wrapped: None,
                    pending: None,
                    line_height: None,
                });
            }
            (None, Some(_)) => {
                let breaks = self.text_rows.take().map(|state| state.breaks);
                self.list_state.set_uniform_item_height(None);
                if breaks.is_some_and(|breaks| !breaks.is_empty()) {
                    self.reconcile_layout_items_from(self.layout_items.len());
                }
            }
            (None, None) => return Ok(()),
        }
        cx.notify();
        Ok(())
    }

    pub(super) fn text_rows_supported(&self) -> Result<(), TextRowsError> {
        let model = &self.model;
        if model
            .regions
            .as_slice()
            .iter()
            .any(|region| region.policy() != EditPolicy::Readonly)
        {
            return Err(TextRowsError::EditableRegion);
        }
        if !model.projection.is_identity() {
            return Err(TextRowsError::Projection);
        }
        if !model.blocks.is_empty() || !model.anchored_blocks.is_empty() {
            return Err(TextRowsError::Blocks);
        }
        if !model.resolved_styles.paragraphs.is_empty()
            || model
                .resolved_styles
                .inline
                .iter()
                .any(|style| style.font_family.is_some())
        {
            return Err(TextRowsError::Styles);
        }
        Ok(())
    }

    /// Keeps rows wrapped for the current width, text and font, before the list
    /// lays out. Short texts wrap now; long ones wrap in the background.
    pub(super) fn prepare_text_rows(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.text_rows.is_none() {
            return;
        }
        // Only hosts can change the structure, and rows require it to stay text.
        debug_assert_eq!(self.text_rows_supported(), Ok(()));
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        let style = window.text_style();
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        // The line height a text element lays out with, snapped to device pixels.
        let line_height =
            window.pixel_snap(style.line_height.to_pixels(font_size.into(), rem_size));
        if state.line_height != Some(line_height) {
            state.line_height = Some(line_height);
            self.list_state.set_uniform_item_height(Some(line_height));
        }
        let wrap = RowWrap {
            revision: self.model.revision,
            width: (bounds.size.width - state.rows.inset.to_pixels(rem_size)).max(px(0.)),
            font: style.font(),
            font_size,
        };
        if state.wrapped.as_ref() == Some(&wrap)
            || state
                .pending
                .as_ref()
                .is_some_and(|(pending, _)| *pending == wrap)
        {
            return;
        }
        let mut wrapper = cx
            .text_system()
            .line_wrapper(wrap.font.clone(), wrap.font_size);
        let text = self.model.display_text();
        if text.len() <= WRAP_IN_FRAME_BYTES {
            let breaks = wrap_rows(text, &mut wrapper, wrap.width);
            state.pending = None;
            self.apply_text_rows(wrap, breaks, cx);
            return;
        }
        let text = text.to_owned();
        let pending = wrap.clone();
        let task = cx.spawn(async move |this, cx| {
            let width = pending.width;
            let breaks = cx
                .background_spawn(async move { wrap_rows(&text, &mut wrapper, width) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.text_rows.as_mut() else {
                    return;
                };
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|(wrap, _)| *wrap == pending)
                {
                    // This task is finishing; let it complete rather than cancel it.
                    if let Some((_, task)) = state.pending.take() {
                        task.detach();
                    }
                    this.apply_text_rows(pending, breaks, cx);
                }
            });
        });
        state.pending = Some((wrap, task));
    }

    /// Installs a finished wrap, laying out again only the lines whose rows changed.
    fn apply_text_rows(&mut self, wrap: RowWrap, breaks: Vec<usize>, cx: &mut Context<Self>) {
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        state.wrapped = Some(wrap);
        if *state.breaks == breaks {
            return;
        }
        let old = std::mem::replace(&mut state.breaks, Arc::new(breaks));
        let breaks = state.breaks.clone();
        let lines = changed_lines(self.model.display_text(), &old, &breaks);
        if lines.is_empty() {
            return;
        }
        self.capture_viewport_anchor();
        let text = self.model.display_text();
        let text_len = self.model.text.len();
        let display_start = |item: &DocumentLayoutItem<I>| match item {
            DocumentLayoutItem::Text { display, .. } => display.start,
            _ => usize::MAX,
        };
        let old_len = self.layout_items.len();
        let mut items = Vec::with_capacity(old_len);
        let mut old_items = std::mem::take(&mut self.layout_items).into_iter();
        let mut moved = 0;
        // Rows have a known height, so one splice over the changed span replaces
        // many small ones; the viewport anchor keeps the visible text in place.
        let mut span: Option<(usize, usize)> = None;
        for line in &lines {
            // A line's rows start within it; the next line starts after its LF.
            let first = moved
                + old_items
                    .as_slice()
                    .partition_point(|item| display_start(item) < line.start);
            let last = moved
                + old_items
                    .as_slice()
                    .partition_point(|item| display_start(item) <= line.end);
            items.extend(old_items.by_ref().take(first - moved));
            old_items.by_ref().take(last - first).for_each(drop);
            moved = last;
            let rows: Vec<_> = rows_of_line(line.clone(), &breaks)
                .map(|row| self.model.text_layout_item(text, row, 0..text_len))
                .collect();
            let (start, end) = span.get_or_insert((first, last));
            *end = last;
            debug_assert!(*start <= first);
            items.extend(rows);
        }
        items.extend(old_items);
        let added = items.len() as isize - old_len as isize;
        self.layout_items = items;
        if let Some((start, end)) = span {
            let count = (end as isize - start as isize + added) as usize;
            self.list_state.splice(start..end, count);
        }
        cx.notify();
    }
}

/// The display rows of `line`, split at the row starts inside it.
pub(super) fn rows_of_line(
    line: Range<usize>,
    breaks: &[usize],
) -> impl Iterator<Item = Range<usize>> + '_ {
    let from = breaks.partition_point(|start| *start <= line.start);
    let to = breaks.partition_point(|start| *start < line.end).max(from);
    let starts = std::iter::once(line.start).chain(breaks[from..to].iter().copied());
    let ends = breaks[from..to]
        .iter()
        .copied()
        .chain(std::iter::once(line.end));
    starts.zip(ends).map(|(start, end)| start..end)
}

/// Lines, as display ranges without their LF, that contain a row start present
/// in only one of `old` and `new`. Both are ascending, so each line is found once.
fn changed_lines(text: &str, old: &[usize], new: &[usize]) -> Vec<Range<usize>> {
    let mut lines: Vec<Range<usize>> = Vec::new();
    let (mut old, mut new) = (old.iter().peekable(), new.iter().peekable());
    loop {
        let changed = match (old.peek(), new.peek()) {
            (Some(a), Some(b)) if a == b => {
                old.next();
                new.next();
                continue;
            }
            (Some(a), Some(b)) if a < b => *old.next().unwrap(),
            (Some(_), Some(_)) => *new.next().unwrap(),
            (Some(_), None) => *old.next().unwrap(),
            (None, Some(_)) => *new.next().unwrap(),
            (None, None) => break,
        };
        if lines.last().is_some_and(|line| line.contains(&changed)) {
            continue;
        }
        let start = text[..changed].rfind('\n').map_or(0, |ix| ix + 1);
        let end = text[changed..]
            .find('\n')
            .map_or(text.len(), |ix| changed + ix);
        lines.push(start..end);
    }
    lines
}

/// Row starts inside each line of `text` when wrapped to `width`, ascending.
/// A start that would split a grapheme cluster moves back to the cluster's
/// start; a cluster wider than a row keeps a row to itself.
pub(super) fn wrap_rows(text: &str, wrapper: &mut LineWrapper, width: Pixels) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let mut row_start = 0;
        for boundary in wrapper.wrap_line_unindented(&[LineFragment::text(line)], width) {
            let mut start = boundary.ix;
            let mut cursor = GraphemeCursor::new(start, line.len(), true);
            if !cursor.is_boundary(line, 0).unwrap_or(true) {
                start = cursor
                    .prev_boundary(line, 0)
                    .ok()
                    .flatten()
                    .unwrap_or(row_start);
            }
            if row_start < start && start < line.len() {
                breaks.push(line_start + start);
                row_start = start;
            }
        }
        line_start += line.len() + 1;
    }
    breaks
}

#[cfg(test)]
mod tests {
    use gpui::{
        Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
        VisualTestContext, div, size,
    };
    use unicode_segmentation::UnicodeSegmentation as _;

    use super::*;
    use crate::{
        Theme,
        document::{DocumentRegion, EditOrigin, EditTransaction, TextEdit},
    };

    #[test]
    fn rows_split_lines_and_follow_host_edits() {
        assert_eq!(
            rows_of_line(4..20, &[2, 9, 15, 25]).collect::<Vec<_>>(),
            [4..9, 9..15, 15..20]
        );
        assert_eq!(rows_of_line(4..4, &[4]).collect::<Vec<_>>(), [4..4]);
        let mut state = TextRowsState {
            rows: TextRows::new(),
            breaks: Arc::new(vec![5, 10, 20, 30]),
            wrapped: None,
            pending: None,
            line_height: None,
        };
        // Starts before an edit stay, starts inside it drop, later ones shift.
        state.apply_edits([(8..12, 1), (25..25, 4)]);
        assert_eq!(*state.breaks, [5, 17, 31]);
        let text = "aaaa\nbbbbbbbb\ncccc";
        assert_eq!(changed_lines(text, &[2, 7, 9], &[2, 8]), [5..13]);
        assert_eq!(changed_lines(text, &[], &[16]), [14..18]);
    }

    #[gpui::test]
    fn wrapped_rows_start_between_grapheme_clusters(cx: &mut TestAppContext) {
        let text = format!(
            "{}\n\n{}",
            "e\u{301}👨\u{200d}👩\u{200d}👧xy中".repeat(40),
            "plain words wrap at spaces ".repeat(20)
        );
        let font = gpui::font("Zed Mono");
        let mut wrapper = cx.text_system().line_wrapper(font, px(16.));
        let breaks = wrap_rows(&text, &mut wrapper, px(97.));
        assert!(breaks.len() > 10);
        let clusters: Vec<_> = text.grapheme_indices(true).map(|(ix, _)| ix).collect();
        let mut previous = 0;
        for start in &breaks {
            assert!(
                clusters.contains(start),
                "row starts inside a cluster at {start}"
            );
            assert!(*start > previous && text.as_bytes()[start - 1] != b'\n');
            previous = *start;
        }
    }

    struct Root(Entity<DocumentState<&'static str>>);

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn rows_view(
        text: &str,
        cx: &mut TestAppContext,
    ) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
        let mut document = None;
        let text = text.to_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                let len = text.len();
                document = Some(cx.new(|cx| {
                    DocumentState::new(
                        text,
                        vec![DocumentRegion::new("source", 0..len, EditPolicy::Readonly)],
                        cx,
                    )
                    .unwrap()
                }));
                cx.new(|_| Root(document.clone().unwrap()))
            })
            .unwrap()
        });
        let cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(size(px(320.), px(200.)));
        (document.unwrap(), cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    /// The rows the document should show, wrapped directly.
    fn expected(
        document: &Entity<DocumentState<&'static str>>,
        width: Pixels,
        cx: &mut VisualTestContext,
    ) -> Vec<usize> {
        cx.update(|window, cx| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let mut wrapper = cx.text_system().line_wrapper(style.font(), font_size);
            wrap_rows(document.read(cx).model.display_text(), &mut wrapper, width)
        })
    }

    fn breaks(
        document: &Entity<DocumentState<&'static str>>,
        cx: &mut VisualTestContext,
    ) -> Vec<usize> {
        document.read_with(cx, |document, _| {
            document.text_rows.as_ref().unwrap().breaks.to_vec()
        })
    }

    #[gpui::test]
    fn rows_wrap_to_the_width_without_changing_text_or_measuring(cx: &mut TestAppContext) {
        let line = "word ".repeat(120);
        let text = format!("short\n{line}\nlast\n");
        let (document, mut cx) = rows_view(&text, cx);
        document.update(&mut cx, |document, cx| {
            document
                .set_text_rows(Some(TextRows::new().with_inset(px(20.))), cx)
                .unwrap();
        });
        draw(&mut cx);
        let wrapped = breaks(&document, &mut cx);
        assert!(!wrapped.is_empty());
        assert_eq!(wrapped, expected(&document, px(300.), &mut cx));
        document.read_with(&cx, |document, _| {
            assert_eq!(document.text(), text);
            assert_eq!(document.layout_items, document.model.layout_items(&wrapped));
            // Rows of the long line are its text, each without a line break.
            let rows: String = document
                .layout_items
                .iter()
                .filter_map(|item| match item {
                    DocumentLayoutItem::Text { display, .. }
                        if display.start >= 6 && display.end <= 6 + line.len() =>
                    {
                        Some(&document.display_text()[display.clone()])
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(rows, line);
            // One row per item, all one line tall: the extent is exact unmeasured.
            let line_height = document.text_rows.as_ref().unwrap().line_height.unwrap();
            let total = line_height * document.layout_items.len() as f32;
            let list = &document.list_state;
            assert_eq!(
                list.max_offset_for_scrollbar().y,
                (total - list.viewport_bounds().size.height).max(px(0.))
            );
            for ix in 0..document.layout_items.len() {
                if let Some(bounds) = list.bounds_for_item(ix) {
                    assert_eq!(bounds.size.height, line_height);
                }
            }
        });

        // A host edit keeps the rows of untouched lines and wraps the text again.
        document.update(&mut cx, |document, cx| {
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..5, "a longer first line")],
                text.len(),
            )
            .unwrap();
            let len = text.len() - 5 + "a longer first line".len();
            document
                .apply_host_transaction(
                    transaction,
                    vec![DocumentRegion::new("source", 0..len, EditPolicy::Readonly)],
                    cx,
                )
                .unwrap();
        });
        draw(&mut cx);
        let rewrapped = breaks(&document, &mut cx);
        assert_eq!(rewrapped, expected(&document, px(300.), &mut cx));
        document.read_with(&cx, |document, _| {
            assert_eq!(
                document.layout_items,
                document.model.layout_items(&rewrapped)
            );
        });

        // A wider document wraps into fewer rows.
        cx.simulate_resize(size(px(520.), px(200.)));
        draw(&mut cx);
        let wider = breaks(&document, &mut cx);
        assert_eq!(wider, expected(&document, px(500.), &mut cx));
        assert!(wider.len() < rewrapped.len());

        // Leaving row mode restores one item per line.
        document.update(&mut cx, |document, cx| {
            document.set_text_rows(None, cx).unwrap();
            assert_eq!(document.layout_items, document.model.layout_items(&[]));
        });
    }

    #[gpui::test]
    fn long_texts_wrap_in_the_background(cx: &mut TestAppContext) {
        let text = format!("{}\n", "word ".repeat(WRAP_IN_FRAME_BYTES / 5 + 10));
        let (document, mut cx) = rows_view(&text, cx);
        document.update(&mut cx, |document, cx| {
            document.set_text_rows(Some(TextRows::new()), cx).unwrap();
        });
        // The first frame shows the source line unwrapped.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(breaks(&document, &mut cx).is_empty());
        draw(&mut cx);
        draw(&mut cx);
        let wrapped = breaks(&document, &mut cx);
        assert!(!wrapped.is_empty());
        assert_eq!(wrapped, expected(&document, px(320.), &mut cx));
    }

    #[gpui::test]
    fn rows_require_read_only_text(cx: &mut TestAppContext) {
        let document = cx.new(|cx| {
            DocumentState::new(
                "draft",
                vec![DocumentRegion::new("draft", 0..5, EditPolicy::Editable)],
                cx,
            )
            .unwrap()
        });
        document.update(cx, |document, cx| {
            assert_eq!(
                document.set_text_rows(Some(TextRows::new()), cx),
                Err(TextRowsError::EditableRegion)
            );
            assert!(document.text_rows.is_none());
        });
    }
}
