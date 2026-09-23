use std::ops::Range;

use gpui::{HighlightStyle, SharedString, TextStyleRefinement};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentStyles {
    paragraphs: Vec<DocumentParagraphStyle>,
    inline: Vec<DocumentInlineStyle>,
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

    pub(super) fn transformed(&self, edits: &[super::TextEdit]) -> Self {
        let mut styles = self.clone();
        for style in &mut styles.paragraphs {
            style.source = super::position::transform_source_range(style.source.clone(), edits);
        }
        for style in &mut styles.inline {
            style.source = super::position::transform_source_range(style.source.clone(), edits);
        }
        styles
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentParagraphStyle {
    source: Range<usize>,
    text_style: TextStyleRefinement,
}

impl DocumentParagraphStyle {
    pub fn new(source: Range<usize>, text_style: TextStyleRefinement) -> Self {
        Self { source, text_style }
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
}

impl DocumentInlineStyle {
    pub fn new(source: Range<usize>, highlight: HighlightStyle) -> Self {
        Self {
            source,
            highlight,
            font_family: None,
        }
    }

    pub fn font_family(mut self, font_family: impl Into<SharedString>) -> Self {
        self.font_family = Some(font_family.into());
        self
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentStyleError {
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
