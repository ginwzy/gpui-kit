use std::{error::Error, fmt, ops::Range};

use super::{DocumentProjection, DocumentRegions, EditPolicy, InlineSize, inline::OBJECT};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentBlock<I> {
    id: I,
    source: Range<usize>,
    inline_size: Option<InlineSize>,
    replaceable: bool,
}

impl<I> DocumentBlock<I> {
    pub fn new(id: I, source: Range<usize>) -> Self {
        Self {
            id,
            source,
            inline_size: None,
            replaceable: false,
        }
    }

    /// Opt into an indivisible inline object in `TextRows` layout. Its entire
    /// source range must project to one U+FFFC, instead of the hidden source
    /// required by a default block. The renderer must fit this declared box.
    pub fn with_inline_size(mut self, size: InlineSize) -> Self {
        self.inline_size = Some(size);
        self
    }

    pub fn inline_size(&self) -> Option<InlineSize> {
        self.inline_size
    }

    /// Allow range replacement requests for this inline object. Defaults to
    /// false; ordinary atomic blocks always retain their existing protection.
    /// The host must handle `DocumentEvent::ReplaceRequested` and commit a
    /// validated structure transaction before any source is changed.
    pub fn replaceable(mut self, replaceable: bool) -> Self {
        self.replaceable = replaceable;
        self
    }

    pub fn is_replaceable(&self) -> bool {
        self.inline_size.is_some() && self.replaceable
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    /// Moves the block by `delta`, as an edit before it does.
    pub(crate) fn shift(&mut self, delta: isize) {
        let shift = |offset: usize| offset.checked_add_signed(delta).unwrap();
        self.source = shift(self.source.start)..shift(self.source.end);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    MissingRenderer,
    InlineRequiresTextRows,
    InvalidInlineSize,
    InlineProjection(Range<usize>),
    InvalidRange(Range<usize>),
    OutOfBounds {
        range: Range<usize>,
        source_len: usize,
    },
    Overlap(Range<usize>),
    MissingAtomicRegion(Range<usize>),
    RegionIdMismatch(Range<usize>),
    VisibleSource(Range<usize>),
}

impl fmt::Display for BlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRenderer => formatter.write_str("document block renderer is not set"),
            Self::InlineRequiresTextRows => {
                formatter.write_str("inline objects require TextRows layout")
            }
            Self::InvalidInlineSize => {
                formatter.write_str("inline object dimensions must be finite and nonnegative")
            }
            Self::InlineProjection(range) => write!(
                formatter,
                "inline object {range:?} must project to one U+FFFC"
            ),
            Self::InvalidRange(range) => write!(formatter, "invalid block range {range:?}"),
            Self::OutOfBounds { range, source_len } => {
                write!(
                    formatter,
                    "block range {range:?} exceeds source length {source_len}"
                )
            }
            Self::Overlap(range) => write!(formatter, "block range {range:?} overlaps another"),
            Self::MissingAtomicRegion(range) => {
                write!(
                    formatter,
                    "block range {range:?} has no matching atomic region"
                )
            }
            Self::RegionIdMismatch(range) => {
                write!(
                    formatter,
                    "block ID differs from atomic region ID at {range:?}"
                )
            }
            Self::VisibleSource(range) => {
                write!(
                    formatter,
                    "block source {range:?} is not hidden by projection"
                )
            }
        }
    }
}

impl Error for BlockError {}

pub(crate) fn validate_blocks<I: Eq>(
    blocks: &[DocumentBlock<I>],
    regions: &DocumentRegions<I>,
    projection: &DocumentProjection,
    source_len: usize,
) -> Result<(), BlockError> {
    let mut previous_end = 0;
    for block in blocks {
        let source = block.source();
        if source.start >= source.end {
            return Err(BlockError::InvalidRange(source));
        }
        if source.end > source_len {
            return Err(BlockError::OutOfBounds {
                range: source,
                source_len,
            });
        }
        if source.start < previous_end {
            return Err(BlockError::Overlap(source));
        }
        previous_end = source.end;

        let candidates = regions.as_slice();
        let from = candidates.partition_point(|region| region.range().start < source.start);
        let Some(region) = candidates[from..]
            .iter()
            .take_while(|region| region.range().start == source.start)
            .find(|region| region.range() == source && region.policy() == EditPolicy::Atomic)
        else {
            return Err(BlockError::MissingAtomicRegion(source));
        };
        if region.id() != block.id() {
            return Err(BlockError::RegionIdMismatch(source));
        }
        let spans = projection.spans();
        let from = spans.partition_point(|span| span.source().start < source.start);
        let replacement = spans[from..]
            .iter()
            .take_while(|span| span.source().start == source.start)
            .find(|span| span.source() == source)
            .map(|span| span.replacement());
        if let Some(size) = block.inline_size {
            if !size.is_valid() {
                return Err(BlockError::InvalidInlineSize);
            }
            if replacement != Some(OBJECT) {
                return Err(BlockError::InlineProjection(source));
            }
        } else if replacement != Some("") {
            return Err(BlockError::VisibleSource(source));
        }
    }
    Ok(())
}

#[cfg(debug_assertions)]
pub(crate) fn transform_blocks<I: Clone>(
    blocks: &[DocumentBlock<I>],
    edits: &[super::TextEdit],
) -> Vec<DocumentBlock<I>> {
    blocks
        .iter()
        .map(|block| {
            let mut next = block.clone();
            next.source = super::position::transform_source_range(block.source(), edits);
            next
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentRegion, ProjectionSpan};

    #[test]
    fn block_requires_one_hidden_atomic_region_with_the_same_id() {
        let regions = DocumentRegions::new(
            vec![DocumentRegion::new("tool", 0..3, EditPolicy::Atomic)],
            3,
        )
        .unwrap();
        let projection = DocumentProjection::new(3, vec![ProjectionSpan::hide(0..3)]).unwrap();

        assert_eq!(
            validate_blocks(
                &[DocumentBlock::new("tool", 0..3)],
                &regions,
                &projection,
                3
            ),
            Ok(())
        );
        assert_eq!(
            validate_blocks(
                &[DocumentBlock::new("other", 0..3)],
                &regions,
                &projection,
                3
            ),
            Err(BlockError::RegionIdMismatch(0..3))
        );
    }

    #[test]
    fn each_block_finds_its_own_region_and_span_among_neighbors() {
        let regions = DocumentRegions::new(
            vec![
                DocumentRegion::new("card", 5..8, EditPolicy::Atomic),
                DocumentRegion::new("lead", 0..0, EditPolicy::Editable),
                DocumentRegion::new("text", 3..5, EditPolicy::Readonly),
                DocumentRegion::new("tool", 0..3, EditPolicy::Atomic),
            ],
            8,
        )
        .unwrap();
        let hidden = DocumentProjection::new(
            8,
            vec![ProjectionSpan::hide(0..3), ProjectionSpan::hide(5..8)],
        )
        .unwrap();
        let blocks = [
            DocumentBlock::new("tool", 0..3),
            DocumentBlock::new("card", 5..8),
        ];

        assert_eq!(validate_blocks(&blocks, &regions, &hidden, 8), Ok(()));
        assert_eq!(
            validate_blocks(&[DocumentBlock::new("text", 3..5)], &regions, &hidden, 8),
            Err(BlockError::MissingAtomicRegion(3..5))
        );
        let replaced = DocumentProjection::new(
            8,
            vec![
                ProjectionSpan::hide(0..3),
                ProjectionSpan::replace(5..8, "card"),
            ],
        )
        .unwrap();
        assert_eq!(
            validate_blocks(&blocks, &regions, &replaced, 8),
            Err(BlockError::VisibleSource(5..8))
        );
    }
}
