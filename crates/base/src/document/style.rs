use std::{collections::BinaryHeap, ops::Range};

use gpui::{HighlightStyle, Hsla, SharedString, TextStyleRefinement};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentStyles {
    paragraphs: Vec<DocumentParagraphStyle>,
    pub(super) inline: Vec<DocumentInlineStyle>,
}

impl DocumentStyles {
    pub(super) fn splice(&mut self, range: Range<usize>, length: usize, mut replacement: Self) {
        let delta = length as isize - range.len() as isize;
        let shift = |source: &mut Range<usize>, delta: isize| {
            *source = source.start.checked_add_signed(delta).unwrap()
                ..source.end.checked_add_signed(delta).unwrap();
        };
        for style in &mut replacement.paragraphs {
            shift(&mut style.source, range.start as isize);
        }
        for style in &mut replacement.inline {
            shift(&mut style.source, range.start as isize);
        }
        let from = self
            .paragraphs
            .partition_point(|style| style.source.end <= range.start);
        let to = self
            .paragraphs
            .partition_point(|style| style.source.start < range.end);
        for style in &mut self.paragraphs[to..] {
            shift(&mut style.source, delta);
        }
        self.paragraphs.splice(from..to, replacement.paragraphs);
        let from = self
            .inline
            .partition_point(|style| style.source.end <= range.start);
        let to = self
            .inline
            .partition_point(|style| style.source.start < range.end);
        for style in &mut self.inline[to..] {
            shift(&mut style.source, delta);
        }
        self.inline.splice(from..to, replacement.inline);
    }

    pub fn new(
        mut paragraphs: Vec<DocumentParagraphStyle>,
        mut inline: Vec<DocumentInlineStyle>,
    ) -> Self {
        paragraphs.sort_by_key(|style| (style.source.start, style.source.end));
        inline.sort_by_key(|style| (style.source.start, style.source.end));
        Self { paragraphs, inline }
    }

    pub fn paragraphs(&self) -> &[DocumentParagraphStyle] {
        &self.paragraphs
    }

    pub fn inline(&self) -> &[DocumentInlineStyle] {
        &self.inline
    }

    pub fn is_empty(&self) -> bool {
        self.paragraphs.is_empty() && self.inline.is_empty()
    }

    /// Follows `edits` inside editable source `region`: styles at its edges
    /// or inside it transform, later styles move by `delta`. Returns the
    /// paragraph and inline styles that transformed, by index.
    pub(super) fn edit_region(
        &mut self,
        region: Range<usize>,
        edits: &[super::TextEdit],
        delta: isize,
    ) -> (Range<usize>, Range<usize>) {
        let shift = |source: &mut Range<usize>| {
            *source = source.start.checked_add_signed(delta).unwrap()
                ..source.end.checked_add_signed(delta).unwrap();
        };
        let from = self
            .paragraphs
            .partition_point(|style| style.source.end < region.start);
        let to = self
            .paragraphs
            .partition_point(|style| style.source.start <= region.end);
        for style in &mut self.paragraphs[from..to] {
            style.source = super::position::transform_source_range(style.source.clone(), edits);
        }
        for style in &mut self.paragraphs[to..] {
            shift(&mut style.source);
        }
        let paragraphs = from..to;
        let from = self
            .inline
            .partition_point(|style| style.source.end < region.start);
        let to = self
            .inline
            .partition_point(|style| style.source.start <= region.end);
        for style in &mut self.inline[to..] {
            shift(&mut style.source);
        }
        // As in `transformed`, a replaced token's decoration goes with it.
        let mut kept = from;
        for ix in from..to {
            let source =
                super::position::transform_source_range(self.inline[ix].source.clone(), edits);
            if !source.is_empty() {
                self.inline[ix].source = source;
                self.inline.swap(kept, ix);
                kept += 1;
            }
        }
        self.inline.drain(kept..to);
        (paragraphs, from..kept)
    }

    #[cfg(debug_assertions)]
    pub(super) fn transformed(&self, edits: &[super::TextEdit]) -> Self {
        let mut styles = self.clone();
        for style in &mut styles.paragraphs {
            style.source = super::position::transform_source_range(style.source.clone(), edits);
        }
        for style in &mut styles.inline {
            style.source = super::position::transform_source_range(style.source.clone(), edits);
        }
        // Inline ranges exclude insertions at their boundaries. Replacing a
        // whole styled token can therefore invert its endpoints; the old
        // decoration has been deleted and must not reject the text edit.
        styles.inline.retain(|style| !style.source.is_empty());
        styles
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentParagraphStyle {
    source: Range<usize>,
    text_style: TextStyleRefinement,
    hanging_marker: Option<SharedString>,
}

impl DocumentParagraphStyle {
    pub fn new(source: Range<usize>, text_style: TextStyleRefinement) -> Self {
        Self {
            source,
            text_style,
            hanging_marker: None,
        }
    }

    /// Declares that the paragraph's display text starts with `marker`, such
    /// as a list bullet. Wrapped rows and later lines of the paragraph align
    /// with the text after it instead of with the marker.
    pub fn with_hanging_marker(mut self, marker: impl Into<SharedString>) -> Self {
        self.hanging_marker = Some(marker.into());
        self
    }

    pub fn hanging_marker(&self) -> Option<&SharedString> {
        self.hanging_marker.as_ref()
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub fn text_style(&self) -> &TextStyleRefinement {
        &self.text_style
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentInlineStyle {
    source: Range<usize>,
    highlight: HighlightStyle,
    font_family: Option<SharedString>,
    link: Option<SharedString>,
    link_hover: Option<Hsla>,
    font_scale: Option<u8>,
}

impl DocumentInlineStyle {
    /// Merge overlapping byte ranges in input order, taking the last explicit
    /// value independently for each field. Empty ranges only add boundaries;
    /// default-only layers retain coverage. All ranges use the same coordinates.
    pub fn flatten(styles: Vec<Self>) -> Vec<Self> {
        let mut events = styles
            .iter()
            .enumerate()
            .flat_map(|(ix, style)| {
                [
                    (style.source.start, ix, true),
                    (style.source.end, ix, false),
                ]
            })
            .collect::<Vec<_>>();
        events.sort_unstable_by_key(|event| event.0);

        // Each field independently takes the last covering Some in input order.
        // A layer enters/leaves each heap at most once: O(S log S) time and O(S)
        // space even for fully nested ranges, without rescanning active layers.
        let mut fields: [BinaryHeap<usize>; 11] = std::array::from_fn(|_| BinaryHeap::new());
        let mut active = 0_usize;
        let mut result = Vec::with_capacity(styles.len());
        let mut boundaries = events.chunk_by(|a, b| a.0 == b.0).peekable();
        while let Some(events) = boundaries.next() {
            let start = events[0].0;
            let Some(next) = boundaries.peek() else { break };
            for &(_, ix, entering) in events {
                let style = &styles[ix];
                // Empty/reversed ranges still contribute boundaries, as before.
                if style.source.is_empty() {
                    continue;
                }
                if !entering {
                    active -= 1;
                    continue;
                }
                active += 1;
                let layer = style.highlight;
                let present = [
                    layer.color.is_some(),
                    layer.font_weight.is_some(),
                    layer.font_style.is_some(),
                    layer.background_color.is_some(),
                    layer.underline.is_some(),
                    layer.strikethrough.is_some(),
                    layer.fade_out.is_some(),
                    style.font_family.is_some(),
                    style.link.is_some(),
                    style.link_hover.is_some(),
                    style.font_scale.is_some(),
                ];
                for (heap, present) in fields.iter_mut().zip(present) {
                    if present {
                        heap.push(ix);
                    }
                }
            }
            // Coverage is separate from the heaps: a default-only layer emits a run.
            if active == 0 {
                continue;
            }
            let [
                color,
                font_weight,
                font_style,
                background_color,
                underline,
                strikethrough,
                fade_out,
                font_family,
                link,
                link_hover,
                font_scale,
            ] = fields.each_mut().map(|heap| {
                while heap
                    .peek()
                    .is_some_and(|&ix| styles[ix].source.end <= start)
                {
                    heap.pop();
                }
                heap.peek().map(|&ix| &styles[ix])
            });
            let highlight = HighlightStyle {
                color: color.and_then(|s| s.highlight.color),
                font_weight: font_weight.and_then(|s| s.highlight.font_weight),
                font_style: font_style.and_then(|s| s.highlight.font_style),
                background_color: background_color.and_then(|s| s.highlight.background_color),
                underline: underline.and_then(|s| s.highlight.underline),
                strikethrough: strikethrough.and_then(|s| s.highlight.strikethrough),
                fade_out: fade_out.and_then(|s| s.highlight.fade_out),
            };
            result.push(Self {
                source: start..next[0].0,
                highlight,
                font_family: font_family.and_then(|s| s.font_family.clone()),
                font_scale: font_scale.and_then(|s| s.font_scale),
                link: link.and_then(|s| s.link.clone()),
                link_hover: link_hover.and_then(|s| s.link_hover),
            });
        }
        result
    }

    /// Rebase a fragment-local style into its composed document.
    pub fn shifted(mut self, offset: usize) -> Self {
        self.source = self.source.start + offset..self.source.end + offset;
        self
    }

    pub fn new(source: Range<usize>, highlight: HighlightStyle) -> Self {
        Self {
            source,
            highlight,
            font_family: None,
            link: None,
            link_hover: None,
            font_scale: None,
        }
    }

    pub fn font_family(mut self, font_family: impl Into<SharedString>) -> Self {
        self.font_family = Some(font_family.into());
        self
    }

    /// Makes the styled text a link to `target`. Pointing at it shows a hand
    /// cursor, and a click that selects nothing emits
    /// [`DocumentEvent::LinkClicked`](super::DocumentEvent::LinkClicked).
    /// The document does not interpret the target.
    pub fn with_link(mut self, target: impl Into<SharedString>) -> Self {
        self.link = Some(target.into());
        self
    }

    /// Reduce the run's size to a percentage of its paragraph font in display-row
    /// mode. The paragraph retains its line height and baseline; source stays text.
    /// Measured-line mode uses the paragraph size, retaining this override for
    /// when display rows are enabled again.
    pub fn font_scale(mut self, percent: u8) -> Self {
        assert!((1..=100).contains(&percent));
        self.font_scale = Some(percent);
        self
    }

    pub fn font_scale_override(&self) -> Option<u8> {
        self.font_scale
    }

    /// Paint-only emphasis for a link under the pointer: color and underline.
    pub fn with_link_hover_color(mut self, color: Hsla) -> Self {
        self.link_hover = Some(color);
        self
    }

    pub fn link_hover_color(&self) -> Option<Hsla> {
        self.link_hover
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub fn highlight(&self) -> HighlightStyle {
        self.highlight
    }

    pub fn font_family_override(&self) -> Option<&SharedString> {
        self.font_family.as_ref()
    }

    pub fn link(&self) -> Option<&SharedString> {
        self.link.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentStyleError {
    UnknownNode,
    OutOfBounds {
        range: Range<usize>,
        source_len: usize,
    },
    InvalidBoundary(Range<usize>),
    Overlap(Range<usize>),
    EditableRange(Range<usize>),
    AtomicRange(Range<usize>),
    ParagraphBoundary(Range<usize>),
}

impl std::fmt::Display for DocumentStyleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownNode => formatter.write_str("style node does not exist"),
            Self::OutOfBounds { range, source_len } => {
                write!(
                    formatter,
                    "style range {range:?} exceeds source length {source_len}"
                )
            }
            Self::InvalidBoundary(range) => {
                write!(
                    formatter,
                    "style range {range:?} is not on a UTF-8 boundary"
                )
            }
            Self::Overlap(range) => write!(formatter, "style range {range:?} overlaps a peer"),
            Self::EditableRange(range) => {
                write!(
                    formatter,
                    "style range {range:?} intersects editable source"
                )
            }
            Self::AtomicRange(range) => {
                write!(formatter, "style range {range:?} intersects atomic source")
            }
            Self::ParagraphBoundary(range) => write!(
                formatter,
                "paragraph style range {range:?} must map to complete display lines"
            ),
        }
    }
}

impl std::error::Error for DocumentStyleError {}

#[cfg(test)]
mod tests;
