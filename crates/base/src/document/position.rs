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
    SourceOffsetOutOfBounds { offset: usize, source_len: usize },
    NodeNotFound,
    NodeOffsetOutOfBounds { offset: usize, node_len: usize },
}

impl fmt::Display for PositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

use std::{error::Error, fmt};
