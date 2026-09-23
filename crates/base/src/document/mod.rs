//! Editing contracts for mixed readonly and editable documents.
//!
//! The document module owns text interaction and source coordinates. Product
//! concepts are represented by consumer-provided node and region identifiers.

mod anchored_block;
mod block;
mod element;
mod position;
mod projection;
mod region;
mod state;
mod style;
mod transaction;

pub use anchored_block::{AnchoredBlock, AnchoredBlockError};
pub use block::{BlockError, DocumentBlock};
pub use element::DocumentElement;
pub use position::{
    Affinity, AnchorBias, DocumentAnchor, DocumentPosition, DocumentRevision, DocumentSelection,
    PositionError,
};
pub use projection::{DocumentProjection, ProjectionError, ProjectionMapping, ProjectionSpan};
pub use region::{DocumentRegion, DocumentRegions, EditPolicy, RegionError};
pub use state::{
    DocumentEditRejection, DocumentEvent, DocumentSnapshot, DocumentState, DocumentStructure,
    DocumentTextContext, HostTransactionError, PresentationError, RoutedEdit, ScrollPinError,
    StructureError, StructureReplay, UndoCheckpoint,
};
pub use style::{DocumentInlineStyle, DocumentParagraphStyle, DocumentStyleError, DocumentStyles};
pub use transaction::{EditDecision, EditOrigin, EditTransaction, TextEdit, TransactionError};
