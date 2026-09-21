use std::{error::Error, fmt, ops::Range};

use super::DocumentRevision;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditOrigin {
    User,
    Composition,
    Paste,
    Undo,
    Redo,
    Host,
}

/// One replacement expressed in the source coordinates before a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    range: Range<usize>,
    replacement: String,
}

impl TextEdit {
    pub fn new(range: Range<usize>, replacement: impl Into<String>) -> Self {
        Self {
            range,
            replacement: replacement.into(),
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }
}

/// A normalized group of disjoint edits against one source revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditTransaction {
    revision: DocumentRevision,
    origin: EditOrigin,
    edits: Vec<TextEdit>,
}

impl EditTransaction {
    pub fn new(
        revision: DocumentRevision,
        origin: EditOrigin,
        mut edits: Vec<TextEdit>,
        source_len: usize,
    ) -> Result<Self, TransactionError> {
        if edits.is_empty() {
            return Err(TransactionError::Empty);
        }

        for (index, edit) in edits.iter().enumerate() {
            if edit.range.start > edit.range.end {
                return Err(TransactionError::InvalidRange { index });
            }
            if edit.range.end > source_len {
                return Err(TransactionError::OutOfBounds { index, source_len });
            }
        }

        edits.sort_by(|left, right| {
            right
                .range
                .start
                .cmp(&left.range.start)
                .then_with(|| right.range.end.cmp(&left.range.end))
        });

        for (index, pair) in edits.windows(2).enumerate() {
            let upper = &pair[0].range;
            let lower = &pair[1].range;
            let duplicate_insert =
                upper.is_empty() && lower.is_empty() && upper.start == lower.start;
            if lower.end > upper.start || duplicate_insert {
                return Err(TransactionError::Overlap {
                    first: index,
                    second: index + 1,
                });
            }
        }

        Ok(Self {
            revision,
            origin,
            edits,
        })
    }

    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn origin(&self) -> EditOrigin {
        self.origin
    }

    /// Edits in application order, from highest source offset to lowest.
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
}

/// Result of pre-mutation routing for an edit request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditDecision<R> {
    Apply,
    Reject,
    Route(R),
    MaterializeAndApply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionError {
    Empty,
    InvalidRange { index: usize },
    OutOfBounds { index: usize, source_len: usize },
    Overlap { first: usize, second: usize },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("transaction has no edits"),
            Self::InvalidRange { index } => {
                write!(formatter, "edit {index} has an invalid range")
            }
            Self::OutOfBounds { index, source_len } => {
                write!(formatter, "edit {index} exceeds source length {source_len}")
            }
            Self::Overlap { first, second } => {
                write!(formatter, "edits {first} and {second} overlap")
            }
        }
    }
}

impl Error for TransactionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_sorts_edits_into_application_order() {
        let transaction = EditTransaction::new(
            DocumentRevision::INITIAL,
            EditOrigin::User,
            vec![TextEdit::new(1..2, "a"), TextEdit::new(8..9, "b")],
            10,
        )
        .unwrap();

        assert_eq!(transaction.edits()[0].range(), 8..9);
        assert_eq!(transaction.edits()[1].range(), 1..2);
    }

    #[test]
    fn transaction_rejects_overlapping_edits() {
        let error = EditTransaction::new(
            DocumentRevision::INITIAL,
            EditOrigin::User,
            vec![TextEdit::new(2..6, "a"), TextEdit::new(5..8, "b")],
            10,
        )
        .unwrap_err();

        assert_eq!(
            error,
            TransactionError::Overlap {
                first: 0,
                second: 1
            }
        );
    }

    #[test]
    fn transaction_rejects_duplicate_insertions() {
        let error = EditTransaction::new(
            DocumentRevision::INITIAL,
            EditOrigin::Paste,
            vec![TextEdit::new(3..3, "a"), TextEdit::new(3..3, "b")],
            3,
        )
        .unwrap_err();

        assert!(matches!(error, TransactionError::Overlap { .. }));
    }
}
