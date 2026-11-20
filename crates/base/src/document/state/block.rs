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

use gpui::{AbsoluteLength, App, Font, Pixels, SharedString, Window, px};

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
    fn get(&mut self, font: &Font, size: Pixels, cx: &App) -> Pixels {
        if let Some((_, _, advance)) = self
            .0
            .iter()
            .find(|(known, known_size, _)| known == font && *known_size == size)
        {
            return *advance;
        }
        let mut wrapper = cx.text_system().line_wrapper(font.clone(), size);
        let mut buffer = [0; 4];
        let advance = (b' '..=b'~').fold(Pixels::ZERO, |widest, byte| {
            widest.max(wrapper.cluster_width(char::from(byte).encode_utf8(&mut buffer)))
        });
        self.0.push((font.clone(), size, advance));
        advance
    }
}

/// Wraps `text` for a block `width` wide. Lines that fit their row whole, as
/// most short ASCII lines do, are not shaped.
pub(in crate::document) fn wrap_block(
    text: &Arc<BlockText>,
    width: Pixels,
    advances: &mut BlockAdvances,
    window: &Window,
    cx: &App,
) -> WrappedBlock {
    let rem_size = window.rem_size();
    let frame = &text.frame;
    let available =
        (width - frame.inset_start().to_pixels(rem_size) - frame.inset_end().to_pixels(rem_size))
            .max(px(0.));
    let row_width = frame
        .max_width()
        .map_or(available, |max| max.to_pixels(rem_size).min(available));
    let font_size = text.font_size.to_pixels(rem_size);
    // The line height a text element lays out with, snapped to device pixels.
    let line_height = window.pixel_snap(text.line_height.to_pixels(rem_size));
    let advance = advances.get(&text.font, font_size, cx);
    let mut wrapper = None;
    let mut rows = Vec::new();
    let mut start = 0;
    for line in text.text.split('\n') {
        if fits_unwrapped(line, advance, row_width) {
            rows.push(start..start + line.len());
        } else {
            let wrapper = wrapper
                .get_or_insert_with(|| cx.text_system().line_wrapper(text.font.clone(), font_size));
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
