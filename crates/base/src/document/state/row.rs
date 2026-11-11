//! A published row is one complete layout. Pending work lives in TextRows or
//! Tables, never in independently replaceable parts of a published row.
use std::sync::Arc;

use super::{
    table::Resolved,
    text_rows::{RowFrame, WrapKey},
};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Layout {
    /// `wrap == None` is a framed, bounded placeholder awaiting line breaks.
    Text {
        frame: RowFrame,
        wrap: Option<WrapKey>,
    },
    Table(Table),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Table {
    frame: RowFrame,
    cells: Arc<Resolved>,
}

impl Layout {
    pub(super) fn table(frame: RowFrame, cells: Arc<Resolved>) -> Self {
        Self::Table(Table {
            frame: frame.with_table_height(cells.geometry.height),
            cells,
        })
    }

    pub(super) fn frame(&self) -> RowFrame {
        match self {
            Self::Text { frame, .. } => *frame,
            Self::Table(table) => table.frame,
        }
    }

    pub(super) fn wrapped(&self) -> Option<WrapKey> {
        match self {
            Self::Text { wrap, .. } => *wrap,
            Self::Table(_) => None,
        }
    }

    pub(super) fn cells(&self) -> Option<&Arc<Resolved>> {
        match self {
            Self::Table(table) => Some(&table.cells),
            Self::Text { .. } => None,
        }
    }
}
