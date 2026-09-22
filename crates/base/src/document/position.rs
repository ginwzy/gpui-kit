use std::{error::Error, fmt, ops::Range};

/// Which side of an ambiguous visual or structural boundary a position uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Affinity {
    #[default]
    Before,
    After,
}

/// How a runtime anchor moves when text is inserted at its exact offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AnchorBias {
    #[default]
    Left,
    Right,
}

/// A domain-stable position inside one document node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentPosition<N> {
    node_id: N,
    offset: usize,
    affinity: Affinity,
}

impl<N> DocumentPosition<N> {
    pub fn new(node_id: N, offset: usize, affinity: Affinity) -> Self {
        Self {
            node_id,
            offset,
            affinity,
        }
    }

    pub fn node_id(&self) -> &N {
        &self.node_id
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn affinity(&self) -> Affinity {
        self.affinity
    }

    pub fn into_node_id(self) -> N {
        self.node_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionError {
    InvalidBoundary { offset: usize },
    SourceOffsetOutOfBounds { offset: usize, source_len: usize },
    NodeNotFound,
    NodeOffsetOutOfBounds { offset: usize, node_len: usize },
}

impl fmt::Display for PositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundary { offset } => {
                write!(formatter, "offset {offset} is not on a UTF-8 boundary")
            }
            Self::SourceOffsetOutOfBounds { offset, source_len } => write!(
                formatter,
                "source offset {offset} exceeds document length {source_len}"
            ),
            Self::NodeNotFound => formatter.write_str("document node no longer exists"),
            Self::NodeOffsetOutOfBounds { offset, node_len } => {
                write!(
                    formatter,
                    "node offset {offset} exceeds node length {node_len}"
                )
            }
        }
    }
}

impl Error for PositionError {}

/// Monotonic revision of a document's logical source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentRevision(u64);

impl DocumentRevision {
    pub const INITIAL: Self = Self(0);

    pub fn value(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Opaque identity for an offset that follows document transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentAnchor(u64);

impl DocumentAnchor {
    pub(crate) fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Directional selection expressed with runtime anchors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentSelection {
    anchor: DocumentAnchor,
    head: DocumentAnchor,
}

impl DocumentSelection {
    pub fn new(anchor: DocumentAnchor, head: DocumentAnchor) -> Self {
        Self { anchor, head }
    }

    pub fn anchor(self) -> DocumentAnchor {
        self.anchor
    }

    pub fn head(self) -> DocumentAnchor {
        self.head
    }
}

pub(super) fn transform_offset(
    offset: usize,
    bias: AnchorBias,
    range: &Range<usize>,
    replacement_len: usize,
) -> usize {
    if range.is_empty() {
        return if offset < range.start {
            offset
        } else if offset > range.start {
            shift_offset(offset, replacement_len as isize)
        } else if bias == AnchorBias::Right {
            range.start + replacement_len
        } else {
            range.start
        };
    }

    if offset < range.start {
        offset
    } else if offset > range.end {
        shift_offset(offset, replacement_len as isize - range.len() as isize)
    } else if offset == range.end {
        range.start + replacement_len
    } else if bias == AnchorBias::Right {
        range.start + replacement_len
    } else {
        range.start
    }
}

pub(super) fn shift_offset(offset: usize, delta: isize) -> usize {
    if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs())
    }
}

// Descriptors outside editable source use right-biased starts and left-biased
// ends, so insertion at either edge cannot swallow neighboring text.
pub(super) fn transform_source_range(
    mut range: Range<usize>,
    edits: &[super::TextEdit],
) -> Range<usize> {
    for edit in edits {
        let was_empty = range.is_empty();
        range.start = transform_offset(
            range.start,
            AnchorBias::Right,
            &edit.range(),
            edit.replacement().len(),
        );
        range.end = if was_empty {
            range.start
        } else {
            transform_offset(
                range.end,
                AnchorBias::Left,
                &edit.range(),
                edit.replacement().len(),
            )
        };
    }
    range
}
