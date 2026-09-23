use std::{error::Error, fmt};

use super::{Affinity, DocumentPosition, PositionError};

/// A rendered row attached to a source position without occupying source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchoredBlock<I> {
    id: I,
    position: DocumentPosition<I>,
}

impl<I> AnchoredBlock<I> {
    pub fn new(id: I, position: DocumentPosition<I>) -> Self {
        Self { id, position }
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn position(&self) -> &DocumentPosition<I> {
        &self.position
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchoredBlockError {
    MissingRenderer,
    DuplicateId,
    InvalidPosition(PositionError),
    EditableRegion,
    NotLineBoundary,
}

impl fmt::Display for AnchoredBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRenderer => formatter.write_str("anchored block renderer is not set"),
            Self::DuplicateId => formatter.write_str("anchored block ID is not unique"),
            Self::InvalidPosition(error) => error.fmt(formatter),
            Self::EditableRegion => {
                formatter.write_str("anchored block requires a readonly or routed region")
            }
            Self::NotLineBoundary => {
                formatter.write_str("anchored block must follow a display line")
            }
        }
    }
}

impl Error for AnchoredBlockError {}

pub(super) fn display_offset<I>(
    block: &AnchoredBlock<I>,
    resolve: impl FnOnce(&DocumentPosition<I>) -> Result<usize, PositionError>,
    project: impl FnOnce(usize, Affinity) -> Option<usize>,
    display: &str,
) -> Result<(usize, usize), AnchoredBlockError> {
    let source = resolve(block.position()).map_err(AnchoredBlockError::InvalidPosition)?;
    let offset = project(source, block.position().affinity()).ok_or(
        AnchoredBlockError::InvalidPosition(PositionError::NodeNotFound),
    )?;
    if !display.is_char_boundary(offset)
        || (offset != 0 && offset != display.len() && !display[..offset].ends_with('\n'))
    {
        return Err(AnchoredBlockError::NotLineBoundary);
    }
    Ok((source, offset))
}
