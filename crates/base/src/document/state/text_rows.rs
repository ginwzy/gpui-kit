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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AbsoluteLength, App, AppContext as _, Bounds, Context, Font, LineWrapper, LineWrapperHandle,
    Pixels, Refineable as _, Task, TextStyleRefinement, Window, px,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{Derived, DocumentLayoutItem, DocumentState, DocumentTextContext};
use crate::document::DocumentRegion;

mod font_runs;
mod wrap;
use font_runs::{FontOverride, font_runs};
use wrap::{Breaks, Previous, Wrap};

/// A display row holds at most this many bytes. A row of zero-advance text
/// never reaches its width, so without this bound one row could hold a whole
/// line, and showing it would shape every character. Only a single grapheme
/// cluster may exceed the bound; GPUI shapes a bounded prefix of such a
/// cluster (`MAX_DISPLAY_CLUSTER_CHARS`). Text and positions are unaffected.
const MAX_ROW_BYTES: usize = 4096;

/// Lines never wrapped before all wrap within the frame when they total up to
/// this many bytes, so a text of this size shows its final rows and extent in
/// its first frame. Beyond it, only lines in view do.
const FIRST_WRAP_IN_FRAME_BYTES: usize = 256 * 1024;
/// Lines in view, and after an invalidation lines near it, wrap within the
/// frame up to this many bytes; the rest wrap in the background.
const REWRAP_IN_FRAME_BYTES: usize = 32 * 1024;
/// At most this many lines go to one background wrap. Preparing them reads
/// the model on the UI thread; the rest go to the next wrap once it lands.
const BACKGROUND_LINES_PER_WRAP: usize = 4096;
/// Frames the width must hold before a resize's whole pass runs.
const RESIZE_QUIET_FRAMES: u8 = 2;
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
    rem_size: Pixels,
    baseline: Pixels,
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
        presentation: Option<&super::DocumentTextPresentation>,
    ) -> Pixels {
        if self.hidden {
            return px(0.);
        }
        let mut height = self.inline_metrics(presentation).0;
        if row.start == line.start {
            height += self.space_before;
        }
        if row.end == line.end {
            height += self.space_after;
        }
        height
    }

    /// Height and baseline of this row, followed by its text's top inset.
    pub(in crate::document) fn inline_metrics(
        &self,
        presentation: Option<&super::DocumentTextPresentation>,
    ) -> (Pixels, Pixels, Pixels) {
        if presentation.is_none_or(|presentation| presentation.objects.is_empty()) {
            return (self.line_height, self.baseline, px(0.));
        }
        let mut above = self.baseline;
        let mut below = self.line_height - self.baseline;
        for (_, object) in presentation.into_iter().flat_map(|p| &p.objects) {
            let (size, baseline) = object.resolve(self.rem_size);
            above = above.max(baseline);
            below = below.max(size.height - baseline);
        }
        (above + below, above, above - self.baseline)
    }

    pub(in crate::document) fn line_height(&self) -> Pixels {
        self.line_height
    }

    pub(in crate::document) fn rem_size(&self) -> Pixels {
        self.rem_size
    }

    pub(in crate::document) fn inline_indent(
        &self,
        presentation: &super::DocumentTextPresentation,
    ) -> Pixels {
        if presentation
            .indent
            .as_ref()
            .is_some_and(|indent| !indent.hanging)
        {
            self.indent
        } else {
            px(0.)
        }
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
    previous: HashMap<usize, u64>,
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
    starts: Breaks,
    input: Option<Previous>,
}

impl RowCache {
    /// Where rows after the first start in the line keyed `key`, relative to
    /// the line, as last wrapped. Empty for a line never wrapped.
    pub(in crate::document) fn row_starts(&self, key: u64) -> Option<&Breaks> {
        self.lines.get(&key).map(|rows| &rows.starts)
    }

    /// Records the wrap of the line at display offset `line_start`. A
    /// one-row line only updates an entry it had.
    fn record(
        &mut self,
        line_start: usize,
        key: u64,
        wrap: WrapKey,
        starts: Breaks,
        input: Option<Previous>,
    ) {
        if starts.is_empty()
            && !self.lines.contains_key(&key)
            && input.as_ref().is_none_or(|input| input.range.len() <= 2048)
        {
            return;
        }
        if let Some(old) = self.previous.insert(line_start, key)
            && old != key
            && let Some(rows) = self.lines.get_mut(&old)
        {
            // Retain one derived input per line position, not every historical
            // 8 MiB snapshot after successive edits. The line wrapped here
            // before may still be shown elsewhere, so it keeps its rows.
            rows.input = None;
            if rows.starts.is_empty() {
                self.lines.remove(&old);
            }
        }
        self.lines.insert(
            key,
            CachedRows {
                generation: wrap.generation,
                widths: wrap.widths,
                starts,
                input,
            },
        );
    }
}

/// Identifies a line's text and every style that changes its advances, from
/// the item for the whole line.
pub(in crate::document) fn line_key<I>(item: &DocumentLayoutItem<I>) -> u64 {
    let DocumentLayoutItem::Text {
        fingerprint,
        display,
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
    let mut styled = false;
    if let Some(style) = font_refinement(presentation) {
        0u8.hash(&mut hasher);
        hash_font_refinement(style, &mut hasher);
        styled = true;
    }
    if let Some(indent) = &presentation.indent {
        1u8.hash(&mut hasher);
        indent.marker.hash(&mut hasher);
        indent.hanging.hash(&mut hasher);
        styled = true;
    }
    for (range, font) in font_runs(presentation, display.len()) {
        if font != FontOverride::default() {
            2u8.hash(&mut hasher);
            range.hash(&mut hasher);
            font.hash(&mut hasher);
            styled = true;
        }
    }
    for (range, size) in &presentation.objects {
        3u8.hash(&mut hasher);
        range.hash(&mut hasher);
        size.is_width_constrained().hash(&mut hasher);
        for length in [size.width(), size.height(), size.baseline()] {
            match length {
                AbsoluteLength::Pixels(value) => (0u8, f32::from(value).to_bits()),
                AbsoluteLength::Rems(value) => (1u8, value.0.to_bits()),
            }
            .hash(&mut hasher);
        }
        styled = true;
    }
    // A paint-only presentation (including a default/colored paragraph) has
    // exactly the same font inputs as no presentation at all.
    if styled {
        hasher.finish()
    } else {
        *fingerprint
    }
}

fn font_refinement(presentation: &super::DocumentTextPresentation) -> Option<&TextStyleRefinement> {
    presentation.text_style.as_ref().filter(|style| {
        style.font_family.is_some()
            || style.font_features.is_some()
            || style.font_fallbacks.is_some()
            || style.font_size.is_some()
            || style.font_weight.is_some()
            || style.font_style.is_some()
    })
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

pub(in crate::document) struct TextRowsState<I> {
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
    request: u64,
    cancel: Arc<AtomicBool>,
    publishing: Option<RowPublication<I>>,
    publication_task: Option<Task<()>>,
    retired: Vec<(super::LayoutItems<I>, gpui::PreparedListItems)>,
    invalid_frames: std::collections::HashSet<(Option<I>, Range<usize>)>,
    frames: HashMap<(Option<I>, Range<usize>), RowFrame>,
    previous_styles: HashMap<u64, RowsStyle>,
    edit: Option<LineEdit>,
    /// Items replaced since the last layout. When no whole-document pass is
    /// due, only these resolve their frames and check their wraps.
    dirty: Option<Range<usize>>,
    /// Geometry was just invalidated: the next whole pass may also rewrap
    /// lines beyond the view within the frame budget. Later passes, which
    /// pick up background results, leave lines out of view to the background.
    nearby: bool,
    /// A drag's width changes await their whole pass, which runs once the
    /// width has held for `RESIZE_QUIET_FRAMES` frames.
    resize_pending: bool,
    /// Frames laid out since the width last changed.
    quiet_frames: u8,
}

/// Unchanged extents relative to one cached line, composed directly from the
/// existing transaction stream. This is a pending layout hint, not history.
pub(super) struct LineEdit {
    line_start: usize,
    key: u64,
    prefix: usize,
    suffix: usize,
    current_len: usize,
}

struct WrapResult {
    generation: u64,
    request: u64,
    lines: std::collections::VecDeque<WrappedLine>,
}

impl<I> Drop for TextRowsState<I> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A line's wrap, identified by its display range and text/font key. The old
/// first-row index is only a lookup hint: another line's reflow can move it.
struct WrappedLine {
    item: usize,
    range: Range<usize>,
    key: u64,
    widths: (Pixels, Pixels),
    starts: Breaks,
    input: Previous,
}

impl<I: Clone> TextRowsState<I> {
    fn invalidate(&mut self) {
        self.nearby = true;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.request += 1;
        // Keep the one worker until it acknowledges cancellation, so each
        // resize cannot start another simultaneous wrap.
        self.ready = None;
        self.publication_task = None;
        if let Some(work) = self.publishing.take() {
            self.retired.push((work.items, work.heights));
        }
        self.check = true;
        self.scan_all = true;
    }
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
            request: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            publishing: None,
            publication_task: None,
            retired: Vec::new(),
            invalid_frames: std::collections::HashSet::new(),
            frames: HashMap::new(),
            previous_styles: HashMap::new(),
            edit: None,
            dirty: None,
            nearby: true,
            resize_pending: false,
            quiet_frames: u8::MAX,
        }
    }

    /// Items `old` were replaced by `len` items outside a publication's own
    /// attachment. Work that addresses items by index follows the change:
    /// queued lines the replacement touched are dropped and later ones move.
    /// A partly built line at or after the change restarts, since its rows
    /// were read with offsets from before it.
    fn items_spliced(&mut self, old: &Range<usize>, len: usize) {
        let moved = |ix: usize| {
            if ix <= old.start {
                ix
            } else if ix >= old.end {
                ix - old.len() + len
            } else {
                old.start + len
            }
        };
        if let Some(dirty) = &mut self.dirty {
            *dirty = moved(dirty.start)..moved(dirty.end);
        }
        if let Some(work) = &mut self.publishing {
            let restart = work.row > 0
                && work
                    .lines
                    .front()
                    .is_some_and(|(rows, _, _)| rows.end > old.start);
            work.lines.retain_mut(|(rows, _, _)| {
                if rows.end <= old.start {
                    true
                } else if rows.start >= old.end {
                    *rows = moved(rows.start)..moved(rows.end);
                    true
                } else {
                    false
                }
            });
            if restart {
                self.retired.push((
                    std::mem::take(&mut work.items),
                    std::mem::replace(&mut work.heights, gpui::PreparedListItems::new()),
                ));
                work.row = 0;
            }
        }
    }

    /// Whether row geometry is still settling: items wait for layout to frame
    /// or wrap them, or lines out of view wait for a background wrap.
    pub(in crate::document) fn is_wrapping(&self) -> bool {
        self.resolve
            || self.check
            || self.dirty.is_some()
            || self.pending.is_some()
            || self.ready.is_some()
            || self.publishing.is_some()
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
    previous: Option<Previous>,
    fonts_key: u64,
    priority: bool,
    objects: Vec<(Range<usize>, Pixels)>,
}

/// The fonts lines wrap in. The base style's font is the first.
struct FontTable {
    base_style: gpui::TextStyle,
    rem_size: Pixels,
    fonts: Vec<(Font, Pixels)>,
    /// Each font's hash with its size, so keying a line's fonts does not
    /// hash every font description again.
    hashes: Vec<u64>,
}

impl FontTable {
    fn new(window: &Window) -> Self {
        let base_style = window.text_style();
        let rem_size = window.rem_size();
        let base = (base_style.font(), base_style.font_size.to_pixels(rem_size));
        Self {
            base_style,
            rem_size,
            hashes: vec![Self::hash(&base)],
            fonts: vec![base],
        }
    }

    fn hash((font, size): &(Font, Pixels)) -> u64 {
        let mut hash = DefaultHasher::new();
        font.hash(&mut hash);
        f32::from(*size).to_bits().hash(&mut hash);
        hash.finish()
    }

    fn index(&mut self, font: Font, size: Pixels) -> usize {
        if let Some(ix) = self
            .fonts
            .iter()
            .position(|(known, known_size)| *known == font && *known_size == size)
        {
            return ix;
        }
        let entry = (font, size);
        self.hashes.push(Self::hash(&entry));
        self.fonts.push(entry);
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
type OutdatedLine = (Range<usize>, WrapKey, Breaks);

/// A candidate tree is built in bounded batches while the published tree
/// remains usable. Each line is attached atomically, never one row at a time.
struct RowPublication<I> {
    lines: std::collections::VecDeque<OutdatedLine>,
    items: super::LayoutItems<I>,
    heights: gpui::PreparedListItems,
    row: usize,
}

/// A line whose rows lag behind the wrap its width and fonts need.
struct StaleLine {
    items: Range<usize>,
    /// The display line, so prioritizing needs no item lookups.
    line: Range<usize>,
    bytes: usize,
    wrapped_before: bool,
    in_view: bool,
    focused: bool,
    widths: (Pixels, Pixels),
}

impl LineSpec {
    /// Repeated lines in one synchronous batch share their completed wrap.
    /// The retained row cache is installed only after the whole batch returns.
    fn wrap_batch(
        specs: &[Self],
        text: Arc<str>,
        wrappers: &mut [LineWrapperHandle],
    ) -> Vec<WrappedLine> {
        let mut completed: HashMap<u64, usize> = HashMap::new();
        let mut lines: Vec<WrappedLine> = Vec::with_capacity(specs.len());
        for spec in specs {
            let prior = completed
                .get(&spec.key)
                .map(|ix| &lines[*ix])
                .filter(|line| {
                    line.widths == spec.widths && line.input.fonts_key == spec.fonts_key
                });
            let line = if let Some(prior) = prior {
                WrappedLine {
                    item: spec.item,
                    range: spec.range.clone(),
                    key: spec.key,
                    widths: spec.widths,
                    starts: prior.starts.clone(),
                    input: prior.input.clone(),
                }
            } else {
                spec.wrap(text.clone(), wrappers)
            };
            completed.insert(spec.key, lines.len());
            lines.push(line);
        }
        lines
    }

    fn wrap(&self, text: Arc<str>, wrappers: &mut [LineWrapperHandle]) -> WrappedLine {
        let mut work = Wrap::new(
            &text[self.range.clone()],
            &self.runs,
            self.widths,
            self.previous.clone(),
        )
        .with_objects(self.objects.clone());
        let cancel = AtomicBool::new(false);
        while !work.step(
            &text[self.range.clone()],
            &self.runs,
            wrappers,
            REWRAP_IN_FRAME_BYTES,
            &cancel,
        ) {}
        self.result(text, work.finish())
    }

    fn result(&self, text: Arc<str>, starts: Breaks) -> WrappedLine {
        // A cached line must not pin the whole document revision. A single
        // huge source line can share the input directly; other lines retain
        // only their own immutable text.
        let text: Arc<str> = if self.range == (0..text.len()) {
            text
        } else {
            text[self.range.clone()].into()
        };
        let range = 0..text.len();
        WrappedLine {
            item: self.item,
            range: self.range.clone(),
            key: self.key,
            widths: self.widths,
            input: Previous {
                text,
                range,
                breaks: starts.clone(),
                runs: self.runs.clone().into(),
                widths: self.widths,
                fonts_key: self.fonts_key,
                change: None,
            },
            starts,
        }
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// With text rows, replaced items are dropped a bounded batch per later
    /// frame, so a large tree does not drop in one frame.
    pub(super) fn retire_layout_items(&mut self, items: super::LayoutItems<I>) {
        if let Some(state) = self.text_rows.as_mut() {
            state.retired.push((items, gpui::PreparedListItems::new()));
        }
    }
    /// Whether exact text-row geometry is still being prepared. The current
    /// geometry remains internally consistent, but may be a bounded initial
    /// placeholder or the preceding complete wrap.
    pub fn is_laying_out_text(&self) -> bool {
        self.text_rows
            .as_ref()
            .is_some_and(|rows| rows.is_wrapping())
    }

    pub(super) fn retained_row_style(
        &self,
        wrap: Option<WrapKey>,
        presentation: &mut super::DocumentTextPresentation,
    ) {
        let Some(state) = &self.text_rows else {
            return;
        };
        let Some(style) = wrap.and_then(|wrap| state.previous_styles.get(&wrap.generation)) else {
            return;
        };
        let mut retained = TextStyleRefinement {
            font_family: Some(style.font.family.clone()),
            font_features: Some(style.font.features.clone()),
            font_fallbacks: style.font.fallbacks.clone(),
            font_weight: Some(style.font.weight),
            font_style: Some(style.font.style),
            font_size: Some(style.font_size.into()),
            line_height: Some(style.line_height.into()),
            ..Default::default()
        };
        if let Some(original) = &presentation.text_style {
            retained.refine(original);
            if let Some(size) = original.font_size {
                retained.font_size = Some(size.to_pixels(style.rem_size).into());
            }
            if let Some(height) = original.line_height {
                retained.line_height = Some(
                    height
                        .to_pixels(retained.font_size.unwrap(), style.rem_size)
                        .into(),
                );
            }
        }
        presentation.text_style = Some(retained);
    }

    /// A new line has no exact row geometry yet. Paint a bounded prefix, with
    /// its real source offsets, until its complete wrap is published. Finding
    /// the prefix itself must not traverse an unbounded grapheme cluster.
    pub(super) fn pending_row_paint_range(
        &self,
        display: Range<usize>,
        wrapped: Option<WrapKey>,
    ) -> Range<usize> {
        const PREFIX_BYTES: usize = 2048;
        if self.text_rows.is_none() || wrapped.is_some() || display.len() <= PREFIX_BYTES {
            return display;
        }
        let text = &self.model.display_text()[display.clone()];
        let end = text.floor_char_boundary(PREFIX_BYTES);
        let end = text[..end]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(ix, _)| ix);
        display.start..display.start + end
    }

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
        assert!(
            rows.is_some()
                || self
                    .model
                    .blocks
                    .iter()
                    .all(|block| block.inline_size().is_none()),
            "remove inline objects before disabling text rows"
        );
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
                self.layout_items
                    .map_range(0..self.layout_items.len(), |item| {
                        item.forget_geometry();
                    });
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
    /// Asks the host again for the frames of rows `items`, keeping their slots.
    pub(super) fn reframe_rows(&mut self, items: &[usize]) {
        if self.text_rows.is_none() {
            for &ix in items {
                self.list_state.remeasure_items(ix..ix + 1);
            }
            return;
        }
        let lines: Vec<_> = items
            .iter()
            .filter_map(|&ix| match &self.layout_items[ix] {
                DocumentLayoutItem::Text { node_id, line, .. } => {
                    Some((node_id.clone(), line.clone()))
                }
                _ => None,
            })
            .collect();
        self.text_rows
            .as_mut()
            .unwrap()
            .invalid_frames
            .extend(lines);
    }

    pub(super) fn clear_row_geometry(&mut self, owned: impl Fn(Option<&I>) -> bool) -> bool {
        let Some(state) = self.text_rows.as_mut() else {
            return false;
        };
        state.invalidate();
        let mut cleared = false;
        let mut ix = 0;
        while ix < self.layout_items.len() {
            match &self.layout_items[ix] {
                DocumentLayoutItem::Text { node_id, line, .. } => {
                    if owned(node_id.as_ref()) {
                        self.text_rows
                            .as_mut()
                            .unwrap()
                            .invalid_frames
                            .insert((node_id.clone(), line.clone()));
                        cleared = true;
                    }
                    ix = self.line_rows(ix).end;
                }
                DocumentLayoutItem::Block { id, .. }
                | DocumentLayoutItem::AnchoredBlock { id, .. }
                    if owned(Some(id)) =>
                {
                    self.layout_items.update(ix, |item| match item {
                        DocumentLayoutItem::Block { height, .. }
                        | DocumentLayoutItem::AnchoredBlock { height, .. } => {
                            height.0 = BlockGeometry::Unresolved
                        }
                        _ => unreachable!(),
                    });
                    cleared = true;
                    ix += 1;
                }
                _ => {
                    ix += 1;
                }
            }
        }
        let state = self.text_rows.as_mut().unwrap();
        state.resolve |= cleared;
        state.check |= cleared;
        state.scan_all |= cleared;
        cleared
    }

    /// Layout items were added or replaced; layout resolves and wraps them.
    pub(super) fn layout_items_changed(&mut self) {
        if let Some(state) = self.text_rows.as_mut() {
            state.invalidate();
            state.edit = None;
            state.resolve = true;
            state.check = true;
            state.scan_all = true;
        }
    }

    /// Items `old` were replaced by `len` items, and later items moved with
    /// them. Only the replacement resolves and wraps; work in flight goes on.
    /// Background wraps are matched to lines by display range and text key,
    /// so results for lines the change moved are dropped and checked again.
    pub(super) fn layout_items_replaced(&mut self, old: Range<usize>, len: usize) {
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        state.items_spliced(&old, len);
        state.edit = None;
        let new = old.start..old.start + len;
        state.dirty = Some(match state.dirty.take() {
            Some(dirty) => dirty.start.min(new.start)..dirty.end.max(new.end),
            None => new,
        });
    }

    pub(super) fn text_row_edit(&self, transaction: &super::EditTransaction) -> Option<LineEdit> {
        let [edit] = transaction.edits() else {
            return None;
        };
        if edit.replacement().contains('\n') {
            return None;
        }
        let state = self.text_rows.as_ref()?;
        let source = edit.range();
        let start = self
            .model
            .source_to_display(source.start, super::Affinity::After)?;
        let end = self
            .model
            .source_to_display(source.end, super::Affinity::Before)?;
        let DocumentLayoutItem::Text { line, line_key, .. } = self
            .layout_items
            .get(self.layout_item_for_source(source.start))?
        else {
            return None;
        };
        if start < line.start || end > line.end || start > end {
            return None;
        }
        let key = *state.cache.previous.get(&line.start)?;
        let cached = state.cache.lines.get(&key)?.input.as_ref()?;
        let mut prefix = start - line.start;
        let mut suffix = line.end - end;
        if let Some(previous) = state.edit.as_ref().filter(|edit| {
            edit.key == key && edit.line_start == line.start && edit.current_len == line.len()
        }) {
            prefix = prefix.min(previous.prefix);
            suffix = suffix.min(previous.suffix);
        } else if key != *line_key || cached.range.len() != line.len() {
            return None;
        }
        Some(LineEdit {
            line_start: line.start,
            key,
            prefix,
            suffix,
            current_len: line.len() - (end - start) + edit.replacement().len(),
        })
    }

    pub(super) fn set_text_row_edit(&mut self, edit: Option<LineEdit>) {
        if let Some(state) = self.text_rows.as_mut() {
            state.edit = edit;
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
            if let Some(old_style) = state.style.replace(style) {
                state.previous_styles.insert(state.generation, old_style);
                state.generation += 1;
                self.clear_row_geometry(|_| true);
            }
        }
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        let resized = state.width != Some(bounds.size.width);
        if resized {
            state.invalidate();
            state.width = Some(bounds.size.width);
            state.check = true;
            state.scan_all = true;
        }
        // While the width keeps changing, only the view is checked: the rest
        // keeps its complete earlier wrap. A width change soon after another
        // is a drag; its whole pass and background wrap follow once the width
        // has held for a few frames, even when other redraws interleave. A
        // lone change takes its whole pass at once.
        if resized {
            state.resize_pending |= state.quiet_frames < RESIZE_QUIET_FRAMES;
            state.quiet_frames = 0;
        } else {
            state.quiet_frames = state.quiet_frames.saturating_add(1);
        }
        let defer = state.resize_pending && state.quiet_frames < RESIZE_QUIET_FRAMES;
        if defer {
            cx.notify();
        } else {
            state.resize_pending = false;
        }
        // Replaced items resolve now, or with a whole-document pass.
        if state.resolve {
            state.resolve = false;
            let (_, whole) = self.resolve_row_geometry(0..usize::MAX, window, cx);
            // Every line just took its frame and wrap: a whole wrap check
            // would only confirm it.
            if whole
                && let Some(state) = self.text_rows.as_mut()
                && state.scan_all
                && !state.resize_pending
                && state.pending.is_none()
                && state.ready.is_none()
                && state.publishing.is_none()
            {
                state.scan_all = false;
                state.check = false;
                state.dirty = None;
            }
        } else if let Some(dirty) = state.dirty.clone() {
            self.resolve_row_geometry(dirty, window, cx);
        }
        // Publication that starts this frame publishes its first batch at once.
        let publishing = self
            .text_rows
            .as_ref()
            .is_some_and(|state| state.publishing.is_some());
        self.consume_ready_rows(cx);
        let state = self.text_rows.as_mut().expect("text rows are on");
        // A whole pass also checks replaced items. Replaced items are checked
        // while a publication is under way; lines in view wait for it.
        let dirty = state.dirty.take().filter(|_| !state.scan_all);
        let idle = state.publishing.is_none();
        if (idle && state.check) || dirty.is_some() {
            self.wrap_stale_lines(bounds, dirty, idle, defer, window, cx);
        }
        if publishing
            && self
                .text_rows
                .as_ref()
                .is_some_and(|state| state.publishing.is_some())
        {
            self.advance_row_publication(cx);
        }
        self.retire_row_geometry(cx);
    }

    fn consume_ready_rows(&mut self, cx: &mut Context<Self>) {
        if self
            .text_rows
            .as_ref()
            .is_some_and(|state| state.publishing.is_none())
            && let Some(state) = self.text_rows.as_mut()
            && let Some(result) = state.ready.as_mut()
        {
            state.check = true;
            state.scan_all = state.pending.is_none();
            if result.generation == state.generation && result.request == state.request {
                let generation = result.generation;
                let lines = result.lines.drain(..result.lines.len().min(64)).collect();
                if result.lines.is_empty() {
                    state.ready = None;
                }
                let (settled, outdated) = self.take_wrapped(generation, lines);
                self.settle_line_rows(settled, outdated, cx);
            } else {
                state.ready = None;
            }
        }
    }

    fn retire_row_geometry(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.text_rows.as_mut() {
            let mut budget = 512;
            while let Some((items, heights)) = state.retired.last_mut() {
                if budget == 0 {
                    break;
                }
                let count = items.len().max(heights.len()).min(budget);
                items.retire(count);
                heights.retire(count);
                budget -= count;
                if items.len() == 0 && heights.is_empty() {
                    state.retired.pop();
                }
            }
            if !state.retired.is_empty() {
                cx.notify();
            }
        }
    }

    /// Resolves the frame of every line and the height of every block in
    /// `items` that lacks one, and declares their heights to the list.
    /// Returns where `items` ends once changed lines collapse to placeholders,
    /// and whether every line in it is now a single row its frame shows whole
    /// with its wrap recorded, so no line there needs a wrap check.
    fn resolve_row_geometry(
        &mut self,
        items: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (usize, bool) {
        /// Adjacent single rows taking the same frame and wrap: their first
        /// item and count. Uniform text needs a handful, not one per line.
        struct Singles {
            start: usize,
            count: usize,
            frame: RowFrame,
            wrap: Option<WrapKey>,
        }
        let ascii_advance = self.ascii_advance(window, cx);
        let base_style = window.text_style();
        let len = self.layout_items.len();
        let end = items.end.min(len);
        let start = if items.start < end {
            self.line_rows(items.start).start
        } else {
            end
        };
        let (start, plain_whole) =
            self.resolve_plain_lines(start..end, &base_style, ascii_advance, window, cx);
        let state = self.text_rows.as_ref().unwrap();
        let (generation, invalid, width) = (state.generation, &state.invalid_frames, state.width);
        let keyed = !invalid.is_empty() || !state.frames.is_empty();
        // Without a resolver, every unstyled line has the same frame.
        let mut plain_frame = None;
        let mut singles: Vec<Singles> = Vec::new();
        // Lines whose rows collapse to one pending row, and frame map keys.
        let mut collapses = Vec::new();
        let mut keys = Vec::new();
        let mut whole = true;
        let mut blocks = Vec::new();
        let mut trailer = None;
        // Read every item once, in order; changes apply afterwards.
        let mut ix = start;
        let mut iter = self.layout_items.iter_from(start).peekable();
        while ix < end {
            let Some(item) = iter.next() else {
                break;
            };
            match item {
                DocumentLayoutItem::Text {
                    line,
                    node_id,
                    frame,
                    wrapped,
                    presentation,
                    ..
                } => {
                    let mut last = item;
                    let mut count = 1;
                    while let Some(next) = iter.next_if(|next| {
                        matches!(next, DocumentLayoutItem::Text { line: other, node_id: other_node, .. }
                            if other == line && other_node == node_id)
                    }) {
                        last = next;
                        count += 1;
                    }
                    let rows = ix..ix + count;
                    ix = rows.end;
                    if frame.0.is_some()
                        && (invalid.is_empty()
                            || !invalid.contains(&(node_id.clone(), line.clone())))
                    {
                        whole = false;
                        continue;
                    }
                    // The first row without a frame frames its whole line.
                    let plain = self.text_frame_resolver.is_none() && presentation.is_none();
                    let resolved = match plain_frame {
                        Some(frame) if plain => frame,
                        _ => {
                            let frame =
                                self.resolve_line_frame(item, last, &base_style, window, cx);
                            if plain {
                                plain_frame = Some(frame);
                            }
                            frame
                        }
                    };
                    let keep_old = wrapped.0.is_some_and(|wrap| wrap.generation != generation);
                    // Most lines are in neither map; skip building their key.
                    if keep_old || keyed {
                        keys.push(((node_id.clone(), line.clone()), resolved, keep_old));
                    }
                    if !keep_old && count > 1 && frame.0 != Some(resolved) {
                        // The host changed the frame, and its renderer cannot
                        // reproduce the old one: one pending row until reflow.
                        collapses.push((rows, resolved));
                        whole = false;
                        continue;
                    }
                    // Existing complete rows keep their old geometry until
                    // the candidate is published.
                    if count > 1 || keep_old && wrapped.0.is_some() {
                        whole = false;
                        continue;
                    }
                    // As the wrap check decides it, without a second pass.
                    let wrap = width.filter(|_| !keep_old).and_then(|width| {
                        let widths = resolved.widths(width);
                        let whole = resolved.hidden
                            || line.is_empty()
                            || presentation.is_none()
                                && ascii_advance.is_some_and(|advance| {
                                    fits_unwrapped(
                                        &self.model.display_text()[line.clone()],
                                        advance,
                                        widths.0,
                                    )
                                });
                        whole.then_some(WrapKey { generation, widths })
                    });
                    whole &= wrap.is_some();
                    match singles.last_mut() {
                        Some(last)
                            if last.start + last.count == rows.start
                                && last.frame == resolved
                                && last.wrap == wrap =>
                        {
                            last.count += 1;
                        }
                        _ => singles.push(Singles {
                            start: rows.start,
                            count: 1,
                            frame: resolved,
                            wrap,
                        }),
                    }
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
                        blocks.push((ix, geometry));
                    }
                    ix += 1;
                }
                DocumentLayoutItem::Trailer => {
                    trailer = Some(ix);
                    ix += 1;
                }
            }
        }
        drop(iter);

        let state = self.text_rows.as_mut().unwrap();
        for (key, frame, keep_old) in keys {
            state.invalid_frames.remove(&key);
            if keep_old {
                state.frames.insert(key, frame);
            } else {
                state.frames.remove(&key);
            }
        }
        // Collapse changed lines from the end, so earlier indices stand; later
        // indices move back by the rows each collapse removed before them.
        let mut collapsed: Vec<(usize, usize)> = Vec::new();
        for (rows, resolved) in collapses.into_iter().rev() {
            let (
                DocumentLayoutItem::Text {
                    source: first,
                    line: display_line,
                    line_key,
                    ..
                },
                DocumentLayoutItem::Text { source: last, .. },
            ) = (
                &self.layout_items[rows.start],
                &self.layout_items[rows.end - 1],
            )
            else {
                unreachable!("line rows are text items")
            };
            let display_line = display_line.clone();
            let mut item = self.model.text_layout_item(
                self.model.display_text(),
                display_line.clone(),
                first.start..last.end,
                display_line.clone(),
                *line_key,
            );
            if let DocumentLayoutItem::Text { frame, .. } = &mut item {
                frame.0 = Some(resolved);
            }
            let mut heights = gpui::PreparedListItems::new();
            heights.extend([Some(resolved.row_height(
                &display_line,
                &display_line,
                item.presentation(),
            ))]);
            self.before_list_splice();
            let old = self
                .layout_items
                .splice_prepared(rows.clone(), vec![item].into());
            let old_heights = self.list_state.splice_prepared(rows.clone(), heights);
            self.after_list_splice();
            let state = self.text_rows.as_mut().unwrap();
            state.retired.push((old, old_heights));
            state.items_spliced(&rows, 1);
            collapsed.push((rows.end, rows.len() - 1));
        }
        collapsed.reverse();
        let mut removed = 0;
        let collapsed: Vec<(usize, usize)> = collapsed
            .into_iter()
            .map(|(end, rows)| {
                removed += rows;
                (end, removed)
            })
            .collect();
        let moved = |ix: usize| {
            ix - collapsed
                .partition_point(|(end, _)| *end <= ix)
                .checked_sub(1)
                .map_or(0, |at| collapsed[at].1)
        };

        // Adjacent single rows take their frames and declare their heights in
        // one pass, and one list update, per run.
        let mut singles = singles.into_iter().peekable();
        while let Some(first) = singles.next() {
            let start = moved(first.start);
            let mut len = first.count;
            let mut run = vec![first];
            while let Some(next) = singles.next_if(|next| moved(next.start) == start + len) {
                len += next.count;
                run.push(next);
            }
            let mut heights = Vec::with_capacity(len);
            let mut segments = run.iter();
            let mut current = segments.next().map(|segment| (segment, segment.count));
            self.layout_items.map_range(start..start + len, |item| {
                let Some((segment, left)) = &mut current else {
                    return;
                };
                if let DocumentLayoutItem::Text {
                    display,
                    line,
                    frame,
                    wrapped,
                    presentation,
                    ..
                } = item
                {
                    frame.0 = Some(segment.frame);
                    if segment.wrap.is_some() {
                        wrapped.0 = segment.wrap;
                    }
                    heights.push(Some(segment.frame.row_height(
                        display,
                        line,
                        presentation.as_deref(),
                    )));
                }
                *left -= 1;
                if *left == 0 {
                    current = segments.next().map(|segment| (segment, segment.count));
                }
            });
            self.list_state.set_item_heights(start, heights);
        }
        for (ix, geometry) in blocks {
            let at = moved(ix);
            self.layout_items.update(at, |item| {
                if let DocumentLayoutItem::Block { height, .. }
                | DocumentLayoutItem::AnchoredBlock { height, .. } = item
                {
                    height.0 = geometry;
                }
            });
            self.list_state.set_item_heights(at, [geometry.height()]);
        }
        if let Some(ix) = trailer {
            self.list_state
                .set_item_heights(moved(ix), [Some(self.trailer_item_height(window))]);
        }
        (moved(end), whole && plain_whole)
    }

    /// [`Self::resolve_row_geometry`] for the lines a document opens with:
    /// unstyled, unframed and one row each, without a frame resolver. They
    /// share one frame, so each is framed, keyed and declared in one pass over
    /// the items rather than a read and then a write. Stops at the first other
    /// item, which starts a line; returns its index and whether every framed
    /// line also recorded its wrap.
    fn resolve_plain_lines(
        &mut self,
        items: Range<usize>,
        base_style: &gpui::TextStyle,
        ascii_advance: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (usize, bool) {
        fn is_plain<I>(item: &DocumentLayoutItem<I>) -> bool {
            matches!(
                item,
                DocumentLayoutItem::Text { display, line, frame, wrapped, presentation, .. }
                    if display == line
                        && frame.0.is_none()
                        && wrapped.0.is_none()
                        && presentation.is_none()
            )
        }
        let state = self.text_rows.as_ref().unwrap();
        if items.is_empty()
            || self.text_frame_resolver.is_some()
            || !state.invalid_frames.is_empty()
            || !state.frames.is_empty()
        {
            return (items.start, true);
        }
        let (generation, width) = (state.generation, state.width);
        let Some(first) = self
            .layout_items
            .get(items.start)
            .filter(|item| is_plain(item))
            .cloned()
        else {
            return (items.start, true);
        };
        let frame = self.resolve_line_frame(&first, &first, base_style, window, cx);
        let widths = width.map(|width| frame.widths(width));
        let display_text = self.model.display_text();
        let mut heights = Vec::new();
        let mut whole = true;
        let mut stopped = false;
        self.layout_items.map_range(items.clone(), |item| {
            if stopped || !is_plain(item) {
                stopped = true;
                return;
            }
            let DocumentLayoutItem::Text {
                display,
                line,
                frame: line_frame,
                wrapped,
                ..
            } = item
            else {
                unreachable!("plain items are text");
            };
            let (display, line) = (&*display, &*line);
            line_frame.0 = Some(frame);
            // As the wrap check decides it, without a second pass.
            wrapped.0 = widths
                .filter(|widths| {
                    frame.hidden
                        || line.is_empty()
                        || ascii_advance.is_some_and(|advance| {
                            fits_unwrapped(&display_text[line.clone()], advance, widths.0)
                        })
                })
                .map(|widths| WrapKey { generation, widths });
            whole &= wrapped.0.is_some();
            heights.push(Some(frame.row_height(display, line, None)));
        });
        let framed = heights.len();
        if framed > 0 {
            self.list_state.set_item_heights(items.start, heights);
        }
        (items.start + framed, whole)
    }

    pub(super) fn trailer_item_height(&self, window: &Window) -> Pixels {
        self.trailer_height
            .unwrap_or_else(|| window.viewport_size().height)
    }

    /// The text items of the display line that contains text item `ix`.
    pub(super) fn line_rows(&self, ix: usize) -> Range<usize> {
        let DocumentLayoutItem::Text {
            display,
            line,
            node_id,
            ..
        } = &self.layout_items[ix]
        else {
            return ix..ix + 1;
        };
        if display == line {
            return ix..ix + 1;
        }
        let same_line = |ix: usize| {
            matches!(&self.layout_items[ix], DocumentLayoutItem::Text { line: other, node_id: other_node, .. }
                if other == line && other_node == node_id)
        };
        // A line's rows are adjacent. Gallop out from `ix`, then bisect, so
        // the cost follows the line's row count rather than the document's.
        let start = if display.start == line.start {
            ix
        } else {
            let (mut inside, mut step) = (ix, 1);
            let mut lo = 0;
            while let Some(probe) = inside.checked_sub(step) {
                if !same_line(probe) {
                    lo = probe + 1;
                    break;
                }
                inside = probe;
                step *= 2;
            }
            let mut hi = inside;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if same_line(mid) {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            lo
        };
        let end = if display.end == line.end {
            ix + 1
        } else {
            let len = self.layout_items.len();
            let (mut inside, mut step) = (ix, 1);
            let mut hi = len;
            while inside + step < len {
                let probe = inside + step;
                if !same_line(probe) {
                    hi = probe;
                    break;
                }
                inside = probe;
                step *= 2;
            }
            let mut lo = inside + 1;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if same_line(mid) {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        start..end
    }

    fn resolve_line_frame(
        &self,
        first: &DocumentLayoutItem<I>,
        last: &DocumentLayoutItem<I>,
        base_style: &gpui::TextStyle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> RowFrame {
        let (
            DocumentLayoutItem::Text {
                source: first_source,
                node_id,
                presentation,
                line,
                ..
            },
            DocumentLayoutItem::Text {
                source: last_source,
                ..
            },
        ) = (first, last)
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
        // Only a frame's font size and line height count; the full style is
        // built only to measure an indent marker.
        let font_size = refinement
            .and_then(|refinement| refinement.font_size)
            .unwrap_or(base_style.font_size)
            .to_pixels(rem_size);
        let line_height = refinement
            .and_then(|refinement| refinement.line_height)
            .unwrap_or(base_style.line_height);
        let (indent, hanging) = presentation
            .as_ref()
            .and_then(|presentation| presentation.indent.as_ref())
            .map_or((px(0.), true), |indent| {
                let mut style = base_style.clone();
                if let Some(refinement) = refinement {
                    style.refine(refinement);
                }
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
        let line_height = window.pixel_snap(line_height.to_pixels(font_size.into(), rem_size));
        let baseline = if self.model.inline_objects_in(line.clone()).is_empty() {
            line_height
        } else {
            let mut style = base_style.clone();
            if let Some(refinement) = refinement {
                style.refine(refinement);
            }
            let font = window.text_system().resolve_font(&style.font());
            window
                .text_system()
                .baseline_offset(font, font_size, line_height)
        };
        RowFrame {
            space_before: frame.space_before.to_pixels(rem_size),
            space_after: frame.space_after.to_pixels(rem_size),
            inset_start: frame.inset_start.to_pixels(rem_size),
            inset_end: frame.inset_end.to_pixels(rem_size),
            max_width: frame.max_width.map(|width| width.to_pixels(rem_size)),
            hidden: frame.hidden,
            // The line height a text element lays out with, snapped to device pixels.
            line_height,
            rem_size,
            baseline,
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
                presentation,
                ..
            } => frame.0.map_or(px(0.), |frame| {
                frame.row_height(display, line, presentation.as_deref())
            }),
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
        dirty: Option<Range<usize>>,
        idle: bool,
        defer: bool,
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
        // have become stale; its result brings back a full check. Replaced
        // items are checked wherever they are.
        let scan_all = state.scan_all && !defer;
        let nearby = state.nearby;
        let len = self.layout_items.len();
        let line_start = |ix: usize| {
            if ix < len {
                self.line_rows(ix).start
            } else {
                ix
            }
        };
        let mut scopes = Vec::new();
        if scan_all {
            scopes.push(0..len);
        } else {
            if let Some(dirty) = &dirty {
                scopes.push(line_start(dirty.start)..dirty.end.min(len));
            }
            if idle && state.check {
                scopes.push(line_start(view.start)..view.end.min(len));
            }
            scopes.sort_by_key(|scope| scope.start);
            if let [first, second] = &mut scopes[..]
                && second.start <= first.end
            {
                first.end = first.end.max(second.end);
                scopes.pop();
            }
        }
        let mut stale = Vec::new();
        let mut settled = Vec::new();
        let mut outdated = Vec::new();
        for scope in scopes {
            // Each scope starts a line. Walk it in order: a line's rows follow
            // its first row, so no item is looked up by index.
            let mut ix = scope.start;
            let mut items = self.layout_items.iter_from(ix).peekable();
            while ix < scope.end {
                let Some(item) = items.next() else {
                    break;
                };
                let DocumentLayoutItem::Text {
                    line,
                    line_key,
                    presentation,
                    node_id,
                    frame: Derived(Some(item_frame)),
                    wrapped,
                    ..
                } = item
                else {
                    ix += 1;
                    continue;
                };
                let mut count = 1;
                while items
                .next_if(|next| {
                    matches!(next, DocumentLayoutItem::Text { line: other, node_id: other_node, .. }
                        if other == line && other_node == node_id)
                })
                .is_some()
            {
                count += 1;
            }
                let rows = ix..ix + count;
                ix = rows.end;
                let frame = if state.frames.is_empty() {
                    item_frame
                } else {
                    state
                        .frames
                        .get(&(node_id.clone(), line.clone()))
                        .unwrap_or(item_frame)
                };
                let wrap = WrapKey {
                    generation,
                    widths: frame.widths(width),
                };
                if wrapped.0 == Some(wrap) && item_frame == frame {
                    continue;
                }
                if frame.hidden || line.is_empty() {
                    if rows.len() == 1 {
                        push_settled(&mut settled, rows, wrap);
                    } else {
                        outdated.push((rows, wrap, Breaks::default()));
                    }
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
                        push_settled(&mut settled, rows, wrap);
                    } else {
                        outdated.push((rows, wrap, Breaks::default()));
                    }
                    continue;
                }
                match state.cache.lines.get(line_key) {
                    // Another line with this text and fonts already wrapped for it.
                    Some(cached)
                        if cached.generation == generation && cached.widths == wrap.widths =>
                    {
                        if self.rows_match(rows.clone(), &cached.starts) {
                            push_settled(&mut settled, rows, wrap);
                        } else {
                            outdated.push((rows, wrap, cached.starts.clone()));
                        }
                    }
                    cached => stale.push(StaleLine {
                        in_view: rows.start < view.end && view.start < rows.end,
                        focused: false,
                        items: rows,
                        line: line.clone(),
                        bytes: line.len(),
                        wrapped_before: cached.is_some() || wrapped.0.is_some(),
                        widths: wrap.widths,
                    }),
                }
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
        // Priority changes ordering, never the execution budget. Display
        // bounds every cluster's shaping (`MAX_DISPLAY_CLUSTER_CHARS`), so a
        // line's cost follows its bytes whatever its script: a short Unicode
        // line wraps in the frame like ASCII, and a long one in the background.
        let focus = self
            .reveal_target()
            .or_else(|| self.viewport_top_offset())
            .unwrap_or_else(|| self.model.cursor());
        let focus = self.model.source_to_display(focus, super::Affinity::After);
        for stale_line in &mut stale {
            stale_line.focused = focus.is_some_and(|offset| {
                stale_line.line.start <= offset && offset <= stale_line.line.end
            });
            stale_line.in_view |= stale_line.focused;
        }
        // Stable, so document order holds within each priority.
        stale.sort_by_key(|line| (!line.focused, !line.in_view));
        let (now, later): (Vec<_>, Vec<_>) = stale.into_iter().partition(|line| {
            // A small new text wraps whole. Otherwise the frame wraps the
            // lines it shows, and right after an invalidation those near them.
            let budget = if !line.wrapped_before && line.bytes <= first_budget {
                &mut first_budget
            } else if line.in_view || line.wrapped_before && nearby && scan_all {
                &mut rewrap_budget
            } else {
                return false;
            };
            if line.bytes <= REWRAP_IN_FRAME_BYTES && line.bytes <= *budget {
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
            let text = self.model.projection_map.shared_display_text();
            let lines = LineSpec::wrap_batch(&specs, text, &mut wrappers);
            let (now_settled, now_outdated) = self.take_wrapped(generation, lines);
            settled.extend(now_settled);
            outdated.extend(now_outdated);
        }
        // Both batches refer to the current layout. Freeze the background
        // inputs before settling rows changes the indices of later lines.
        // Replaced items are checked as a whole pass is, for their own range.
        let full = scan_all || dirty.is_some();
        let background = (full
            && !later.is_empty()
            && self
                .text_rows
                .as_ref()
                .is_some_and(|state| state.pending.is_none()))
        .then(|| {
            let mut fonts = FontTable::new(window);
            let specs: Vec<_> = later
                .iter()
                .take(BACKGROUND_LINES_PER_WRAP)
                .map(|line| self.line_spec(line, &mut fonts))
                .collect();
            let wrappers = fonts.wrappers(cx);
            (specs, wrappers)
        });
        self.settle_line_rows(settled, outdated, cx);

        let state = self.text_rows.as_mut().expect("text rows are on");
        if !defer {
            state.scan_all = false;
        }
        if scan_all {
            state.nearby = false;
        }
        if !full {
            return;
        }
        // Lines still stale wrap as they scroll into view.
        if scan_all {
            state.check = !later.is_empty();
        } else {
            state.check |= !later.is_empty();
        }
        let Some((specs, mut wrappers)) = background else {
            return;
        };
        let text = self.model.projection_map.shared_display_text();
        let request = state.request;
        let cancel = state.cancel.clone();
        let task = cx.spawn(async move |this, cx| {
            let (sender, receiver) = smol::channel::bounded(2);
            let worker = cx.background_spawn(async move {
                let mut lines = Vec::new();
                for spec in specs {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut work = Wrap::new(
                        &text[spec.range.clone()],
                        &spec.runs,
                        spec.widths,
                        spec.previous.clone(),
                    )
                    .with_objects(spec.objects.clone());
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        if work.step(
                            &text[spec.range.clone()],
                            &spec.runs,
                            &mut wrappers,
                            16 * 1024,
                            &cancel,
                        ) {
                            break;
                        }
                        smol::future::yield_now().await;
                    }
                    let starts = work.finish();
                    lines.push(spec.result(text.clone(), starts));
                    if (spec.priority || lines.len() == 64)
                        && sender.send(std::mem::take(&mut lines)).await.is_err()
                    {
                        return;
                    }
                }
                if !lines.is_empty() {
                    let _ = sender.send(lines).await;
                }
            });
            while let Ok(lines) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| {
                        let Some(state) = this
                            .text_rows
                            .as_mut()
                            .filter(|state| state.request == request)
                        else {
                            // Drain the bounded channel until the cancelled
                            // worker returns, then release the single slot.
                            return true;
                        };
                        state
                            .ready
                            .get_or_insert_with(|| WrapResult {
                                generation,
                                request,
                                lines: Default::default(),
                            })
                            .lines
                            .extend(lines);
                        this.schedule_row_publication(cx);
                        cx.notify();
                        true
                    })
                    .ok()
                    != Some(true)
                {
                    return;
                }
            }
            worker.await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.text_rows.as_mut() else {
                    return;
                };
                // This task is finishing; let it complete rather than cancel it.
                if let Some(task) = state.pending.take() {
                    task.detach();
                }
                state.check = true;
                state.scan_all = true;
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
                state.cache.record(
                    line.range.start,
                    line.key,
                    wrap,
                    line.starts.clone(),
                    Some(line.input),
                );
            }
            if matches {
                settled.push((rows, wrap));
            } else {
                outdated.push((rows, wrap, line.starts));
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
        self.mark_settled_rows(settled);
        if outdated.is_empty() {
            return;
        }
        outdated.sort_by_key(|(rows, _, _)| rows.start);
        outdated.dedup_by_key(|(rows, _, _)| rows.start);
        self.replace_line_rows(outdated, cx);
    }

    /// Records the wrap of one-row lines whose row already matches it. Their
    /// rows do not change, so they update in place, a run of adjacent lines
    /// at a time, without waiting for a publication.
    fn mark_settled_rows(&mut self, mut settled: Vec<(Range<usize>, WrapKey)>) {
        // Nearby lines share one pass: items between them are visited and
        // left as they are, which costs less than another tree splice.
        const MAX_GAP: usize = 256;
        settled.sort_by_key(|(rows, _)| rows.start);
        let mut settled = settled.into_iter().peekable();
        while let Some(first) = settled.next() {
            let start = first.0.start;
            let mut end = first.0.end;
            let mut batch = vec![first];
            while let Some(next) = settled.next_if(|(rows, _)| rows.start <= end + MAX_GAP) {
                end = end.max(next.0.end);
                batch.push(next);
            }
            let frames = &self.text_rows.as_ref().expect("text rows are on").frames;
            let mut heights: Vec<(usize, Option<Pixels>)> = Vec::new();
            let mut segments = batch.into_iter().peekable();
            let mut ix = start;
            self.layout_items.map_range(start..end, |item| {
                let here = ix;
                ix += 1;
                while segments.next_if(|(rows, _)| rows.end <= here).is_some() {}
                let Some((rows, wrap)) = segments.peek() else {
                    return;
                };
                if here < rows.start {
                    return;
                }
                let DocumentLayoutItem::Text {
                    display,
                    line,
                    node_id,
                    frame,
                    wrapped,
                    presentation,
                    ..
                } = item
                else {
                    return;
                };
                wrapped.0 = Some(*wrap);
                // A frame kept for an older style takes over with the wrap.
                if !frames.is_empty() {
                    if let Some(retained) = frames.get(&(node_id.clone(), line.clone())) {
                        frame.0 = Some(*retained);
                    }
                    heights.push((
                        here,
                        frame
                            .0
                            .map(|frame| frame.row_height(display, line, presentation.as_deref())),
                    ));
                }
            });
            let mut heights = heights.into_iter().peekable();
            while let Some((at, height)) = heights.next() {
                let mut run = vec![height];
                while let Some((_, height)) = heights.next_if(|(next, _)| *next == at + run.len()) {
                    run.push(height);
                }
                self.list_state.set_item_heights(at, run);
            }
        }
    }

    /// Whether the rows of line items `rows` start where `starts` says.
    fn rows_match(&self, rows: Range<usize>, starts: &Breaks) -> bool {
        // Completed multirow wraps are recognized by WrapKey during the scan.
        // Comparing every old break on the UI thread would undo bounded work.
        rows.len() == 1 && starts.is_empty()
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

    pub(in crate::document) fn inline_width_limit(&self, frame: RowFrame) -> Pixels {
        let (first, rest) = frame.widths(
            self.text_rows
                .as_ref()
                .and_then(|state| state.width)
                .unwrap_or_default(),
        );
        first.min(rest)
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
        let spec = |runs: Vec<(usize, usize)>, fonts: &FontTable| {
            let objects: Vec<_> = self
                .model
                .inline_objects_in(display.clone())
                .into_iter()
                .map(|(range, size)| {
                    (
                        range,
                        size.constrained_to(fonts.rem_size, line.widths.0.min(line.widths.1))
                            .width()
                            .to_pixels(fonts.rem_size),
                    )
                })
                .collect();
            let mut hash = DefaultHasher::new();
            for font in std::iter::once(0).chain(runs.iter().map(|(_, font)| *font)) {
                fonts.hashes[font].hash(&mut hash);
            }
            // A previous plain wrap must never reuse advances from objects,
            // even when a presentation change keeps the same source text.
            if !objects.is_empty() {
                for (range, width) in &objects {
                    range.hash(&mut hash);
                    f32::from(*width).to_bits().hash(&mut hash);
                }
            }
            let fonts_key = hash.finish();
            LineSpec {
                item: line.items.start,
                key: *line_key,
                range: display.clone(),
                runs,
                widths: line.widths,
                previous: self
                    .text_rows
                    .as_ref()
                    .filter(|_| objects.is_empty())
                    .and_then(|state| {
                        let key = state.cache.previous.get(&display.start)?;
                        let cached = state.cache.lines.get(key)?;
                        // A wrap resumes only for the same widths and fonts;
                        // check before copying its input.
                        (cached.generation == state.generation)
                            .then_some(cached.input.as_ref())
                            .flatten()
                            .filter(|input| {
                                input.fonts_key == fonts_key && input.widths == line.widths
                            })
                            .cloned()
                            .map(|mut input| {
                                input.change = state
                                    .edit
                                    .as_ref()
                                    .filter(|edit| {
                                        edit.key == *key
                                            && edit.line_start == display.start
                                            && edit.current_len == display.len()
                                    })
                                    .map(|edit| (edit.prefix, edit.suffix));
                                input
                            })
                    }),
                fonts_key,
                priority: line.in_view,
                objects,
            }
        };
        // Unstyled lines wrap in the base font.
        let inline = &self.model.resolved_styles.inline;
        let from = inline.partition_point(|style| style.display.end <= display.start);
        if matches!(
            &self.layout_items[line.items.start],
            DocumentLayoutItem::Text {
                presentation: None,
                ..
            }
        ) && inline
            .get(from)
            .is_none_or(|style| style.display.start >= display.end)
        {
            return spec(Vec::new(), fonts);
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
            return spec(Vec::new(), fonts);
        };
        let mut style = fonts.base_style.clone();
        if let Some(refinement) = font_refinement(presentation) {
            style.refine(refinement);
        }
        let size = style.font_size.to_pixels(fonts.rem_size);
        let base = style.font();
        let runs = font_runs(presentation, display.len())
            .map(|(range, overrides)| {
                let mut font = base.clone();
                overrides.apply(&mut font);
                (range.end, fonts.index(font, size))
            })
            .collect();
        spec(runs, fonts)
    }

    /// Replaces the rows of each line in `lines`, given in ascending order by
    /// their items, with the rows of its wrap, keeping the visible text in place.
    fn replace_line_rows(&mut self, lines: Vec<OutdatedLine>, cx: &mut Context<Self>) {
        let state = self.text_rows.as_mut().expect("text rows enabled");
        if let Some(work) = state.publishing.as_mut() {
            // Lines attach from the end, so earlier lines keep their indices.
            // Keep the queue in that order; a partly built line that is no
            // longer first restarts after the lines now before it.
            let front = work.lines.front().map(|(rows, _, _)| rows.start);
            for line in lines {
                let at = work
                    .lines
                    .partition_point(|(rows, _, _)| rows.start > line.0.start);
                work.lines.insert(at, line);
            }
            if work.row > 0 && work.lines.front().map(|(rows, _, _)| rows.start) != front {
                state.retired.push((
                    std::mem::take(&mut work.items),
                    std::mem::replace(&mut work.heights, gpui::PreparedListItems::new()),
                ));
                work.row = 0;
            }
        } else {
            state.publishing = Some(RowPublication {
                lines: lines.into_iter().rev().collect(),
                items: super::LayoutItems::default(),
                heights: gpui::PreparedListItems::new(),
                row: 0,
            });
        }
        if let Some(bounds) = self.last_bounds {
            let view = self.items_in_view(bounds);
            self.publish_rows_in_view(view, cx);
        }
        self.advance_row_publication(cx);
        self.schedule_row_publication(cx);
    }

    /// Attaches the queued lines in `view` now, whatever the batch budget: the
    /// frame that wraps the lines it shows also shows their rows. The queue
    /// otherwise attaches from the last line, so lines before an attached one
    /// keep their indices; lines after these move by the rows they gained.
    fn publish_rows_in_view(&mut self, view: Range<usize>, cx: &mut Context<Self>) {
        let Some(state) = self.text_rows.as_mut() else {
            return;
        };
        let Some(work) = state.publishing.as_mut() else {
            return;
        };
        // The queue runs from the last line to the first.
        let first = work
            .lines
            .partition_point(|(rows, _, _)| rows.start >= view.end);
        let last = work
            .lines
            .partition_point(|(rows, _, _)| rows.end > view.start);
        if first >= last {
            return;
        }
        // A partly built line in view restarts with the others.
        if first == 0 && work.row > 0 {
            state.retired.push((
                std::mem::take(&mut work.items),
                std::mem::replace(&mut work.heights, gpui::PreparedListItems::new()),
            ));
            work.row = 0;
        }
        let shown: std::collections::VecDeque<_> = work.lines.drain(first..last).collect();
        let gained = shown
            .iter()
            .map(|(rows, _, starts)| (starts.len() + 1) as isize - rows.len() as isize)
            .sum::<isize>();
        for (rows, _, _) in work.lines.range_mut(..first) {
            *rows = rows.start.wrapping_add_signed(gained)..rows.end.wrapping_add_signed(gained);
        }
        let rest = state.publishing.replace(RowPublication {
            lines: shown,
            items: super::LayoutItems::default(),
            heights: gpui::PreparedListItems::new(),
            row: 0,
        });
        self.advance_rows(usize::MAX, cx);
        debug_assert!(
            self.text_rows
                .as_ref()
                .is_some_and(|state| state.publishing.is_none())
        );
        if let Some(rest) = rest.filter(|rest| !rest.lines.is_empty()) {
            self.text_rows.as_mut().unwrap().publishing = Some(rest);
        }
    }

    fn schedule_row_publication(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self
            .text_rows
            .as_mut()
            .filter(|state| state.publication_task.is_none())
        else {
            return;
        };
        let request = state.request;
        state.publication_task = Some(cx.spawn(async move |this, cx| {
            loop {
                // Yield to the platform event loop as well as other executor
                // tasks. Completion is not limited to one batch per vsync.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1))
                    .await;
                let ongoing = this
                    .update(cx, |this, cx| {
                        if !this
                            .text_rows
                            .as_ref()
                            .is_some_and(|state| state.request == request)
                        {
                            return false;
                        }
                        this.consume_ready_rows(cx);
                        this.advance_row_publication(cx);
                        this.retire_row_geometry(cx);
                        let state = this.text_rows.as_mut().unwrap();
                        let ongoing = state.publishing.is_some()
                            || state.ready.is_some()
                            || !state.retired.is_empty();
                        if !ongoing && let Some(task) = state.publication_task.take() {
                            task.detach();
                        }
                        cx.notify();
                        ongoing
                    })
                    .unwrap_or(false);
                if !ongoing {
                    break;
                }
            }
        }));
    }

    /// Build a bounded batch, keeping published geometry usable until the
    /// complete candidate subtree can be attached without visiting its rows.
    /// Adjacent lines completed in one batch attach together: one splice of
    /// each tree per run, however many small lines the run holds.
    fn advance_row_publication(&mut self, cx: &mut Context<Self>) {
        const ROW_BATCH: usize = 512;
        self.advance_rows(ROW_BATCH, cx);
    }

    /// Builds and attaches up to `budget` rows of the publication.
    fn advance_rows(&mut self, mut budget: usize, cx: &mut Context<Self>) {
        let Some(mut work) = self
            .text_rows
            .as_mut()
            .and_then(|state| state.publishing.take())
        else {
            return;
        };
        // Lines completed in this batch, last first, and the rows built for them.
        let mut run: Vec<(
            Range<usize>,
            Vec<DocumentLayoutItem<I>>,
            Vec<Option<Pixels>>,
        )> = Vec::new();
        while budget > 0 {
            let Some((rows, wrap, starts)) = work.lines.front() else {
                break;
            };
            let (rows, wrap, starts) = (rows.clone(), *wrap, starts.clone());
            if work.row == 0
                && let Some(gap) = run
                    .last()
                    .map(|(last, ..)| rows.end..last.start)
                    .filter(|gap| !gap.is_empty())
            {
                // A short gap of framed text joins the run unchanged, so
                // lines between unchanged rows still attach in one splice.
                match self.unchanged_rows(gap) {
                    Some(gap) => run.push(gap),
                    None => self.attach_line_run(std::mem::take(&mut run)),
                }
            }
            let DocumentLayoutItem::Text {
                source: first,
                line,
                line_key,
                node_id,
                frame,
                ..
            } = &self.layout_items[rows.start]
            else {
                unreachable!()
            };
            let (line, line_key) = (line.clone(), *line_key);
            let DocumentLayoutItem::Text { source: last, .. } = &self.layout_items[rows.end - 1]
            else {
                unreachable!()
            };
            let segment = first.start..last.end;
            let frames = &self.text_rows.as_ref().unwrap().frames;
            let frame = if frames.is_empty() {
                frame.0
            } else {
                frames
                    .get(&(node_id.clone(), line.clone()))
                    .copied()
                    .or(frame.0)
            };
            let count = (starts.len() + 1 - work.row).min(budget);
            let mut items = Vec::with_capacity(count);
            let mut heights = Vec::with_capacity(count);
            for row_ix in work.row..work.row + count {
                let begin = if row_ix == 0 {
                    0
                } else {
                    starts.start(row_ix - 1)
                };
                let end = if row_ix == starts.len() {
                    line.len()
                } else {
                    starts.start(row_ix)
                };
                let range = line.start + begin..line.start + end;
                let mut item = if rows.len() == 1 && starts.is_empty() {
                    // Empty editable nodes can share this display boundary
                    // with another node. Preserve their explicit ownership.
                    self.layout_items[rows.start].clone()
                } else {
                    self.model.text_layout_item(
                        self.model.display_text(),
                        range.clone(),
                        segment.clone(),
                        line.clone(),
                        line_key,
                    )
                };
                if let DocumentLayoutItem::Text {
                    frame: row_frame,
                    wrapped,
                    ..
                } = &mut item
                {
                    row_frame.0 = frame;
                    wrapped.0 = Some(wrap);
                }
                heights
                    .push(frame.map(|frame| frame.row_height(&range, &line, item.presentation())));
                items.push(item);
            }
            budget -= count;
            if work.row == 0 && count == starts.len() + 1 {
                run.push((rows, items, heights));
                work.lines.pop_front();
                continue;
            }
            // A line too long for one batch is built across batches and
            // attached alone, after the lines completed before it.
            self.attach_line_run(std::mem::take(&mut run));
            work.items.extend(items);
            work.heights.extend(heights);
            work.row += count;
            if work.row <= starts.len() {
                break;
            }
            let items = std::mem::take(&mut work.items);
            let heights = std::mem::replace(&mut work.heights, gpui::PreparedListItems::new());
            self.attach_rows(rows, items, heights);
            work.row = 0;
            work.lines.pop_front();
        }
        self.attach_line_run(run);
        if !work.lines.is_empty() {
            self.text_rows.as_mut().unwrap().publishing = Some(work);
        } else {
            self.prune_row_cache();
        }
        cx.notify();
    }

    /// Items `rows` as they are, with their declared heights, to carry over
    /// within a run; `None` when they are many, or not all framed text.
    #[allow(clippy::type_complexity)]
    fn unchanged_rows(
        &self,
        rows: Range<usize>,
    ) -> Option<(
        Range<usize>,
        Vec<DocumentLayoutItem<I>>,
        Vec<Option<Pixels>>,
    )> {
        const MAX_GAP: usize = 64;
        if rows.len() > MAX_GAP {
            return None;
        }
        let mut items = Vec::with_capacity(rows.len());
        let mut heights = Vec::with_capacity(rows.len());
        for item in self.layout_items.range(rows.clone()) {
            let DocumentLayoutItem::Text {
                display,
                line,
                frame: Derived(Some(frame)),
                presentation,
                ..
            } = item
            else {
                return None;
            };
            heights.push(Some(frame.row_height(
                display,
                line,
                presentation.as_deref(),
            )));
            items.push(item.clone());
        }
        Some((rows, items, heights))
    }

    /// Attaches adjacent lines given last first, in one splice of each tree.
    fn attach_line_run(
        &mut self,
        run: Vec<(
            Range<usize>,
            Vec<DocumentLayoutItem<I>>,
            Vec<Option<Pixels>>,
        )>,
    ) {
        let (Some((last, ..)), Some((first, ..))) = (run.first(), run.last()) else {
            return;
        };
        let rows = first.start..last.end;
        let mut items = super::LayoutItems::default();
        let mut heights = gpui::PreparedListItems::new();
        for (_, line_items, line_heights) in run.into_iter().rev() {
            items.extend(line_items);
            heights.extend(line_heights);
        }
        self.attach_rows(rows, items, heights);
    }

    /// Replaces items `rows` with complete rows. The viewport is anchored to
    /// content, so it keeps its text, and a pending reveal finds its row.
    fn attach_rows(
        &mut self,
        rows: Range<usize>,
        items: super::LayoutItems<I>,
        heights: gpui::PreparedListItems,
    ) {
        self.before_list_splice();
        let old_items = self.layout_items.splice_prepared(rows.clone(), items);
        let old_heights = self.list_state.splice_prepared(rows, heights);
        self.after_list_splice();
        self.text_rows
            .as_mut()
            .unwrap()
            .retired
            .push((old_items, old_heights));
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

/// Records one-row lines whose row matches their wrap, merging adjacent ones
/// with the same wrap, so uniform text costs a handful of entries.
fn push_settled(settled: &mut Vec<(Range<usize>, WrapKey)>, rows: Range<usize>, wrap: WrapKey) {
    match settled.last_mut() {
        Some((last, last_wrap)) if last.end == rows.start && *last_wrap == wrap => {
            last.end = rows.end;
        }
        _ => settled.push((rows, wrap)),
    }
}

/// The display rows of `line`, split at `starts`, which are relative to the line.
pub(in crate::document) fn rows_of_line(
    line: Range<usize>,
    starts: impl IntoIterator<Item: std::borrow::Borrow<u32>, IntoIter: Clone>,
) -> impl Iterator<Item = Range<usize>> {
    use std::borrow::Borrow as _;
    let end = line.end;
    let starts = starts
        .into_iter()
        .map(move |start| line.start + *start.borrow() as usize)
        .filter(move |start| *start < end);
    let row_starts = std::iter::once(line.start).chain(starts.clone());
    let row_ends = starts.chain(std::iter::once(end));
    row_starts.zip(row_ends).map(|(start, end)| start..end)
}

/// Where rows after the first start in `line` when its first row wraps to
/// `widths.0` and later rows to `widths.1`, relative to the line. `runs` gives
/// each run's end and the wrapper of its font.
///
/// Reference greedy wrapper for the test backend's additive font advances.
/// Production `Wrap` also validates each row with contextual shaping; the
/// native `text_width` integration test checks its actual rendered rows.
/// Tabs advance to the next stop and clusters remain indivisible, including
/// a single cluster larger than [`MAX_ROW_BYTES`].
#[cfg(test)]
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
        let end = ix + cluster.len();
        if (x + cluster_advance > width || end - row_start > MAX_ROW_BYTES) && ix > row_start {
            let start = candidate
                .filter(|start| *start > row_start && end - start <= MAX_ROW_BYTES)
                .unwrap_or(ix);
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
    line.len() <= MAX_ROW_BYTES
        && ascii_advance * line.len() as f32 <= width - px(0.5)
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
mod font_tests;

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Entity, IntoElement, ParentElement as _, Styled as _, TestAppContext, VisualTestContext,
        div, prelude::FluentBuilder as _, size,
    };

    use super::{super::tests::document_view, *};
    use crate::document::{
        DocumentBlock, DocumentParagraphStyle, DocumentProjection, DocumentSnapshot,
        DocumentStyles, EditPolicy, ProjectionSpan,
    };

    #[gpui::test]
    fn batch_wrap_reuses_matching_lines_without_reusing_positions(cx: &mut TestAppContext) {
        let (_, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            let line = "words 中文🙂\tmore words repeated across rows";
            let text: Arc<str> = format!("{line}\n").repeat(5).into();
            let specs: Vec<_> = (0..5)
                .map(|ix| LineSpec {
                    item: ix,
                    key: 7,
                    range: ix * (line.len() + 1)..ix * (line.len() + 1) + line.len(),
                    runs: Vec::new(),
                    widths: if ix == 2 {
                        (px(70.), px(90.))
                    } else {
                        (px(120.), px(120.))
                    },
                    previous: None,
                    fonts_key: if ix == 4 { 1 } else { 0 },
                    priority: false,
                    objects: Vec::new(),
                })
                .collect();
            let mut wrappers = FontTable::new(window).wrappers(cx);
            let actual = LineSpec::wrap_batch(&specs, text.clone(), &mut wrappers);
            for (spec, actual) in specs.iter().zip(&actual) {
                let expected = spec.wrap(text.clone(), &mut wrappers);
                assert_eq!(
                    actual.starts.starts().collect::<Vec<_>>(),
                    expected.starts.starts().collect::<Vec<_>>()
                );
                assert_eq!(actual.item, expected.item);
                assert_eq!(actual.range, expected.range);
                assert_eq!(actual.input.text, expected.input.text);
                assert_eq!(actual.input.fonts_key, expected.input.fonts_key);
            }
            assert!(Arc::ptr_eq(&actual[0].input.text, &actual[1].input.text));
            assert!(!Arc::ptr_eq(&actual[1].input.text, &actual[2].input.text));
            assert!(!Arc::ptr_eq(&actual[3].input.text, &actual[4].input.text));
        });
    }

    #[gpui::test]
    fn visible_long_line_obeys_budget_and_rejects_superseded_results(cx: &mut TestAppContext) {
        let (document, mut cx) = super::super::tests::document_view(cx);
        let text: Arc<str> = "word ".repeat(100_000).into();
        document.update(&mut cx, |doc, cx| {
            doc.reset(
                DocumentSnapshot::new(
                    text.to_string(),
                    vec![DocumentRegion::new(
                        "source",
                        0..text.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::identity(text.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        cx.update(|window, cx| {
            document.update(cx, |doc, cx| {
                let bounds = |width| Bounds::new(gpui::point(px(0.), px(0.)), size(px(width), px(400.)));
                doc.prepare_text_rows(bounds(320.), window, cx);
                assert_eq!(doc.layout_items.len(), 1);
                let state = doc.text_rows.as_ref().unwrap();
                assert!(state.pending.is_some(), "visibility must not bypass the budget");
                let request = state.request;
                let generation = state.generation;
                let cancel = state.cancel.clone();
                assert!(doc.pending_row_paint_range(0..text.len(), None).len() <= 2048);
                doc.prepare_text_rows(bounds(160.), window, cx);
                assert!(cancel.load(Ordering::Relaxed));
                let DocumentLayoutItem::Text { line_key, .. } = &doc.layout_items[0] else { unreachable!() };
                let input = Previous { text: text.clone(), range: 0..text.len(), breaks: Breaks::default(), runs: Arc::from([]), widths: (px(320.), px(320.)), fonts_key: 0, change: None };
                doc.text_rows.as_mut().unwrap().ready = Some(WrapResult {
                    generation, request,
                    lines: vec![WrappedLine { item: 0, range: 0..text.len(), key: *line_key, widths: input.widths, starts: Breaks::default(), input }].into(),
                });
                doc.prepare_text_rows(bounds(160.), window, cx);
                assert!(matches!(&doc.layout_items[0], DocumentLayoutItem::Text { wrapped, .. } if wrapped.0.is_none()), "a late result cannot publish even when text and font still match");
            });
        });
        settle_rows(&document, &mut cx);
        assert_declared_geometry(&document, &mut cx);
    }

    #[gpui::test]
    fn edits_during_background_rows_settle_to_the_complete_wrap(cx: &mut TestAppContext) {
        use crate::document::{EditDecision, EditOrigin, EditTransaction, TextEdit};
        let snapshot = |text: &str| {
            DocumentSnapshot::new(
                text.to_string(),
                vec![DocumentRegion::new(
                    "draft",
                    0..text.len(),
                    EditPolicy::Editable,
                )],
                DocumentProjection::identity(text.len()),
                vec![],
                DocumentStyles::default(),
            )
        };
        let rows = |document: &Entity<DocumentState<&'static str>>, cx: &mut VisualTestContext| {
            document.read_with(cx, |doc, _| {
                doc.layout_items
                    .iter()
                    .filter_map(|item| match item {
                        DocumentLayoutItem::Text { display, .. } => Some(display.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
        };
        let edit = |document: &Entity<DocumentState<&'static str>>,
                    cx: &mut VisualTestContext,
                    text: &mut String,
                    at: usize,
                    insert: &str| {
            text.insert_str(at, insert);
            document.update(cx, |doc, cx| {
                let transaction = EditTransaction::new(
                    doc.model.revision,
                    EditOrigin::User,
                    vec![TextEdit::new(at..at, insert)],
                    doc.model.text.len(),
                )
                .unwrap();
                assert_eq!(
                    doc.commit_user_transaction(transaction, None, cx),
                    EditDecision::Apply
                );
                assert!(doc.is_laying_out_text(), "an edit leaves rows to settle");
            });
        };
        let frame = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
        };
        // More than a first frame wraps, so rows wrap in the background and
        // publish in batches while the edits arrive.
        let line = "words that wrap across several rows at this width ".repeat(4);
        let mut text = format!("{line}\n").repeat(1500);
        let mut app = cx.clone();
        let (edited, mut cx) = document_view(cx);
        edited.update(&mut cx, |doc, cx| {
            doc.reset(snapshot(&text), cx).unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        cx.simulate_resize(size(px(320.), px(400.)));
        frame(&mut cx);
        assert!(edited.read_with(&cx, |doc, _| {
            doc.text_rows.as_ref().unwrap().pending.is_some()
        }));
        let end = text.len() - 1;
        edit(&edited, &mut cx, &mut text, end, " tail");
        cx.run_until_parked();
        frame(&mut cx);
        assert!(edited.read_with(&cx, |doc, _| {
            doc.text_rows.as_ref().unwrap().publishing.is_some()
        }));
        let middle = text.len() / 2;
        edit(&edited, &mut cx, &mut text, middle, " middle words");
        frame(&mut cx);
        edit(&edited, &mut cx, &mut text, 0, "head ");
        settle_rows(&edited, &mut cx);
        // With nothing in flight, the replaced items alone remain to settle.
        edit(&edited, &mut cx, &mut text, 7, " again");
        settle_rows(&edited, &mut cx);
        assert_declared_geometry(&edited, &mut cx);

        let (fresh, mut fresh_cx) = document_view(&mut app);
        fresh.update(&mut fresh_cx, |doc, cx| {
            doc.reset(snapshot(&text), cx).unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        fresh_cx.simulate_resize(size(px(320.), px(400.)));
        settle_rows(&fresh, &mut fresh_cx);
        assert_eq!(rows(&edited, &mut cx), rows(&fresh, &mut fresh_cx));
    }

    #[gpui::test]
    fn editing_a_wrapped_unicode_paragraph_keeps_its_rows_in_the_same_frame(
        cx: &mut TestAppContext,
    ) {
        use crate::document::{EditDecision, EditOrigin, EditTransaction, TextEdit};
        let (document, mut cx) = document_view(cx);
        let text = "中文段落内容，e\u{301}👩\u{200d}💻 ".repeat(40);
        document.update(&mut cx, |doc, cx| {
            doc.reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![DocumentRegion::new(
                        "draft",
                        0..text.len(),
                        EditPolicy::Editable,
                    )],
                    DocumentProjection::identity(text.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        cx.simulate_resize(size(px(320.), px(400.)));
        settle_rows(&document, &mut cx);
        assert!(document.read_with(&cx, |doc, _| doc.layout_items.len()) > 1);

        document.update(&mut cx, |doc, cx| {
            let transaction = EditTransaction::new(
                doc.model.revision,
                EditOrigin::User,
                vec![TextEdit::new(3..3, "字")],
                doc.model.text.len(),
            )
            .unwrap();
            assert_eq!(
                doc.commit_user_transaction(transaction, None, cx),
                EditDecision::Apply
            );
        });
        // The frame drawn right after the keystroke, before background work runs.
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let drawn = document.read_with(&cx, |doc, _| {
            assert!(
                doc.layout_items.iter().all(|item| !matches!(
                    item,
                    DocumentLayoutItem::Text { wrapped, .. } if wrapped.0.is_none()
                )),
                "the edited paragraph showed unwrapped rows"
            );
            doc.layout_items.len()
        });
        assert!(drawn > 1, "the edited paragraph collapsed to one row");
        settle_rows(&document, &mut cx);
        assert_eq!(
            document.read_with(&cx, |doc, _| doc.layout_items.len()),
            drawn
        );
        assert_declared_geometry(&document, &mut cx);
    }

    #[gpui::test]
    fn rows_hold_bounded_bytes_unless_they_are_one_cluster(cx: &mut TestAppContext) {
        let mut wrappers = vec![
            cx.text_system()
                .line_wrapper(gpui::font("Zed Mono"), px(16.)),
        ];
        let cancelled = AtomicBool::new(false);
        let widths = (px(1e7), px(1e7));
        let cluster = format!("a{}", "\u{301}".repeat(MAX_ROW_BYTES));
        for text in [
            // Zero-advance text never reaches the row width.
            "\u{200b}".repeat(3 * MAX_ROW_BYTES),
            // Word boundaries are preferred within the byte bound.
            "word ".repeat(2 * MAX_ROW_BYTES),
            // An oversized cluster is a row of its own.
            format!("x{cluster}y{cluster}"),
        ] {
            let mut work = Wrap::new(&text, &[], widths, None);
            while !work.step(&text, &[], &mut wrappers, 1024, &cancelled) {}
            let starts: Vec<_> = work.finish().starts().collect();
            assert_eq!(starts, wrap_line(&text, &[], &mut wrappers, widths));
            assert!(!starts.is_empty());
            for row in rows_of_line(0..text.len(), &starts) {
                assert!(
                    row.len() <= MAX_ROW_BYTES || text[row.clone()].graphemes(true).count() == 1,
                    "row {row:?} of {} bytes",
                    row.len()
                );
            }
        }
    }

    #[gpui::test]
    fn resumed_and_incremental_wraps_match_complete_algorithm(cx: &mut TestAppContext) {
        let mut wrappers = vec![
            cx.text_system()
                .line_wrapper(gpui::font("Zed Mono"), px(16.)),
            cx.text_system()
                .line_wrapper(gpui::font("Zed Mono").bold(), px(19.)),
        ];
        let cancelled = AtomicBool::new(false);
        for text in [
            String::new(),
            "   words and tabs\tmore\t".repeat(80),
            "中e\u{301}👩\u{200d}💻🇨🇳 \t尾".repeat(100),
            // Breaks only at the row byte bound.
            "\u{200b}".repeat(2000),
        ] {
            for widths in [(px(90.), px(60.)), (px(0.), px(0.)), (px(200.), px(200.))] {
                for runs in [
                    Vec::new(),
                    vec![
                        (text.floor_char_boundary(text.len() / 2), 1),
                        (text.len(), 0),
                    ],
                ] {
                    let expected = wrap_line(&text, &runs, &mut wrappers, widths);
                    for budget in [1, 7, 1024] {
                        let mut work = Wrap::new(&text, &runs, widths, None);
                        while !work.step(&text, &runs, &mut wrappers, budget, &cancelled) {}
                        assert_eq!(work.finish().starts().collect::<Vec<_>>(), expected);
                    }
                }
            }
        }
        let old: Arc<str> = "word 中e\u{301}👩\u{200d}💻 \t tail ".repeat(3000).into();
        let widths = (px(91.), px(73.));
        let mut initial = Wrap::new(&old, &[], widths, None);
        while !initial.step(&old, &[], &mut wrappers, 256, &cancelled) {}
        let breaks = initial.finish();
        for insertion in ["x", " xyz ", "\t", "\u{301}", "👩\u{200d}"] {
            let mut text = old.to_string();
            let at = text.floor_char_boundary(text.len() / 3);
            text.insert_str(at, insertion);
            let previous = Previous {
                text: old.clone(),
                range: 0..old.len(),
                breaks: breaks.clone(),
                runs: Arc::from([]),
                widths,
                fonts_key: 0,
                change: Some((at, old.len() - at)),
            };
            let mut work = Wrap::new(&text, &[], widths, Some(previous));
            while !work.step(&text, &[], &mut wrappers, 256, &cancelled) {}
            assert!(
                work.reused > 100,
                "must reuse the converged suffix for {insertion:?}"
            );
            assert!(
                work.processed < text.len() / 10,
                "local edit must not rewrap the prefix or suffix"
            );
            assert_eq!(
                work.finish().starts().collect::<Vec<_>>(),
                wrap_line(&text, &[], &mut wrappers, widths)
            );
        }
        let runs = vec![(old.floor_char_boundary(old.len() / 2), 1), (old.len(), 0)];
        let mut initial = Wrap::new(&old, &runs, widths, None);
        while !initial.step(&old, &runs, &mut wrappers, 1024, &cancelled) {}
        let breaks = initial.finish();
        for replacement in ["", "a", "中\t", "e\u{301}👩\u{200d}💻"] {
            let mut text = old.to_string();
            let start = text.floor_char_boundary(text.len() / 3);
            let end = text.ceil_char_boundary(start + 7);
            text.replace_range(start..end, replacement);
            let delta = text.len() as isize - old.len() as isize;
            let next_runs: Vec<_> = runs
                .iter()
                .map(|(end, font)| (end.checked_add_signed(delta).unwrap(), *font))
                .collect();
            let previous = Previous {
                text: old.clone(),
                range: 0..old.len(),
                breaks: breaks.clone(),
                runs: runs.clone().into(),
                widths,
                fonts_key: 0,
                change: Some((start, old.len() - end)),
            };
            let mut work = Wrap::new(&text, &next_runs, widths, Some(previous));
            while !work.step(&text, &next_runs, &mut wrappers, 1024, &cancelled) {}
            assert!(work.reused > 100);
            assert_eq!(
                work.finish().starts().collect::<Vec<_>>(),
                wrap_line(&text, &next_runs, &mut wrappers, widths)
            );
        }
    }

    #[gpui::test]
    fn transaction_deltas_seed_line_checkpoints_without_a_text_diff(cx: &mut TestAppContext) {
        use crate::document::{EditDecision, EditOrigin, EditTransaction, TextEdit};
        let (document, mut cx) = super::super::tests::document_view(cx);
        let text = "word ".repeat(10_000);
        document.update(&mut cx, |doc, cx| {
            doc.reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![DocumentRegion::new(
                        "draft",
                        0..text.len(),
                        EditPolicy::Editable,
                    )],
                    DocumentProjection::identity(text.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        settle_rows(&document, &mut cx);
        cx.update(|window, cx| {
            document.update(cx, |doc, cx| {
                let widths = match &doc.layout_items[0] {
                    DocumentLayoutItem::Text { frame, .. } => frame
                        .0
                        .unwrap()
                        .widths(doc.text_rows.as_ref().unwrap().width.unwrap()),
                    _ => unreachable!(),
                };
                for (at, insert) in [(100, "x"), (200, "y")] {
                    let transaction = EditTransaction::new(
                        doc.model.revision,
                        EditOrigin::User,
                        vec![TextEdit::new(at..at + 1, insert)],
                        doc.model.text.len(),
                    )
                    .unwrap();
                    assert_eq!(
                        doc.commit_user_transaction(transaction, None, cx),
                        EditDecision::Apply
                    );
                }
                let hint = doc.text_rows.as_ref().unwrap().edit.as_ref().unwrap();
                assert_eq!(
                    (hint.prefix, hint.suffix, hint.current_len),
                    (100, text.len() - 201, text.len())
                );
                let stale = StaleLine {
                    items: 0..1,
                    line: 0..text.len(),
                    in_view: true,
                    focused: false,
                    bytes: text.len(),
                    wrapped_before: true,
                    widths,
                };
                let mut fonts = FontTable::new(window);
                let spec = doc.line_spec(&stale, &mut fonts);
                assert_eq!(
                    spec.previous.as_ref().unwrap().change,
                    Some((100, text.len() - 201))
                );
                let mut wrappers = fonts.wrappers(cx);
                let source = doc.model.display_text();
                let mut work = Wrap::new(source, &spec.runs, spec.widths, spec.previous.clone());
                while !work.step(
                    source,
                    &spec.runs,
                    &mut wrappers,
                    1024,
                    &AtomicBool::new(false),
                ) {}
                assert!(work.processed < 1024);
                assert!(work.reused > 100);
                assert_eq!(
                    work.finish().starts().collect::<Vec<_>>(),
                    wrap_line(source, &spec.runs, &mut wrappers, spec.widths)
                );
            });
        });
    }

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

    pub(super) fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    pub(super) fn settle_rows(
        document: &Entity<DocumentState<&'static str>>,
        cx: &mut VisualTestContext,
    ) {
        for _ in 0..10000 {
            draw(cx);
            if document.read_with(cx, |doc, _| !doc.is_laying_out_text()) {
                return;
            }
        }
        panic!("text-row work failed to settle");
    }

    /// Every item's list height is what its frame or block declares, and the
    /// list extent is their sum.
    pub(super) fn assert_declared_geometry(
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
                    } => frame.0.expect("every row is framed").row_height(
                        display,
                        line,
                        item.presentation(),
                    ),
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
    fn plain_lines_frame_in_one_pass_before_the_general_path(cx: &mut TestAppContext) {
        // Plain lines open in one pass; from the styled heading on, lines and
        // blocks take the general path, in the same first frame.
        let (document, mut cx) = document_view(cx);
        let wide = "words that wrap ".repeat(40);
        let plain = format!("short\n{wide}\n中文的段落\n\n");
        let source = format!("{plain}Heading\nafter\n{OBJECT}tail");
        let text_end = source.len() - OBJECT.len() - "tail".len();
        let fixed = text_end..text_end + OBJECT.len();
        let heading = DocumentParagraphStyle::new(
            plain.len()..plain.len() + "Heading\n".len(),
            gpui::TextStyleRefinement {
                font_size: Some(px(24.).into()),
                ..Default::default()
            },
        );
        document.update(&mut cx, |document, cx| {
            document.set_block_renderer(|_, _, _| div().w_full().h(px(40.)).into_any_element(), cx);
            document.set_block_height_resolver(|_, _, _| Some(px(40.).into()), cx);
            document.set_text_rows(Some(TextRows::new()), cx);
            document
                .reset(
                    DocumentSnapshot::new(
                        source.clone(),
                        vec![
                            DocumentRegion::new("text", 0..text_end, EditPolicy::Readonly),
                            DocumentRegion::new("fixed", fixed.clone(), EditPolicy::Atomic),
                            DocumentRegion::new(
                                "draft",
                                fixed.end..source.len(),
                                EditPolicy::Editable,
                            ),
                        ],
                        DocumentProjection::new(
                            source.len(),
                            vec![ProjectionSpan::hide(fixed.clone())],
                        )
                        .unwrap(),
                        vec![DocumentBlock::new("fixed", fixed)],
                        DocumentStyles::new(vec![heading], Vec::new()),
                    ),
                    cx,
                )
                .unwrap();
        });
        cx.simulate_resize(size(px(320.), px(400.)));
        draw(&mut cx);
        assert_declared_geometry(&document, &mut cx);
        let line_height = |text: &str, cx: &mut VisualTestContext| {
            document.read_with(cx, |document, _| {
                document
                    .layout_items
                    .iter()
                    .find_map(|item| match item {
                        DocumentLayoutItem::Text { display, frame, .. }
                            if &document.display_text()[display.clone()] == text =>
                        {
                            frame.0.map(|frame| frame.line_height)
                        }
                        _ => None,
                    })
                    .unwrap()
            })
        };
        assert!(line_height("Heading", &mut cx) > line_height("short", &mut cx));
        assert_eq!(line_height("after", &mut cx), line_height("short", &mut cx));

        settle_rows(&document, &mut cx);
        assert_declared_geometry(&document, &mut cx);
        let rows = line_rows(&document, "text", &mut cx);
        assert_eq!(rows[0], "short");
        assert!(rows.len() > 7, "the wide line wraps: {rows:?}");
        assert_eq!(rows.concat(), source[..text_end].replace('\n', ""));
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

    /// A read-only text of `lines` long lines that turns on text rows in a
    /// window already laid out at 320px, as a newly opened text does. The
    /// update that opens it draws its first frame.
    fn long_lines(
        lines: usize,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DocumentState<&'static str>>,
        VisualTestContext,
        String,
    ) {
        let (document, mut cx) = super::super::tests::document_view(cx);
        cx.simulate_resize(size(px(320.), px(400.)));
        draw(&mut cx);
        let text = (0..lines)
            .map(|line| format!("{line:05} {}", "words that wrap ".repeat(8)))
            .collect::<Vec<_>>()
            .join("\n");
        document.update(&mut cx, |doc, cx| {
            doc.reset(
                DocumentSnapshot::new(
                    text.clone(),
                    vec![DocumentRegion::new(
                        "text",
                        0..text.len(),
                        EditPolicy::Readonly,
                    )],
                    DocumentProjection::identity(text.len()),
                    vec![],
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        let first = text.lines().next().unwrap().to_owned();
        (document, cx, first)
    }

    #[gpui::test]
    fn the_first_layout_shows_its_wrapped_lines_while_the_rest_publish(cx: &mut TestAppContext) {
        // Small enough to wrap whole in the first frame, but more rows than
        // one publication batch, which attaches from the last line.
        let (document, mut cx, first) = long_lines(600, cx);
        assert!(first.len() * 600 < FIRST_WRAP_IN_FRAME_BYTES);
        assert!(document.read_with(&cx, |doc, _| doc.is_laying_out_text()));
        let rows = line_rows(&document, "text", &mut cx);
        assert!(rows[0].len() < first.len(), "{:?}", rows[0]);
        settle_rows(&document, &mut cx);
        assert_declared_geometry(&document, &mut cx);
        assert_eq!(line_rows(&document, "text", &mut cx)[0], rows[0]);
    }

    #[gpui::test]
    fn a_large_new_text_wraps_its_lines_in_view_in_the_first_layout(cx: &mut TestAppContext) {
        let (document, mut cx, first) = long_lines(2500, cx);
        assert!(first.len() * 2500 > FIRST_WRAP_IN_FRAME_BYTES);
        // The rest wraps in the background.
        assert!(document.read_with(&cx, |doc, _| doc.is_laying_out_text()));
        let rows = line_rows(&document, "text", &mut cx);
        assert!(rows[0].len() < first.len(), "{:?}", rows[0]);
        settle_rows(&document, &mut cx);
        assert_declared_geometry(&document, &mut cx);
        assert_eq!(line_rows(&document, "text", &mut cx)[0], rows[0]);
    }

    #[gpui::test]
    fn large_rewraps_land_in_the_background_with_consistent_heights(cx: &mut TestAppContext) {
        // Far more text than a frame rewraps, all of it out of view.
        let paragraph = (0..400)
            .map(|line| format!("{line} {}", "words that wrap ".repeat(12)))
            .collect::<Vec<_>>()
            .join("\n");
        let (document, mut cx, _) = framed_document(&paragraph, cx);
        settle_rows(&document, &mut cx);
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

            settle_rows(&document, &mut cx);
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
