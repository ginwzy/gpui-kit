//! Block geometry a host declares in text rows.
//!
//! A block's height is data, as a text row's is: the host declares a fixed
//! height, or text that Base wraps to the block's width as it wraps display
//! lines, and the block's renderer draws that text by the rows Base hands it.
//! The list then knows the block's height without laying the block out; only
//! blocks declared measured are measured.

#[cfg(test)]
mod tests;

use std::{ops::Range, sync::Arc};

use gpui::{
    AbsoluteLength, App, Font, LineIndent, Pixels, SharedString, TextRun, TextSystem, Window,
    WindowTextSystem, px,
};

use super::text_rows::{TextFrame, fits_unwrapped, wrap_block_line};

/// How tall a block is in text rows, as its host declares it through
/// [`DocumentState::set_block_geometry_resolver`](super::DocumentState::set_block_geometry_resolver).
/// Base evaluates the declaration at the block's width; the list lays out
/// only blocks declared [`measured`](Self::measured) to learn their height.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockGeometry(Declaration);

#[derive(Clone, Debug, PartialEq)]
pub(in crate::document) enum Declaration {
    Measured,
    Fixed(AbsoluteLength),
    Text(Arc<BlockText>),
}

impl BlockGeometry {
    /// The list measures the block. Keep this for blocks whose number follows
    /// the user's actions, not the document's length.
    pub fn measured() -> Self {
        Self(Declaration::Measured)
    }

    /// The same height at every width.
    pub fn fixed(height: impl Into<AbsoluteLength>) -> Self {
        Self(Declaration::Fixed(height.into()))
    }

    /// Text that Base wraps to the block's width. The block's renderer draws
    /// the rows its context carries, each unwrapped, at the text's line height.
    pub fn text(text: BlockText) -> Self {
        Self(Declaration::Text(Arc::new(text)))
    }

    pub(in crate::document) fn declaration(self) -> Declaration {
        self.0
    }
}

/// Text that sets a block's height: one font, its line height, and the frame
/// around it. Base wraps the text as it wraps display lines, to the frame's
/// width in the block; a line break in the text always starts a row. The
/// block is the frame's space before the rows, the rows (at least the
/// minimum height), and the frame's space after them. Spaces are taken as
/// given: a host that lays them out as separate boxes adds them as layout
/// rounds each box.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockText {
    text: SharedString,
    font: Font,
    font_size: AbsoluteLength,
    line_height: AbsoluteLength,
    frame: TextFrame,
    min_height: AbsoluteLength,
    paragraphs: Vec<BlockParagraph>,
}

/// A native text element below the block's title. It keeps its own painting
/// and hit testing; only its wrapped height is needed by the list.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockParagraph {
    text: SharedString,
    font: Font,
    size: Pixels,
    line: Pixels,
    inset: Pixels,
    indent: Pixels,
    min_height: Pixels,
    runs: Vec<gpui::TextRun>,
}

impl BlockParagraph {
    pub fn new(text: impl Into<SharedString>, font: Font, size: Pixels, line: Pixels) -> Self {
        Self {
            text: text.into(),
            font,
            size,
            line,
            inset: px(0.),
            indent: px(0.),
            min_height: px(0.),
            runs: Vec::new(),
        }
    }

    /// A `StyledText` paragraph. Shaping keeps its run boundaries and fonts,
    /// including changes of weight or style within a line.
    pub fn styled(
        text: SharedString,
        style: &gpui::TextStyle,
        size: Pixels,
        line: Pixels,
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    ) -> Self {
        let mut part = Self::new(text, style.font(), size, line);
        if !highlights.is_empty() {
            part.runs = crate::text::highlighted_runs(part.text.len(), style, highlights);
        }
        part
    }

    /// Space outside the text element, including any leading sibling.
    pub fn with_inset(mut self, inset: Pixels) -> Self {
        self.inset = inset;
        self
    }

    /// A uniform line indent *inside* the text element (for example a gutter).
    pub fn with_indent(mut self, indent: Pixels) -> Self {
        self.indent = indent;
        self
    }

    pub fn with_min_height(mut self, height: Pixels) -> Self {
        self.min_height = height;
        self
    }

    fn height(
        &self,
        width: Pixels,
        advances: &mut BlockAdvances,
        line: Pixels,
        mut run: TextRun,
        system: &WindowTextSystem,
    ) -> Pixels {
        let width = (width - self.inset).max(px(0.));
        let mut advance = advances.get(&self.font, self.size, system);
        for run in &self.runs {
            if run.font != self.font {
                advance = advance.max(advances.get(&run.font, self.size, system));
            }
        }
        let mut count = 0usize;
        if self.text.split('\n').all(|text| {
            count += 1;
            fits_unwrapped(text, advance, width - self.indent)
        }) {
            return self.min_height.max(line * count as f32);
        }
        run.font = self.font.clone();
        let runs = if self.runs.is_empty() {
            std::slice::from_ref(&run)
        } else {
            &self.runs
        };
        system
            .shape_indented_text(
                self.text.clone(),
                self.size,
                runs,
                Some(width),
                None,
                LineIndent::uniform(self.indent),
            )
            .map_or(self.min_height, |lines| {
                lines
                    .iter()
                    .map(|text| text.size(line).height)
                    .sum::<Pixels>()
                    .max(self.min_height)
            })
    }
}

impl BlockText {
    pub fn new(
        text: impl Into<SharedString>,
        font: Font,
        font_size: impl Into<AbsoluteLength>,
        line_height: impl Into<AbsoluteLength>,
    ) -> Self {
        Self {
            text: text.into(),
            font,
            font_size: font_size.into(),
            line_height: line_height.into(),
            frame: TextFrame::new(),
            min_height: px(0.).into(),
            paragraphs: Vec::new(),
        }
    }

    /// The space the block keeps before and after the rows, and the widths it
    /// keeps beside them; the rows wrap to the width left, as a framed text
    /// row does.
    pub fn with_frame(mut self, frame: TextFrame) -> Self {
        self.frame = frame;
        self
    }

    /// The least height the rows take, as when they sit beside taller controls.
    pub fn with_min_height(mut self, height: impl Into<AbsoluteLength>) -> Self {
        self.min_height = height.into();
        self
    }

    pub fn text(&self) -> &SharedString {
        &self.text
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    pub fn font_size(&self) -> AbsoluteLength {
        self.font_size
    }

    pub fn line_height(&self) -> AbsoluteLength {
        self.line_height
    }

    pub fn frame(&self) -> &TextFrame {
        &self.frame
    }

    pub fn with_paragraphs(mut self, paragraphs: Vec<BlockParagraph>) -> Self {
        self.paragraphs = paragraphs;
        self
    }

    pub(super) fn wrap_bytes(&self) -> usize {
        self.text.len() + self.paragraphs.iter().map(|p| p.text.len()).sum::<usize>()
    }

    pub fn min_height(&self) -> AbsoluteLength {
        self.min_height
    }
}

/// A block's text as Base wrapped it, for the block's renderer: the byte range
/// of each row in the text, drawn unwrapped, one below another.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockRows {
    text: SharedString,
    rows: Arc<[Range<usize>]>,
    line_height: Pixels,
    width: Pixels,
}

impl BlockRows {
    pub fn text(&self) -> &SharedString {
        &self.text
    }

    /// Each row's range in [`Self::text`], first to last. A line break ends a
    /// row and belongs to none.
    pub fn rows(&self) -> &[Range<usize>] {
        &self.rows
    }

    /// The height each row is drawn at, snapped to device pixels.
    pub fn line_height(&self) -> Pixels {
        self.line_height
    }

    /// The width the rows were wrapped to.
    pub fn width(&self) -> Pixels {
        self.width
    }
}

/// What a block's renderer is given: the block's ID and, when its geometry
/// declares text, the rows Base wrapped that text to.
pub struct DocumentBlockContext<I> {
    id: I,
    rows: Option<BlockRows>,
}

impl<I> DocumentBlockContext<I> {
    pub(in crate::document) fn new(id: I, rows: Option<BlockRows>) -> Self {
        Self { id, rows }
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn rows(&self) -> Option<&BlockRows> {
        self.rows.as_ref()
    }
}

#[cfg(test)]
thread_local! {
    /// While a test records them, the bytes of block text each rewrap pass
    /// took beyond the view.
    pub(in crate::document) static REWRAP_PASSES: std::cell::RefCell<Option<Vec<usize>>> =
        const { std::cell::RefCell::new(None) };
}

/// Declared text wrapped for one block width.
#[derive(Debug, PartialEq)]
pub(in crate::document) struct WrappedBlock {
    text: Arc<BlockText>,
    rows: BlockRows,
    /// The width of the block, not of its rows.
    width: Pixels,
    height: Pixels,
}

impl WrappedBlock {
    pub(in crate::document) fn text(&self) -> &Arc<BlockText> {
        &self.text
    }

    pub(in crate::document) fn rows(&self) -> &BlockRows {
        &self.rows
    }

    pub(in crate::document) fn width(&self) -> Pixels {
        self.width
    }

    pub(in crate::document) fn height(&self) -> Pixels {
        self.height
    }
}

/// The widest printable ASCII character of each font blocks wrap in, so a
/// short ASCII line fits its row without a wrap.
#[derive(Default)]
pub(in crate::document) struct BlockAdvances(Vec<(Font, Pixels, Pixels)>);

impl BlockAdvances {
    fn get(&mut self, font: &Font, size: Pixels, system: &Arc<TextSystem>) -> Pixels {
        if let Some((_, _, advance)) = self
            .0
            .iter()
            .find(|(known, known_size, _)| known == font && *known_size == size)
        {
            return *advance;
        }
        let mut wrapper = system.line_wrapper(font.clone(), size);
        let mut buffer = [0; 4];
        let advance = (b' '..=b'~').fold(Pixels::ZERO, |widest, byte| {
            widest.max(wrapper.cluster_width(char::from(byte).encode_utf8(&mut buffer)))
        });
        self.0.push((font.clone(), size, advance));
        advance
    }
}

/// Keeps the header and accumulated native heights private until the whole
/// declaration is ready. Its identity follows the declaration, not a list index.
pub(super) struct BlockWork {
    text: Arc<BlockText>,
    width: Pixels,
    header: Option<WrappedBlock>,
    paragraph: usize,
    body: Pixels,
}

impl BlockWork {
    pub(super) fn new(text: Arc<BlockText>, width: Pixels) -> Self {
        Self {
            text,
            width,
            header: None,
            paragraph: 0,
            body: px(0.),
        }
    }

    pub(super) fn matches(&self, text: &Arc<BlockText>, width: Pixels) -> bool {
        self.width == width && Arc::ptr_eq(&self.text, text)
    }

    pub(super) fn next_bytes(&self) -> Option<usize> {
        if self.header.is_none() {
            return Some(self.text.text.len().max(1));
        }
        if self.text.frame.hidden() {
            return None;
        }
        self.text
            .paragraphs
            .get(self.paragraph)
            .map(|p| p.text.len().max(1))
    }

    pub(super) fn metrics(&self, window: &Window) -> (Pixels, Pixels, TextRun) {
        let rem = window.rem_size();
        let (line, len) = if self.header.is_none() {
            (self.text.line_height.to_pixels(rem), self.text.text.len())
        } else {
            let p = &self.text.paragraphs[self.paragraph];
            (p.line, p.text.len())
        };
        (
            rem,
            window.pixel_snap(line),
            window.text_style().to_run(len),
        )
    }

    pub(super) fn step(
        &mut self,
        (rem, line, run): (Pixels, Pixels, TextRun),
        advances: &mut BlockAdvances,
        system: &WindowTextSystem,
    ) {
        if self.header.is_none() {
            self.header = Some(wrap_header(
                &self.text, self.width, rem, line, advances, system,
            ));
        } else {
            self.body += self.text.paragraphs[self.paragraph]
                .height(self.width, advances, line, run, system);
            self.paragraph += 1;
        }
    }

    pub(super) fn finish(self) -> WrappedBlock {
        debug_assert!(self.next_bytes().is_none());
        let mut wrapped = self.header.expect("a finished block has a header");
        wrapped.height += self.body;
        wrapped
    }
}

/// Wraps `text` for a block `width` wide. Lines that fit their row whole, as
/// most short ASCII lines do, are not shaped.
pub(in crate::document) fn wrap_block(
    text: &Arc<BlockText>,
    width: Pixels,
    advances: &mut BlockAdvances,
    window: &Window,
    _cx: &App,
) -> WrappedBlock {
    let rem_size = window.rem_size();
    let mut wrapped = wrap_header(
        text,
        width,
        rem_size,
        window.pixel_snap(text.line_height.to_pixels(rem_size)),
        advances,
        window.text_system(),
    );
    if !text.frame.hidden() {
        let body = text
            .paragraphs
            .iter()
            .map(|p| {
                p.height(
                    width,
                    advances,
                    window.pixel_snap(p.line),
                    window.text_style().to_run(p.text.len()),
                    window.text_system(),
                )
            })
            .sum::<Pixels>();
        wrapped.height += body;
    }
    wrapped
}

fn wrap_header(
    text: &Arc<BlockText>,
    width: Pixels,
    rem_size: Pixels,
    line_height: Pixels,
    advances: &mut BlockAdvances,
    system: &Arc<TextSystem>,
) -> WrappedBlock {
    let frame = &text.frame;
    let available =
        (width - frame.inset_start().to_pixels(rem_size) - frame.inset_end().to_pixels(rem_size))
            .max(px(0.));
    let row_width = frame
        .max_width()
        .map_or(available, |max| max.to_pixels(rem_size).min(available));
    let font_size = text.font_size.to_pixels(rem_size);
    // The line height a text element lays out with, snapped to device pixels.
    let advance = advances.get(&text.font, font_size, system);
    let mut wrapper = None;
    let mut rows = Vec::new();
    let mut start = 0;
    for line in text.text.split('\n') {
        if fits_unwrapped(line, advance, row_width) {
            rows.push(start..start + line.len());
        } else {
            let wrapper =
                wrapper.get_or_insert_with(|| system.line_wrapper(text.font.clone(), font_size));
            let mut row = start;
            for next in wrap_block_line(line, wrapper, row_width) {
                rows.push(row..start + next);
                row = start + next;
            }
            rows.push(row..start + line.len());
        }
        start += line.len() + 1;
    }
    let height = if frame.hidden() {
        px(0.)
    } else {
        let rows_height =
            (line_height * rows.len() as f32).max(text.min_height.to_pixels(rem_size));
        frame.space_before().to_pixels(rem_size)
            + rows_height
            + frame.space_after().to_pixels(rem_size)
    };
    WrappedBlock {
        rows: BlockRows {
            text: text.text.clone(),
            rows: rows.into(),
            line_height,
            width: row_width,
        },
        text: text.clone(),
        width,
        height,
    }
}
