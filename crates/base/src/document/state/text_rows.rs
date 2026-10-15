//! Text laid out as display rows whose geometry is data.
//!
//! With text rows, the document wraps each display line from the advances of
//! its grapheme clusters in the fonts of its paragraph and inline styles, and
//! lays each display row out as its own unwrapped line. The wrap the rows show
//! is the wrap the geometry counts. A row is its line height, plus the space
//! the host's frame keeps before the line's first row and after its last, so
//! the list knows the height of all text without measuring it. Blocks declare
//! their height, or are measured.
//!
//! Wraps are cached by the text and fonts of a line, so edits of any kind keep
//! the rows of every line they leave unchanged. Lines in view wrap within the
//! frame; a large rewrap of the rest runs in the background, and those lines
//! keep their previous rows, and the heights of those rows, until it lands.
use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use gpui::{
    AbsoluteLength, App, AppContext as _, Bounds, Context, Font, LineWrapper, LineWrapperHandle,
    Pixels, Refineable as _, Task, TextStyleRefinement, Window, px,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{Derived, DocumentLayoutItem, DocumentState, DocumentTextContext};
use crate::document::DocumentRegion;

/// Lines never wrapped before all wrap within the frame when they total up to
/// this many bytes, so a text of this size shows its final rows and extent in
/// its first frame. Beyond it, only lines in view do.
const FIRST_WRAP_IN_FRAME_BYTES: usize = 256 * 1024;
/// Lines wrapped for another width or font rewrap within the frame up to this
/// many bytes beyond those in view; a larger rewrap runs in the background.
const REWRAP_IN_FRAME_BYTES: usize = 32 * 1024;
/// How far past the viewport, in viewport heights, rows count as in view.
const VIEW_MARGIN: f32 = 1.;

/// How a document lays its text out as display rows.
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

    /// Width the row renderer places before the text, such as a gutter, for
    /// lines the host does not frame. Rows wrap to the document width less this.
    pub fn with_inset(mut self, inset: impl Into<AbsoluteLength>) -> Self {
        self.inset = inset.into();
        self
    }

    pub fn inset(&self) -> AbsoluteLength {
        self.inset
    }
}

/// How the host frames one display line of text rows: the space it keeps
/// before the line's first row and after its last, the width it keeps on each
/// side of the text, and the widest the text may be. The text renderer draws
/// each row within its frame and must not change its height.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFrame {
    space_before: AbsoluteLength,
    space_after: AbsoluteLength,
    inset_start: AbsoluteLength,
    inset_end: AbsoluteLength,
    max_width: Option<AbsoluteLength>,
    hidden: bool,
}

impl Default for TextFrame {
    fn default() -> Self {
        Self {
            space_before: px(0.).into(),
            space_after: px(0.).into(),
            inset_start: px(0.).into(),
            inset_end: px(0.).into(),
            max_width: None,
            hidden: false,
        }
    }
}

impl TextFrame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Space above the line's first row.
    pub fn with_space_before(mut self, space: impl Into<AbsoluteLength>) -> Self {
        self.space_before = space.into();
        self
    }

    /// Space below the line's last row.
    pub fn with_space_after(mut self, space: impl Into<AbsoluteLength>) -> Self {
        self.space_after = space.into();
        self
    }

    /// Width the renderer keeps before the text.
    pub fn with_inset_start(mut self, inset: impl Into<AbsoluteLength>) -> Self {
        self.inset_start = inset.into();
        self
    }

    /// Width the renderer keeps after the text.
    pub fn with_inset_end(mut self, inset: impl Into<AbsoluteLength>) -> Self {
        self.inset_end = inset.into();
        self
    }

    /// The widest the text may be, when narrower than the document allows.
    pub fn with_max_width(mut self, width: impl Into<AbsoluteLength>) -> Self {
        self.max_width = Some(width.into());
        self
    }

    /// A hidden line takes no height.
    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    pub fn space_before(&self) -> AbsoluteLength {
        self.space_before
    }

    pub fn space_after(&self) -> AbsoluteLength {
        self.space_after
    }

    pub fn inset_start(&self) -> AbsoluteLength {
        self.inset_start
    }

    pub fn inset_end(&self) -> AbsoluteLength {
        self.inset_end
    }

    pub fn max_width(&self) -> Option<AbsoluteLength> {
        self.max_width
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }
}

/// A line's frame in pixels, with the line height and indent of its text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::document) struct RowFrame {
    space_before: Pixels,
    space_after: Pixels,
    inset_start: Pixels,
    inset_end: Pixels,
    max_width: Option<Pixels>,
    hidden: bool,
    line_height: Pixels,
    /// The width of the paragraph's hanging marker, which wrapped rows align past.
    indent: Pixels,
    /// The line starts at the marker, so only its later rows are indented.
    hanging: bool,
}

impl RowFrame {
    /// How far below the top of `row` its text starts.
    pub(in crate::document) fn text_top(&self, row: &Range<usize>, line: &Range<usize>) -> Pixels {
        if self.hidden || row.start != line.start {
            px(0.)
        } else {
            self.space_before
        }
    }

    pub(in crate::document) fn row_height(
        &self,
        row: &Range<usize>,
        line: &Range<usize>,
    ) -> Pixels {
        if self.hidden {
            return px(0.);
        }
        let mut height = self.line_height;
        if row.start == line.start {
            height += self.space_before;
        }
        if row.end == line.end {
            height += self.space_after;
        }
        height
    }

    /// The widths the line's first and later rows wrap to in a document `width` wide.
    fn widths(&self, width: Pixels) -> (Pixels, Pixels) {
        let available = (width - self.inset_start - self.inset_end).max(px(0.));
        let text = self.max_width.map_or(available, |max| max.min(available));
        let indented = (text - self.indent).max(px(0.));
        (if self.hanging { text } else { indented }, indented)
    }
}

/// A block's height in text rows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::document) enum BlockGeometry {
    /// Layout has not asked the host yet.
    #[default]
    Unresolved,
    /// The list measures the block.
    Measured,
    /// The host declared the block's height.
    Declared(Pixels),
}

impl BlockGeometry {
    pub(in crate::document) fn height(self) -> Option<Pixels> {
        match self {
            Self::Declared(height) => Some(height),
            Self::Unresolved | Self::Measured => None,
        }
    }
}

/// What a line was wrapped for: the advances of a base style generation, and
/// the widths of its first and later rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::document) struct WrapKey {
    generation: u64,
    widths: (Pixels, Pixels),
}

/// The rows of lines that wrap onto more than one row, by line text and fonts,
/// so a line rebuilt by an edit takes its rows again. One-row lines need no
/// entry; their items record what they were wrapped for.
#[derive(Default)]
pub(in crate::document) struct RowCache {
    lines: HashMap<u64, CachedRows, BuildHasherDefault<KeyHasher>>,
}

/// Line keys are hashes already.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*byte);
        }
    }

    fn write_u64(&mut self, key: u64) {
        self.0 = key;
    }
}

struct CachedRows {
    generation: u64,
    widths: (Pixels, Pixels),
    /// Where rows after the first start, relative to the line.
    starts: Arc<[u32]>,
}

impl RowCache {
    /// Where rows after the first start in the line keyed `key`, relative to
    /// the line, as last wrapped. Empty for a line never wrapped.
    pub(in crate::document) fn row_starts(&self, key: u64) -> &[u32] {
        self.lines.get(&key).map_or(&[], |rows| &rows.starts)
    }

    /// Records a wrap. A one-row line only updates an entry it had.
    fn record(&mut self, key: u64, wrap: WrapKey, starts: &[u32]) {
        if starts.is_empty() && !self.lines.contains_key(&key) {
            return;
        }
        self.lines.insert(
            key,
            CachedRows {
                generation: wrap.generation,
                widths: wrap.widths,
                starts: starts.into(),
            },
        );
    }
}

/// Identifies a line's text and every style that changes its advances, from
/// the item for the whole line.
pub(in crate::document) fn line_key<I>(item: &DocumentLayoutItem<I>) -> u64 {
    let DocumentLayoutItem::Text {
        fingerprint,
        presentation,
        ..
    } = item
    else {
        return 0;
    };
    // Unstyled lines are keyed by their text alone.
    let Some(presentation) = presentation else {
        return *fingerprint;
    };
    let mut hasher = DefaultHasher::new();
    fingerprint.hash(&mut hasher);
    {
        if let Some(style) = &presentation.text_style {
            hash_font_refinement(style, &mut hasher);
        }
        if let Some(indent) = &presentation.indent {
            indent.marker.hash(&mut hasher);
            indent.hanging.hash(&mut hasher);
        }
        for (range, highlight) in &presentation.highlights {
            if highlight.font_weight.is_some() || highlight.font_style.is_some() {
                range.hash(&mut hasher);
                highlight
                    .font_weight
                    .map(|weight| weight.0.to_bits())
                    .hash(&mut hasher);
                highlight.font_style.hash(&mut hasher);
            }
        }
        presentation.font_family_overrides.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_font_refinement(style: &TextStyleRefinement, hasher: &mut DefaultHasher) {
    style.font_family.hash(hasher);
    style.font_features.hash(hasher);
    style.font_fallbacks.hash(hasher);
    style
        .font_size
        .map(|size| match size {
            AbsoluteLength::Pixels(pixels) => (0u8, f32::from(pixels).to_bits()),
            AbsoluteLength::Rems(rems) => (1, rems.0.to_bits()),
        })
        .hash(hasher);
    style
        .font_weight
        .map(|weight| weight.0.to_bits())
        .hash(hasher);
    style.font_style.hash(hasher);
}

/// What changes every advance: cached wraps of another base style are stale.
#[derive(Clone, Debug, PartialEq)]
struct RowsStyle {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    rem_size: Pixels,
}

impl RowsStyle {
    fn new(window: &Window) -> Self {
        let style = window.text_style();
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        Self {
            font: style.font(),
            font_size,
            line_height: style.line_height.to_pixels(font_size.into(), rem_size),
            rem_size,
        }
    }
}

pub(in crate::document) struct TextRowsState {
    rows: TextRows,
    pub(in crate::document) cache: RowCache,
    /// Advances change with the base style; wraps of older generations are stale.
    generation: u64,
    style: Option<RowsStyle>,
    width: Option<Pixels>,
    /// Some item lacks its frame or block height.
    resolve: bool,
    /// Some line may need a wrap, or rows from a newer wrap.
    check: bool,
    /// Every line must be checked, not just those in view.
    scan_all: bool,
    /// The widest printable ASCII character in the base font, once a wrap needs it.
    ascii_advance: Option<Pixels>,
    pending: Option<Task<()>>,
    ready: Option<WrapResult>,
}

struct WrapResult {
    generation: u64,
    lines: Vec<WrappedLine>,
}

/// A line's wrap, identified by its display range and text/font key. The old
/// first-row index is only a lookup hint: another line's reflow can move it.
struct WrappedLine {
    item: usize,
    range: Range<usize>,
    key: u64,
    widths: (Pixels, Pixels),
    starts: Vec<u32>,
}

impl TextRowsState {
    fn new(rows: TextRows) -> Self {
        Self {
            rows,
            cache: RowCache::default(),
            generation: 0,
            style: None,
            width: None,
            resolve: true,
            check: true,
            scan_all: true,
            ascii_advance: None,
            pending: None,
            ready: None,
        }
    }

    /// Whether row geometry is still settling: items wait for layout to frame
    /// or wrap them, or lines out of view wait for a background wrap.
    pub(in crate::document) fn is_wrapping(&self) -> bool {
        self.resolve || self.check || self.pending.is_some() || self.ready.is_some()
    }
}

/// One line to wrap: its display range, the fonts of its runs, and its row widths.
struct LineSpec {
    item: usize,
    key: u64,
    range: Range<usize>,
    /// Each run's end in the line and its font in the table; none for a line
    /// entirely in the base font.
    runs: Vec<(usize, usize)>,
    widths: (Pixels, Pixels),
}

/// The fonts lines wrap in. The base style's font is the first.
struct FontTable {
    base_style: gpui::TextStyle,
    rem_size: Pixels,
    fonts: Vec<(Font, Pixels)>,
}

impl FontTable {
    fn new(window: &Window) -> Self {
        let base_style = window.text_style();
        let rem_size = window.rem_size();
        let fonts = vec![(base_style.font(), base_style.font_size.to_pixels(rem_size))];
        Self {
            base_style,
            rem_size,
            fonts,
        }
    }

    fn index(&mut self, font: Font, size: Pixels) -> usize {
        if let Some(ix) = self
            .fonts
            .iter()
            .position(|(known, known_size)| *known == font && *known_size == size)
        {
            return ix;
        }
        self.fonts.push((font, size));
        self.fonts.len() - 1
    }

    fn wrappers(&self, cx: &App) -> Vec<LineWrapperHandle> {
        self.fonts
            .iter()
            .map(|(font, size)| cx.text_system().line_wrapper(font.clone(), *size))
            .collect()
    }
}

/// A line whose rows differ from its wrap: its items, what it was wrapped for,
/// and where its rows start.
type OutdatedLine = (Range<usize>, WrapKey, Arc<[u32]>);

/// A line whose rows lag behind the wrap its width and fonts need.
struct StaleLine {
    items: Range<usize>,
    bytes: usize,
    wrapped_before: bool,
    in_view: bool,
    widths: (Pixels, Pixels),
}

impl LineSpec {
    fn wrap(&self, text: &str, wrappers: &mut [LineWrapperHandle]) -> WrappedLine {
        WrappedLine {
            item: self.item,
            range: self.range.clone(),
            key: self.key,
            widths: self.widths,
            starts: wrap_line(&text[self.range.clone()], &self.runs, wrappers, self.widths),
        }
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Lay text out as display rows, or `None` to return to one measured item
    /// per line.
    ///
    /// The document wraps each line to its width, less the line's frame, from
    /// the advances of its clusters, and draws each display row unwrapped. Text
    /// is therefore never measured: a row is exactly its line height plus the
    /// space its frame keeps before or after the line, the scroll extent is
    /// exact, and only visible rows are shaped. Blocks declare their height
    /// through [`Self::set_block_height_resolver`], or are measured. Row breaks
    /// fall between grapheme clusters. Positions, line movement, selection and
    /// copy are unchanged; each row reaches the text renderer with its own
    /// source range, and the renderer must draw it within its frame.
    pub fn set_text_rows(&mut self, rows: Option<TextRows>, cx: &mut Context<Self>) {
        match (rows, &mut self.text_rows) {
            (Some(rows), Some(state)) => {
                if state.rows == rows {
                    return;
                }
                state.rows = rows;
                self.clear_row_geometry(|_| true);
            }
            (Some(rows), None) => {
                // Lines are keyed already, and none has wrapped yet: the items
                // stand, one row per line, until layout wraps them.
                self.text_rows = Some(TextRowsState::new(rows));
                self.layout_items_changed();
            }
            (None, Some(_)) => {
                self.text_rows = None;
                self.reconcile_layout_items_from(0);
                // Geometry belongs to the rows that derived it. Without any,
                // rows that come back need not clear it before their first layout.
                for item in &mut self.layout_items {
                    item.forget_geometry();
                }
                // Every item returns to measurement, including retained blocks.
                self.list_state
                    .set_item_heights(0, std::iter::repeat_n(None, self.layout_items.len()));
            }
            (None, None) => return,
        }
        cx.notify();
    }

    /// Frames each display line of text rows: the space before and after it,
    /// its insets and its widest text. Called during layout, for lines that are
    /// new or whose node [`Self::remeasure_node`] invalidated, with the context
    /// the text renderer receives, covering the whole line.
    pub fn set_text_frame_resolver(
        &mut self,
        resolver: impl Fn(&DocumentTextContext<I>, &Window, &App) -> TextFrame + 'static,
        cx: &mut Context<Self>,
    ) {
        self.text_frame_resolver = Some(std::rc::Rc::new(resolver));
        self.clear_row_geometry(|_| true);
        cx.notify();
    }

    /// Declares the height of blocks in text rows, or `None` for a block the
    /// list measures. Called during layout for blocks that are new or that
    /// [`Self::remeasure_block`] invalidated.
    pub fn set_block_height_resolver(
        &mut self,
        resolver: impl Fn(&I, &Window, &App) -> Option<AbsoluteLength> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.block_height_resolver = Some(std::rc::Rc::new(resolver));
        self.clear_row_geometry(|_| true);
        cx.notify();
    }

    /// Forgets the frames and block heights of items whose node `owned`
    /// selects. Returns whether any item was cleared.
    pub(super) fn clear_row_geometry(&mut self, owned: impl Fn(Option<&I>) -> bool) -> bool {
        let Some(state) = self.text_rows.as_mut() else {
            return false;
        };
        let mut cleared = false;
        for item in &mut self.layout_items {
            match item {
                DocumentLayoutItem::Text { node_id, frame, .. } if owned(node_id.as_ref()) => {
                    frame.0 = None;
                    cleared = true;
                }
                DocumentLayoutItem::Block { id, height, .. }
                | DocumentLayoutItem::AnchoredBlock { id, height, .. }
                    if owned(Some(id)) =>
                {
                    height.0 = BlockGeometry::Unresolved;
                    cleared = true;
                }
                DocumentLayoutItem::Trailer => cleared = true,
                _ => {}
            }
        }
        state.resolve |= cleared;
        state.check |= cleared;
        state.scan_all |= cleared;
        cleared
    }

    /// Layout items were added or replaced; layout resolves and wraps them.
    pub(super) fn layout_items_changed(&mut self) {
        if let Some(state) = self.text_rows.as_mut() {
            state.resolve = true;
            state.check = true;
            state.scan_all = true;
        }
    }

    /// Keeps every row's frame, every block's height and the wrap of the lines
    /// in view current, before the list lays out.
    pub(super) fn prepare_text_rows(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        let style = RowsStyle::new(window);
        if state.style.as_ref() != Some(&style) {
            // Before the first style, rows have derived no geometry to clear.
            state.ascii_advance = None;
            if state.style.replace(style).is_some() {
                state.generation += 1;
                self.clear_row_geometry(|_| true);
            }
        }
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        if state.width != Some(bounds.size.width) {
            state.width = Some(bounds.size.width);
            state.check = true;
            state.scan_all = true;
        }
        if state.resolve {
            state.resolve = false;
            self.resolve_row_geometry(window, cx);
        }
        if let Some(state) = self.text_rows.as_mut()
            && let Some(result) = state.ready.take()
        {
            state.check = true;
            state.scan_all = true;
            if result.generation == state.generation {
                let (settled, outdated) = self.take_wrapped(result.generation, result.lines);
                self.settle_line_rows(settled, outdated, cx);
            }
        }
        if self.text_rows.as_ref().is_some_and(|state| state.check) {
            self.wrap_stale_lines(bounds, window, cx);
        }
    }

    /// Resolves the frame of every line and the height of every block that
    /// lacks one, and declares their heights to the list.
    fn resolve_row_geometry(&mut self, window: &Window, cx: &mut Context<Self>) {
        let base_style = window.text_style();
        // Without a resolver, every unstyled line has the same frame.
        let mut plain_frame = None;
        let mut heights: Vec<(usize, Option<Pixels>)> = Vec::new();
        let mut ix = 0;
        while ix < self.layout_items.len() {
            match &self.layout_items[ix] {
                DocumentLayoutItem::Text { frame, .. } if frame.0.is_some() => ix += 1,
                DocumentLayoutItem::Text { .. } => {
                    // The first row without a frame frames its whole line.
                    let rows = self.line_rows(ix);
                    {
                        let plain = self.text_frame_resolver.is_none()
                            && matches!(
                                &self.layout_items[rows.start],
                                DocumentLayoutItem::Text {
                                    presentation: None,
                                    ..
                                }
                            );
                        let resolved = match plain_frame {
                            Some(frame) if plain => frame,
                            _ => {
                                let frame =
                                    self.resolve_line_frame(rows.clone(), &base_style, window, cx);
                                if plain {
                                    plain_frame = Some(frame);
                                }
                                frame
                            }
                        };
                        for row_ix in rows.clone() {
                            if let DocumentLayoutItem::Text {
                                display,
                                line,
                                frame,
                                ..
                            } = &mut self.layout_items[row_ix]
                            {
                                frame.0 = Some(resolved);
                                heights.push((row_ix, Some(resolved.row_height(display, line))));
                            }
                        }
                    }
                    ix = rows.end;
                }
                DocumentLayoutItem::Block { id, height, .. }
                | DocumentLayoutItem::AnchoredBlock { id, height, .. } => {
                    if height.0 == BlockGeometry::Unresolved {
                        let geometry = match self.block_height_resolver.clone() {
                            Some(resolver) => match resolver(id, window, cx) {
                                Some(height) => {
                                    BlockGeometry::Declared(height.to_pixels(window.rem_size()))
                                }
                                None => BlockGeometry::Measured,
                            },
                            None => BlockGeometry::Measured,
                        };
                        if let DocumentLayoutItem::Block { height, .. }
                        | DocumentLayoutItem::AnchoredBlock { height, .. } =
                            &mut self.layout_items[ix]
                        {
                            height.0 = geometry;
                        }
                        heights.push((ix, geometry.height()));
                    }
                    ix += 1;
                }
                DocumentLayoutItem::Trailer => {
                    heights.push((ix, Some(self.trailer_item_height(window))));
                    ix += 1;
                }
            }
        }
        // One update per run of adjacent items.
        let mut heights = heights.into_iter().peekable();
        while let Some((start, height)) = heights.next() {
            let mut run = vec![height];
            while let Some((_, height)) = heights.next_if(|(next, _)| *next == start + run.len()) {
                run.push(height);
            }
            self.list_state.set_item_heights(start, run);
        }
    }

    pub(super) fn trailer_item_height(&self, window: &Window) -> Pixels {
        self.trailer_height
            .unwrap_or_else(|| window.viewport_size().height)
    }

    /// The text items of the display line that contains text item `ix`.
    pub(super) fn line_rows(&self, ix: usize) -> Range<usize> {
        let DocumentLayoutItem::Text { line, node_id, .. } = &self.layout_items[ix] else {
            return ix..ix + 1;
        };
        let same_line = |item: &DocumentLayoutItem<I>| {
            matches!(item, DocumentLayoutItem::Text { line: other, node_id: other_node, .. }
                if other == line && other_node == node_id)
        };
        let mut start = ix;
        while start > 0 && same_line(&self.layout_items[start - 1]) {
            start -= 1;
        }
        let mut end = ix + 1;
        while end < self.layout_items.len() && same_line(&self.layout_items[end]) {
            end += 1;
        }
        start..end
    }

    fn resolve_line_frame(
        &self,
        rows: Range<usize>,
        base_style: &gpui::TextStyle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> RowFrame {
        let (
            DocumentLayoutItem::Text {
                source: first_source,
                node_id,
                presentation,
                ..
            },
            DocumentLayoutItem::Text {
                source: last_source,
                ..
            },
        ) = (
            &self.layout_items[rows.start],
            &self.layout_items[rows.end - 1],
        )
        else {
            unreachable!("line rows are text items");
        };
        let rem_size = window.rem_size();
        let frame = match (self.text_frame_resolver.clone(), node_id) {
            (Some(resolver), Some(node_id)) => {
                let region = self
                    .region_at(node_id, first_source.start)
                    .map(DocumentRegion::range);
                let context = DocumentTextContext {
                    node_id: node_id.clone(),
                    source: first_source.start..last_source.end,
                    node_start: region.as_ref().map_or(0, |region| region.start),
                    first_line: region.is_some_and(|region| first_source.start <= region.start),
                    first_row: true,
                    last_row: true,
                };
                resolver(&context, window, cx)
            }
            _ => self
                .text_rows
                .as_ref()
                .map(|state| TextFrame::new().with_inset_start(state.rows.inset))
                .unwrap_or_default(),
        };
        let refinement = presentation
            .as_ref()
            .and_then(|presentation| presentation.text_style.as_ref());
        let style = match refinement {
            Some(refinement) => {
                let mut style = base_style.clone();
                style.refine(refinement);
                std::borrow::Cow::Owned(style)
            }
            None => std::borrow::Cow::Borrowed(base_style),
        };
        let font_size = style.font_size.to_pixels(rem_size);
        let (indent, hanging) = presentation
            .as_ref()
            .and_then(|presentation| presentation.indent.as_ref())
            .map_or((px(0.), true), |indent| {
                // Measured as the text element measures it, in the line's own style.
                let width = window
                    .text_system()
                    .shape_line(
                        indent.marker.clone(),
                        font_size,
                        &[style.to_run(indent.marker.len())],
                        None,
                    )
                    .width;
                (width, indent.hanging)
            });
        RowFrame {
            space_before: frame.space_before.to_pixels(rem_size),
            space_after: frame.space_after.to_pixels(rem_size),
            inset_start: frame.inset_start.to_pixels(rem_size),
            inset_end: frame.inset_end.to_pixels(rem_size),
            max_width: frame.max_width.map(|width| width.to_pixels(rem_size)),
            hidden: frame.hidden,
            // The line height a text element lays out with, snapped to device pixels.
            line_height: window.pixel_snap(style.line_height.to_pixels(font_size.into(), rem_size)),
            indent,
            hanging,
        }
    }

    /// Items around the viewport, before the list lays out this frame.
    fn items_in_view(&self, bounds: Bounds<Pixels>) -> Range<usize> {
        let height = |item: &DocumentLayoutItem<I>| match item {
            DocumentLayoutItem::Text {
                display,
                line,
                frame,
                ..
            } => frame
                .0
                .map_or(px(0.), |frame| frame.row_height(display, line)),
            DocumentLayoutItem::Block { height, .. }
            | DocumentLayoutItem::AnchoredBlock { height, .. } => {
                height.0.height().unwrap_or_default()
            }
            DocumentLayoutItem::Trailer => px(0.),
        };
        let top = self.list_state.logical_scroll_top();
        let margin = bounds.size.height * VIEW_MARGIN;
        let first = top.item_ix.min(self.layout_items.len());
        let mut start = first;
        let mut above = -top.offset_in_item;
        while start > 0 && above < margin {
            start -= 1;
            above += height(&self.layout_items[start]);
        }
        let mut end = first;
        let mut below = -top.offset_in_item;
        while end < self.layout_items.len() && below < bounds.size.height + margin {
            below += height(&self.layout_items[end]);
            end += 1;
        }
        start..end
    }

    /// Wraps the lines whose rows are stale for this width and style: those in
    /// view and a bounded amount more now, the rest in the background.
    fn wrap_stale_lines(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let ascii_advance = self.ascii_advance(window, cx);
        let Some(state) = self.text_rows.as_ref() else {
            return;
        };
        let width = bounds.size.width;
        let generation = state.generation;
        let view = self.items_in_view(bounds);
        // While a background wrap is out, only lines scrolling into view can
        // have become stale; its result brings back a full check.
        let scan_all = state.scan_all;
        let (mut ix, end) = if scan_all {
            (0, self.layout_items.len())
        } else {
            let start = (view.start < self.layout_items.len())
                .then(|| self.line_rows(view.start).start)
                .unwrap_or(view.start);
            (start, view.end)
        };
        let mut stale = Vec::new();
        let mut settled = Vec::new();
        let mut outdated = Vec::new();
        while ix < end {
            let DocumentLayoutItem::Text {
                line,
                line_key,
                presentation,
                frame: Derived(Some(frame)),
                wrapped,
                ..
            } = &self.layout_items[ix]
            else {
                ix += 1;
                continue;
            };
            let rows = self.line_rows(ix);
            ix = rows.end;
            if frame.hidden || line.is_empty() {
                continue;
            }
            let wrap = WrapKey {
                generation,
                widths: frame.widths(width),
            };
            if wrapped.0 == Some(wrap) {
                continue;
            }
            // Wrapping cannot break a line that never reaches its width.
            if presentation.is_none()
                && ascii_advance.is_some_and(|advance| {
                    fits_unwrapped(
                        &self.model.display_text()[line.clone()],
                        advance,
                        wrap.widths.0,
                    )
                })
            {
                if rows.len() == 1 {
                    settled.push((rows, wrap));
                } else {
                    outdated.push((rows, wrap, Arc::from([])));
                }
                continue;
            }
            match state.cache.lines.get(line_key) {
                // Another line with this text and fonts already wrapped for it.
                Some(cached) if cached.generation == generation && cached.widths == wrap.widths => {
                    if self.rows_match(rows.clone(), &cached.starts) {
                        settled.push((rows, wrap));
                    } else {
                        outdated.push((rows, wrap, cached.starts.clone()));
                    }
                }
                cached => stale.push(StaleLine {
                    in_view: rows.start < view.end && view.start < rows.end,
                    items: rows,
                    bytes: line.len(),
                    wrapped_before: cached.is_some() || wrapped.0.is_some(),
                    widths: wrap.widths,
                }),
            }
        }

        // A text new to rows wraps whole in the frame only when it is small;
        // a larger one shows its lines in view and wraps the rest behind.
        let first_bytes: usize = stale
            .iter()
            .filter(|line| !line.wrapped_before)
            .map(|line| line.bytes)
            .sum();
        let mut first_budget = if first_bytes <= FIRST_WRAP_IN_FRAME_BYTES {
            first_bytes
        } else {
            0
        };
        let mut rewrap_budget = REWRAP_IN_FRAME_BYTES;
        let (now, later): (Vec<_>, Vec<_>) = stale.into_iter().partition(|line| {
            let budget = if line.wrapped_before {
                &mut rewrap_budget
            } else {
                &mut first_budget
            };
            if line.in_view {
                *budget = budget.saturating_sub(line.bytes);
                return true;
            }
            if line.bytes <= *budget {
                *budget -= line.bytes;
                return true;
            }
            false
        });

        if !now.is_empty() {
            let mut fonts = FontTable::new(window);
            let specs: Vec<_> = now
                .iter()
                .map(|line| self.line_spec(line, &mut fonts))
                .collect();
            let mut wrappers = fonts.wrappers(cx);
            let text = self.model.display_text();
            let lines = specs
                .iter()
                .map(|spec| spec.wrap(text, &mut wrappers))
                .collect();
            let (now_settled, now_outdated) = self.take_wrapped(generation, lines);
            settled.extend(now_settled);
            outdated.extend(now_outdated);
        }
        // Both batches refer to the current layout. Freeze the background
        // inputs before settling rows changes the indices of later lines.
        let background = (scan_all
            && !later.is_empty()
            && self
                .text_rows
                .as_ref()
                .is_some_and(|state| state.pending.is_none()))
        .then(|| {
            let mut fonts = FontTable::new(window);
            let specs: Vec<_> = later
                .iter()
                .map(|line| self.line_spec(line, &mut fonts))
                .collect();
            (specs, fonts.wrappers(cx))
        });
        self.settle_line_rows(settled, outdated, cx);

        let state = self.text_rows.as_mut().expect("text rows are on");
        state.scan_all = false;
        if !scan_all {
            return;
        }
        // Lines still stale wrap as they scroll into view.
        state.check = !later.is_empty();
        let Some((specs, mut wrappers)) = background else {
            return;
        };
        let text: Arc<str> = self.model.display_text().into();
        let task = cx.spawn(async move |this, cx| {
            let lines = cx
                .background_spawn(async move {
                    specs
                        .iter()
                        .map(|spec| spec.wrap(&text, &mut wrappers))
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.text_rows.as_mut() else {
                    return;
                };
                // This task is finishing; let it complete rather than cancel it.
                if let Some(task) = state.pending.take() {
                    task.detach();
                }
                // Width and font are known only during layout, which validates it.
                state.ready = Some(WrapResult { generation, lines });
                cx.notify();
            });
        });
        let state = self.text_rows.as_mut().expect("text rows are on");
        state.pending = Some(task);
    }

    /// Records finished wraps and sorts their lines into those whose rows
    /// already match, and those to lay out again. A line that changed since its
    /// wrap began is left for the next check.
    fn take_wrapped(
        &mut self,
        generation: u64,
        lines: Vec<WrappedLine>,
    ) -> (Vec<(Range<usize>, WrapKey)>, Vec<OutdatedLine>) {
        let mut settled = Vec::new();
        let mut outdated = Vec::new();
        for line in lines {
            let matches = |ix| {
                matches!(self.layout_items.get(ix), Some(DocumentLayoutItem::Text {
                    display, line: range, line_key, ..
                }) if *range == line.range && *line_key == line.key && display.start == range.start)
            };
            let item = if matches(line.item) {
                line.item
            } else {
                let Some(item) = self
                    .items_around_display(line.range.start)
                    .find(|ix| matches(*ix))
                else {
                    continue;
                };
                item
            };
            let rows = self.line_rows(item);
            let wrap = WrapKey {
                generation,
                widths: line.widths,
            };
            let matches = self.rows_match(rows.clone(), &line.starts);
            if let Some(state) = self.text_rows.as_mut() {
                state.cache.record(line.key, wrap, &line.starts);
            }
            if matches {
                settled.push((rows, wrap));
            } else {
                outdated.push((rows, wrap, line.starts.into()));
            }
        }
        (settled, outdated)
    }

    /// Marks lines whose rows match their wrap, and lays out again the rows of
    /// those that do not.
    fn settle_line_rows(
        &mut self,
        settled: Vec<(Range<usize>, WrapKey)>,
        mut outdated: Vec<OutdatedLine>,
        cx: &mut Context<Self>,
    ) {
        for (rows, wrap) in settled {
            for item in &mut self.layout_items[rows] {
                if let DocumentLayoutItem::Text { wrapped, .. } = item {
                    wrapped.0 = Some(wrap);
                }
            }
        }
        if outdated.is_empty() {
            return;
        }
        outdated.sort_by_key(|(rows, _, _)| rows.start);
        outdated.dedup_by_key(|(rows, _, _)| rows.start);
        self.replace_line_rows(outdated, cx);
    }

    /// Whether the rows of line items `rows` start where `starts` says.
    fn rows_match(&self, rows: Range<usize>, starts: &[u32]) -> bool {
        if rows.len() != starts.len() + 1 {
            return false;
        }
        self.layout_items[rows][1..]
            .iter()
            .zip(starts)
            .all(|(item, start)| {
                matches!(item, DocumentLayoutItem::Text { display, line, .. }
                    if display.start - line.start == *start as usize)
            })
    }

    /// The widest printable ASCII character in the base font.
    fn ascii_advance(&mut self, window: &Window, cx: &App) -> Option<Pixels> {
        let state = self.text_rows.as_mut()?;
        if state.ascii_advance.is_none() {
            let mut wrappers = FontTable::new(window).wrappers(cx);
            let mut buffer = [0; 4];
            state.ascii_advance = Some((b' '..=b'~').fold(Pixels::ZERO, |widest, byte| {
                widest.max(wrappers[0].cluster_width(char::from(byte).encode_utf8(&mut buffer)))
            }));
        }
        state.ascii_advance
    }

    fn line_spec(&self, line: &StaleLine, fonts: &mut FontTable) -> LineSpec {
        let (
            DocumentLayoutItem::Text {
                source: first_source,
                line: display,
                line_key,
                ..
            },
            DocumentLayoutItem::Text {
                source: last_source,
                ..
            },
        ) = (
            &self.layout_items[line.items.start],
            &self.layout_items[line.items.end - 1],
        )
        else {
            unreachable!("line rows are text items");
        };
        let spec = |runs| LineSpec {
            item: line.items.start,
            key: *line_key,
            range: display.clone(),
            runs,
            widths: line.widths,
        };
        // Unstyled lines wrap in the base font.
        if self.layout_items[line.items.clone()].iter().all(|item| {
            matches!(
                item,
                DocumentLayoutItem::Text {
                    presentation: None,
                    ..
                }
            )
        }) {
            return spec(Vec::new());
        }
        let whole = self.model.text_layout_item(
            self.model.display_text(),
            display.clone(),
            first_source.start..last_source.end,
            display.clone(),
            *line_key,
        );
        let DocumentLayoutItem::Text {
            presentation: Some(presentation),
            ..
        } = &whole
        else {
            return spec(Vec::new());
        };
        let mut style = fonts.base_style.clone();
        if let Some(refinement) = &presentation.text_style {
            style.refine(refinement);
        }
        let size = style.font_size.to_pixels(fonts.rem_size);
        let base = style.font();
        let mut boundaries = vec![0, display.len()];
        for (range, _) in &presentation.highlights {
            boundaries.extend([range.start, range.end]);
        }
        for (range, _) in &presentation.font_family_overrides {
            boundaries.extend([range.start, range.end]);
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let runs = boundaries
            .windows(2)
            .map(|run| {
                let covers = |range: &Range<usize>| range.start <= run[0] && run[1] <= range.end;
                let mut font = base.clone();
                for (range, highlight) in &presentation.highlights {
                    if covers(range) {
                        if let Some(weight) = highlight.font_weight {
                            font.weight = weight;
                        }
                        if let Some(font_style) = highlight.font_style {
                            font.style = font_style;
                        }
                    }
                }
                for (range, family) in &presentation.font_family_overrides {
                    if covers(range) {
                        font.family = family.clone();
                    }
                }
                (run[1], fonts.index(font, size))
            })
            .collect();
        spec(runs)
    }

    /// Replaces the rows of each line in `lines`, given in ascending order by
    /// their items, with the rows of its wrap, keeping the visible text in place.
    fn replace_line_rows(&mut self, lines: Vec<OutdatedLine>, cx: &mut Context<Self>) {
        // Layout has not placed this frame's rows yet, so keep the top row by
        // its text rather than by last frame's geometry.
        let top = self.list_state.logical_scroll_top();
        let top_offset = match self.layout_items.get(top.item_ix) {
            Some(DocumentLayoutItem::Text { display, .. }) => Some(display.start),
            _ => None,
        };
        let mut top_row = None;
        let reanchor = self
            .pending_viewport_anchor
            .as_ref()
            .filter(|pending| !pending.reveal_on_wrap)
            .and_then(|pending| {
                let position = self.model.position_for_anchor(pending.anchor).ok()?;
                // A replaced source can leave an anchor with no current geometry.
                self.text_position_for_position(&position)?;
                self.resolve_position(&position).ok()
            });
        if self.text_rows.is_none() {
            return;
        }
        let display_text = self.model.display_text();
        let old_len = self.layout_items.len();
        let mut old = std::mem::take(&mut self.layout_items).into_iter();
        let mut items = Vec::with_capacity(old_len);
        let mut splices = Vec::with_capacity(lines.len());
        let mut moved = 0;
        for (rows, wrap, starts) in lines {
            items.extend(old.by_ref().take(rows.start - moved));
            let replaced: Vec<_> = old.by_ref().take(rows.len()).collect();
            moved = rows.end;
            let (
                DocumentLayoutItem::Text {
                    source: first_source,
                    line,
                    line_key,
                    frame,
                    ..
                },
                DocumentLayoutItem::Text {
                    source: last_source,
                    ..
                },
            ) = (&replaced[0], &replaced[replaced.len() - 1])
            else {
                unreachable!("line rows are text items");
            };
            let segment = first_source.start..last_source.end;
            let new_start = items.len();
            let mut heights = Vec::new();
            for row in rows_of_line(line.clone(), &starts) {
                let mut item = self.model.text_layout_item(
                    display_text,
                    row.clone(),
                    segment.clone(),
                    line.clone(),
                    *line_key,
                );
                if let DocumentLayoutItem::Text {
                    frame: row_frame,
                    wrapped,
                    ..
                } = &mut item
                {
                    *row_frame = *frame;
                    wrapped.0 = Some(wrap);
                }
                heights.push(frame.0.map(|frame| frame.row_height(&row, line)));
                items.push(item);
            }
            if let Some(offset) = top_offset.filter(|_| rows.contains(&top.item_ix)) {
                let row = items[new_start..]
                    .iter()
                    .rposition(|item| {
                        matches!(item, DocumentLayoutItem::Text { display, .. } if display.start <= offset)
                    })
                    .unwrap_or(0);
                top_row = Some(new_start + row);
            }
            splices.push((rows, heights));
        }
        items.extend(old);
        self.layout_items = items;
        // Splice from the end so earlier ranges keep their indices.
        for (rows, heights) in splices.into_iter().rev() {
            self.list_state.splice_with_heights(rows, heights);
        }
        if let Some(item_ix) = top_row {
            self.list_state.scroll_to(gpui::ListOffset {
                item_ix,
                offset_in_item: top.offset_in_item,
            });
        }
        // Re-index a retained visible anchor so its geometry can be restored.
        // Explicit reveals wait until layout supplies the new viewport height.
        if let Some(offset) = reanchor {
            self.list_state.scroll_to(gpui::ListOffset {
                item_ix: self.layout_item_for_source(offset),
                offset_in_item: px(0.),
            });
        }
        self.prune_row_cache();
        cx.notify();
    }

    /// Drops the cached wraps of lines no longer in the document once they
    /// outnumber those that are.
    fn prune_row_cache(&mut self) {
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        if state.cache.lines.len() <= 2 * self.layout_items.len() + 1024 {
            return;
        }
        let live: std::collections::HashSet<u64> = self
            .layout_items
            .iter()
            .filter_map(|item| match item {
                DocumentLayoutItem::Text { line_key, .. } => Some(*line_key),
                _ => None,
            })
            .collect();
        state.cache.lines.retain(|key, _| live.contains(key));
    }
}

/// The display rows of `line`, split at `starts`, which are relative to the line.
pub(in crate::document) fn rows_of_line(
    line: Range<usize>,
    starts: &[u32],
) -> impl Iterator<Item = Range<usize>> + '_ {
    let end = line.end;
    let starts = starts
        .iter()
        .map(move |start| line.start + *start as usize)
        .filter(move |start| *start < end);
    let row_starts = std::iter::once(line.start).chain(starts.clone());
    let row_ends = starts.chain(std::iter::once(end));
    row_starts.zip(row_ends).map(|(start, end)| start..end)
}

/// Where rows after the first start in `line` when its first row wraps to
/// `widths.0` and later rows to `widths.1`, relative to the line. `runs` gives
/// each run's end and the wrapper of its font.
///
/// Rows break with GPUI's wrapping rules but are measured as they are drawn,
/// each as its own unwrapped line: a grapheme cluster advances by its shaped
/// width in its run's font (a ZWJ sequence is one glyph), and a tab advances
/// to the next tab stop from the row's start. Rows therefore never split a
/// cluster. A tab exactly at a stop counts a whole tab, so a row is never
/// estimated narrower than drawn.
fn wrap_line(
    line: &str,
    runs: &[(usize, usize)],
    wrappers: &mut [LineWrapperHandle],
    widths: (Pixels, Pixels),
) -> Vec<u32> {
    let font_at = |ix: usize| {
        runs.get(runs.partition_point(|(end, _)| *end <= ix))
            .or(runs.last())
            .map_or(0, |(_, font)| *font)
    };
    let mut starts = Vec::new();
    let mut row_start = 0;
    let mut width = widths.0;
    let mut x = px(0.);
    // A break opportunity in the current row, after the line's leading indent.
    let mut candidate = None;
    let mut indented = false;
    let mut previous = '\0';
    for (ix, cluster) in line.grapheme_indices(true) {
        let first = cluster.chars().next().unwrap_or_default();
        if indented && LineWrapper::can_break_before(previous, first) {
            candidate = Some(ix);
        }
        indented |= first != ' ';
        let mut cluster_advance = advance(&mut wrappers[font_at(ix)], cluster, x);
        if x + cluster_advance > width && ix > row_start {
            let start = candidate.filter(|start| *start > row_start).unwrap_or(ix);
            starts.push(start as u32);
            row_start = start;
            width = widths.1;
            candidate = None;
            // Tab stops restart with the row, so measure the carried text again.
            x = line[start..ix]
                .grapheme_indices(true)
                .fold(px(0.), |x, (offset, cluster)| {
                    x + advance(&mut wrappers[font_at(start + offset)], cluster, x)
                });
            cluster_advance = advance(&mut wrappers[font_at(ix)], cluster, x);
        }
        x += cluster_advance;
        previous = cluster.chars().last().unwrap_or(first);
    }
    starts
}

/// Whether `wrap_line` keeps `line` whole at `width` without measuring it: in
/// printable ASCII every character is its own cluster, so the line is never
/// wider than its length times the widest one. Half a pixel absorbs rounding
/// in the wrap's running sum.
fn fits_unwrapped(line: &str, ascii_advance: Pixels, width: Pixels) -> bool {
    ascii_advance * line.len() as f32 <= width - px(0.5)
        && line.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// How far `cluster` advances a row already `x` wide.
fn advance(wrapper: &mut LineWrapper, cluster: &str, x: Pixels) -> Pixels {
    if cluster != "\t" {
        return wrapper.cluster_width(cluster);
    }
    let tab = wrapper.char_width('\t');
    if tab <= px(0.) {
        return px(0.);
    }
    // Stops at every tab width; landing on one (within rounding) moves to the next.
    let stops = ((x + px(0.01)) / tab).floor();
    tab * (stops + 1.) - x
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
        VisualTestContext, div, prelude::FluentBuilder as _, size,
    };

    use super::{super::tests::document_view, *};
    use crate::document::{
        DocumentBlock, DocumentParagraphStyle, DocumentProjection, DocumentSnapshot,
        DocumentStyles, EditPolicy, ProjectionSpan,
    };

    #[test]
    fn rows_split_lines_at_starts_relative_to_the_line() {
        assert_eq!(
            rows_of_line(4..20, &[5, 11]).collect::<Vec<_>>(),
            [4..9, 9..15, 15..20]
        );
        assert_eq!(rows_of_line(4..4, &[]).collect::<Vec<_>>(), [4..4]);
        // Starts of a longer line under the same key never reach past this one.
        assert_eq!(
            rows_of_line(0..6, &[3, 9]).collect::<Vec<_>>(),
            [0..3, 3..6]
        );
    }

    #[gpui::test]
    fn wrapped_rows_start_between_grapheme_clusters(cx: &mut TestAppContext) {
        let line = format!(
            "{}{}",
            "e\u{301}👨\u{200d}👩\u{200d}👧xy中".repeat(40),
            "plain words wrap at spaces ".repeat(20)
        );
        let mut wrappers = vec![
            cx.text_system()
                .line_wrapper(gpui::font("Zed Mono"), px(16.)),
        ];
        let starts = wrap_line(&line, &[(line.len(), 0)], &mut wrappers, (px(97.), px(61.)));
        assert!(starts.len() > 10);
        let clusters: Vec<_> = line.grapheme_indices(true).map(|(ix, _)| ix).collect();
        let mut previous = 0;
        for start in &starts {
            let start = *start as usize;
            assert!(
                clusters.contains(&start),
                "row starts inside a cluster at {start}"
            );
            assert!(start > previous);
            previous = start;
        }
    }

    #[test]
    fn rows_are_never_wider_than_they_are_drawn() {
        // The platform's own text system, so clusters, tabs and fallback fonts
        // shape as they do in the app.
        let mut app =
            gpui::TestApp::with_text_system(gpui_platform::current_platform(true).text_system());
        struct Empty;
        impl Render for Empty {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }
        // Bundled fonts, so the test does not depend on what the machine has.
        app.text_system()
            .add_fonts(vec![
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../story-web/fonts/JetBrainsMono-Regular.ttf"
                ))
                .as_slice()
                .into(),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../story-web/fonts/NotoSansSC-Regular-subset.ttf"
                ))
                .as_slice()
                .into(),
            ])
            .unwrap();
        let mut window = app.open_window(|_, _| Empty);
        let json = format!(
            "{{{}}}",
            (0..60)
                .map(|i| format!("\"k{i}\":\"v {i}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        let texts = [
            "\tfn\tmain()\t{\t}\tlet\tx\t=\t1;\t".repeat(12),
            "family 👨\u{200d}👩\u{200d}👧\u{200d}👦 skin 👍🏽 flag 🏳\u{fe0f}\u{200d}🌈 ".repeat(12),
            "e\u{301}a\u{308}n\u{303}o\u{302} Z\u{335}a\u{336}l\u{337}g\u{338}o ".repeat(24),
            "中文的长段落，包含标点；English words 12345 と日本語のテキスト。".repeat(12),
            json,
        ];
        window.update(|_, window, cx| {
            let font = gpui::font("JetBrains Mono");
            let size = px(14.);
            let mut wrappers = vec![cx.text_system().line_wrapper(font.clone(), size)];
            let run = |len| gpui::TextRun {
                len,
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            for text in &texts {
                let clusters: Vec<_> = text.grapheme_indices(true).map(|(ix, _)| ix).collect();
                for width in [px(120.), px(310.)] {
                    let starts = wrap_line(text, &[(text.len(), 0)], &mut wrappers, (width, width));
                    assert!(!starts.is_empty(), "{text:?} wraps at {width:?}");
                    for row in rows_of_line(0..text.len(), &starts) {
                        assert!(row.end == text.len() || clusters.contains(&row.end));
                        let row = &text[row];
                        let drawn = window
                            .text_system()
                            .shape_line(row.to_owned().into(), size, &[run(row.len())], None)
                            .width;
                        // A cluster wider than a row keeps a row to itself.
                        assert!(
                            drawn <= width + px(0.01) || row.graphemes(true).count() == 1,
                            "{row:?} draws {drawn:?} in {width:?}"
                        );
                    }
                }
            }
        });
    }

    const OBJECT: &str = "\u{fffc}";

    /// Heading, a long list paragraph, a declared block, a measured block and a
    /// draft, framed by `frame`.
    fn framed_document(
        paragraph: &str,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DocumentState<&'static str>>,
        VisualTestContext,
        Rc<RefCell<Vec<usize>>>,
    ) {
        let (document, mut cx) = document_view(cx);
        let rendered = Rc::new(RefCell::new(Vec::new()));
        let source = format!("Heading\n- {paragraph}\n{OBJECT}{OBJECT}draft");
        let text_end = source.len() - 2 * OBJECT.len() - "draft".len();
        let fixed = text_end..text_end + OBJECT.len();
        let measured = fixed.end..fixed.end + OBJECT.len();
        let heading = DocumentParagraphStyle::new(
            0..8,
            gpui::TextStyleRefinement {
                font_size: Some(px(24.).into()),
                ..Default::default()
            },
        );
        let list =
            DocumentParagraphStyle::new(8..text_end, Default::default()).with_hanging_marker("- ");
        let renders = rendered.clone();
        document.update(&mut cx, |document, cx| {
            document.set_block_renderer(
                |id, _, _| {
                    div()
                        .w_full()
                        .h(px(if *id == "fixed" { 40. } else { 33. }))
                        .into_any_element()
                },
                cx,
            );
            document.set_text_renderer(
                move |context, text, _, _| {
                    let _ = context;
                    renders.borrow_mut().push(0);
                    div()
                        .pl(px(10.))
                        .when(context.is_first_row(), |el| {
                            el.pt(px(if context.is_first_line() { 6. } else { 0. }))
                        })
                        .child(text)
                        .into_any_element()
                },
                cx,
            );
            document.set_text_frame_resolver(
                |context, _, _| {
                    TextFrame::new()
                        .with_space_before(px(if context.is_first_line() { 6. } else { 0. }))
                        .with_inset_start(px(10.))
                        .with_max_width(px(200.))
                        .with_hidden(*context.node_id() == "hidden")
                },
                cx,
            );
            document.set_block_height_resolver(
                |id, _, _| (*id == "fixed").then_some(px(40.).into()),
                cx,
            );
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .reset(
                    DocumentSnapshot::new(
                        source.clone(),
                        vec![
                            DocumentRegion::new("text", 0..text_end, EditPolicy::Readonly),
                            DocumentRegion::new("fixed", fixed.clone(), EditPolicy::Atomic),
                            DocumentRegion::new("measured", measured.clone(), EditPolicy::Atomic),
                            DocumentRegion::new(
                                "draft",
                                measured.end..source.len(),
                                EditPolicy::Editable,
                            ),
                        ],
                        DocumentProjection::new(
                            source.len(),
                            vec![
                                ProjectionSpan::hide(fixed.clone()),
                                ProjectionSpan::hide(measured.clone()),
                            ],
                        )
                        .unwrap(),
                        vec![
                            DocumentBlock::new("fixed", fixed),
                            DocumentBlock::new("measured", measured),
                        ],
                        DocumentStyles::new(vec![heading, list], Vec::new()),
                    ),
                    cx,
                )
                .unwrap();
        });
        cx.simulate_resize(size(px(320.), px(400.)));
        (document, cx, rendered)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    /// Every item's list height is what its frame or block declares, and the
    /// list extent is their sum.
    fn assert_declared_geometry(
        document: &Entity<DocumentState<&'static str>>,
        cx: &mut VisualTestContext,
    ) {
        document.read_with(cx, |document, _| {
            let list = &document.list_state;
            let mut total = px(0.);
            for (ix, item) in document.layout_items.iter().enumerate() {
                let height = match item {
                    DocumentLayoutItem::Text {
                        display,
                        line,
                        frame,
                        ..
                    } => frame
                        .0
                        .expect("every row is framed")
                        .row_height(display, line),
                    DocumentLayoutItem::Block { id, height, .. } => match height.0 {
                        BlockGeometry::Declared(height) => height,
                        BlockGeometry::Measured => {
                            assert_eq!(*id, "measured");
                            px(33.)
                        }
                        BlockGeometry::Unresolved => panic!("block {id} is unresolved"),
                    },
                    _ => unreachable!(),
                };
                if let Some(bounds) = list.bounds_for_item(ix) {
                    assert_eq!(bounds.size.height, height, "item {ix}: {item:?}");
                }
                total += height;
            }
            let extent = list.max_offset_for_scrollbar().y + list.viewport_bounds().size.height;
            assert_eq!(extent.max(total), extent);
            if total >= list.viewport_bounds().size.height {
                assert_eq!(extent, total);
            }
        });
    }

    fn line_rows(
        document: &Entity<DocumentState<&'static str>>,
        node: &'static str,
        cx: &mut VisualTestContext,
    ) -> Vec<String> {
        document.read_with(cx, |document, _| {
            document
                .layout_items
                .iter()
                .filter_map(|item| match item {
                    DocumentLayoutItem::Text {
                        display, node_id, ..
                    } if *node_id == Some(node) => {
                        Some(document.display_text()[display.clone()].to_owned())
                    }
                    _ => None,
                })
                .collect()
        })
    }

    #[gpui::test]
    fn rows_take_their_frames_fonts_and_declared_blocks_without_measuring(cx: &mut TestAppContext) {
        let paragraph = "words that wrap ".repeat(200);
        let (document, mut cx, rendered) = framed_document(&paragraph, cx);
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        let rows = line_rows(&document, "text", &mut cx);
        // The heading, then the paragraph wrapped within its 200px frame.
        assert_eq!(rows[0], "Heading");
        assert!(rows.len() > 60);
        assert_eq!(rows[1..].concat(), format!("- {paragraph}"));
        let first_rows = rows.len();
        // A frame lays out the rows in view, never every row to measure it.
        rendered.borrow_mut().clear();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let drawn = rendered.borrow().len();
        assert!(drawn < first_rows / 2, "{drawn} of {first_rows} rows drawn");

        // A narrower document wraps the same text into more rows.
        cx.simulate_resize(size(px(150.), px(400.)));
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        let narrow = line_rows(&document, "text", &mut cx);
        assert!(narrow.len() > first_rows);
        assert_eq!(narrow[1..].concat(), format!("- {paragraph}"));

        // Typing in the draft wraps its line within the same frame.
        document.update(&mut cx, |document, cx| {
            let end = document.region(&"draft").unwrap().range().end;
            document.set_selection(end..end, false, cx);
        });
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_selection(&" more draft words".repeat(10), window, cx)
            });
            let _ = window.draw(cx);
        });
        assert!(line_rows(&document, "draft", &mut cx).len() > 1);
        assert_declared_geometry(&document, &mut cx);

        // A host frame change reaches the node's rows at the next layout.
        document.update(&mut cx, |document, cx| {
            document.set_text_renderer(
                |_, text, _, _| div().pl(px(10.)).child(text).into_any_element(),
                cx,
            );
            document.set_text_frame_resolver(
                |_, _, _| {
                    TextFrame::new()
                        .with_space_after(px(5.))
                        .with_max_width(px(70.))
                },
                cx,
            );
        });
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        assert!(line_rows(&document, "text", &mut cx).len() > narrow.len());

        // Leaving text rows returns every item to measurement.
        document.update(&mut cx, |document, cx| {
            document.set_text_rows(None, cx);
            assert_eq!(document.layout_items, document.model.layout_items(None));
        });
        draw(&mut cx);

        // Rows that come back derive their geometry anew.
        document.update(&mut cx, |document, cx| {
            document.set_text_rows(Some(TextRows::new()), cx);
        });
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        assert!(line_rows(&document, "text", &mut cx).len() > narrow.len());
    }

    #[gpui::test]
    fn ascii_lines_short_of_their_width_wrap_whole(cx: &mut TestAppContext) {
        let mut wrappers = vec![
            cx.text_system()
                .line_wrapper(gpui::font("Zed Mono"), px(16.)),
        ];
        let mut buffer = [0; 4];
        let advance = (b' '..=b'~').fold(Pixels::ZERO, |widest, byte| {
            widest.max(wrappers[0].cluster_width(char::from(byte).encode_utf8(&mut buffer)))
        });
        let words = "fn wrap(x: u8) -> Vec<u32> { x.max(1) } // ~!@#$%^&*()_+ ";
        let (mut fit, mut wide) = (0, 0);
        for len in 1..words.len() * 3 {
            let line: String = words.chars().cycle().take(len).collect();
            for width in (20..=900).step_by(37).map(|width| px(width as f32)) {
                if fits_unwrapped(&line, advance, width) {
                    fit += 1;
                    assert_eq!(
                        wrap_line(&line, &[(line.len(), 0)], &mut wrappers, (width, width)),
                        Vec::<u32>::new(),
                        "{line:?} at {width:?}"
                    );
                } else {
                    wide += 1;
                }
            }
        }
        assert!(fit > 0 && wide > 0);
        assert!(!fits_unwrapped("tab\there", advance, px(900.)));
        assert!(!fits_unwrapped("中文", advance, px(900.)));
    }

    #[gpui::test]
    fn large_rewraps_land_in_the_background_with_consistent_heights(cx: &mut TestAppContext) {
        // Far more text than a frame rewraps, all of it out of view.
        let paragraph = (0..400)
            .map(|line| format!("{line} {}", "words that wrap ".repeat(12)))
            .collect::<Vec<_>>()
            .join("\n");
        let (document, mut cx, _) = framed_document(&paragraph, cx);
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        let original = line_rows(&document, "text", &mut cx);
        for (width, narrower) in [(150., true), (320., false), (160., true), (320., false)] {
            let before = line_rows(&document, "text", &mut cx).len();
            cx.simulate_resize(size(px(width), px(400.)));
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            document.read_with(&cx, |document, _| {
                assert!(document.text_rows.as_ref().unwrap().is_wrapping());
            });
            // In-frame reflow changes the indices of the lines still wrapping
            // in the background, in both directions.
            assert_declared_geometry(&document, &mut cx);
            let partial = line_rows(&document, "text", &mut cx).len();
            assert_ne!(partial, before);
            assert_eq!(partial > before, narrower);

            draw(&mut cx);
            draw(&mut cx);
            document.read_with(&cx, |document, _| {
                assert!(!document.text_rows.as_ref().unwrap().is_wrapping());
            });
            assert_declared_geometry(&document, &mut cx);
            let after = line_rows(&document, "text", &mut cx);
            assert_ne!(after.len(), partial);
            assert_eq!(after.len() > partial, narrower);
            if !narrower {
                assert_eq!(after, original);
            }
        }
    }
}
