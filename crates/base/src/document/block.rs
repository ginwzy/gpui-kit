use std::{error::Error, fmt, ops::Range};

use super::{DocumentProjection, DocumentRegions, EditPolicy};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentBlock<I> {
    id: I,
    source: Range<usize>,
}

impl<I> DocumentBlock<I> {
    pub fn new(id: I, source: Range<usize>) -> Self {
        Self { id, source }
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    MissingRenderer,
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

        let Some(region) = regions
            .as_slice()
            .iter()
            .find(|region| region.range() == source && region.policy() == EditPolicy::Atomic)
        else {
            return Err(BlockError::MissingAtomicRegion(source));
        };
        if region.id() != block.id() {
            return Err(BlockError::RegionIdMismatch(source));
        }
        if !projection
            .spans()
            .iter()
            .any(|span| span.source() == source && span.replacement().is_empty())
        {
            return Err(BlockError::VisibleSource(source));
        }
    }
    Ok(())
}

pub(crate) fn transform_blocks<I: Clone>(
    blocks: &[DocumentBlock<I>],
    edits: &[super::TextEdit],
) -> Vec<DocumentBlock<I>> {
    blocks
        .iter()
        .map(|block| {
            let mut source = block.source();
            for edit in edits {
                let edit_range = edit.range();
                source.start =
                    transform_offset(source.start, &edit_range, edit.replacement().len());
                source.end = transform_offset(source.end, &edit_range, edit.replacement().len());
            }
            DocumentBlock::new(block.id().clone(), source)
        })
        .collect()
}

fn transform_offset(offset: usize, range: &Range<usize>, replacement_len: usize) -> usize {
    if offset <= range.start {
        offset
    } else if offset >= range.end {
        if replacement_len >= range.len() {
            offset.saturating_add(replacement_len - range.len())
        } else {
            offset.saturating_sub(range.len() - replacement_len)
        }
    } else {
        range.start + replacement_len
    }
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
}
