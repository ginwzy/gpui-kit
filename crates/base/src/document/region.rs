use std::{error::Error, fmt, ops::Range};

/// Mutation policy for one source range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditPolicy {
    Editable,
    Readonly,
    Routed,
    Atomic,
}

/// A domain-identified source range and its mutation policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentRegion<I> {
    id: I,
    range: Range<usize>,
    policy: EditPolicy,
}

impl<I> DocumentRegion<I> {
    pub fn new(id: I, range: Range<usize>, policy: EditPolicy) -> Self {
        Self { id, range, policy }
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub fn policy(&self) -> EditPolicy {
        self.policy
    }
}

/// Canonical ordered region collection for one source revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentRegions<I> {
    regions: Vec<DocumentRegion<I>>,
}

impl<I: Eq> DocumentRegions<I> {
    pub fn new(
        mut regions: Vec<DocumentRegion<I>>,
        source_len: usize,
    ) -> Result<Self, RegionError> {
        for (index, region) in regions.iter().enumerate() {
            if region.range.start > region.range.end {
                return Err(RegionError::InvalidRange { index });
            }
            if region.range.end > source_len {
                return Err(RegionError::OutOfBounds { index, source_len });
            }
            if regions[..index].iter().any(|other| other.id == region.id) {
                return Err(RegionError::DuplicateId { index });
            }
        }

        regions.sort_by(|left, right| {
            left.range
                .start
                .cmp(&right.range.start)
                .then_with(|| left.range.end.cmp(&right.range.end))
        });

        for (index, pair) in regions.windows(2).enumerate() {
            if pair[0].range.end > pair[1].range.start {
                return Err(RegionError::Overlap {
                    left: index,
                    right: index + 1,
                });
            }
        }

        Ok(Self { regions })
    }

    pub fn as_slice(&self) -> &[DocumentRegion<I>] {
        &self.regions
    }

    pub fn into_vec(self) -> Vec<DocumentRegion<I>> {
        self.regions
    }

    pub(super) fn splice(
        &mut self,
        range: Range<usize>,
        mut replacement: Self,
        start: usize,
        delta: isize,
    ) {
        for region in &mut replacement.regions {
            region.range = region.range.start + start..region.range.end + start;
        }
        for region in &mut self.regions[range.end..] {
            region.range = region.range.start.checked_add_signed(delta).unwrap()
                ..region.range.end.checked_add_signed(delta).unwrap();
        }
        self.regions.splice(range, replacement.regions);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    InvalidBoundary { index: usize },
    InvalidRange { index: usize },
    OutOfBounds { index: usize, source_len: usize },
    DuplicateId { index: usize },
    Overlap { left: usize, right: usize },
}

impl fmt::Display for RegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundary { index } => {
                write!(formatter, "region {index} is not on UTF-8 boundaries")
            }
            Self::InvalidRange { index } => {
                write!(formatter, "region {index} has an invalid range")
            }
            Self::OutOfBounds { index, source_len } => {
                write!(
                    formatter,
                    "region {index} exceeds source length {source_len}"
                )
            }
            Self::DuplicateId { index } => write!(formatter, "region {index} repeats an id"),
            Self::Overlap { left, right } => {
                write!(formatter, "regions {left} and {right} overlap")
            }
        }
    }
}

impl Error for RegionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_are_sorted_and_allow_a_trailing_empty_draft() {
        let regions = DocumentRegions::new(
            vec![
                DocumentRegion::new("draft", 8..8, EditPolicy::Editable),
                DocumentRegion::new("history", 0..8, EditPolicy::Readonly),
            ],
            8,
        )
        .unwrap();

        assert_eq!(regions.as_slice()[0].id(), &"history");
        assert_eq!(regions.as_slice()[1].id(), &"draft");
    }

    #[test]
    fn readonly_zero_width_anchor_is_allowed_between_regions() {
        let regions = DocumentRegions::new(
            vec![
                DocumentRegion::new("before", 0..4, EditPolicy::Readonly),
                DocumentRegion::new("empty", 4..4, EditPolicy::Readonly),
                DocumentRegion::new("after", 4..8, EditPolicy::Readonly),
                DocumentRegion::new("draft", 8..8, EditPolicy::Editable),
            ],
            8,
        )
        .unwrap();

        assert_eq!(regions.as_slice()[1].id(), &"empty");
    }

    #[test]
    fn overlapping_regions_are_rejected() {
        let error = DocumentRegions::new(
            vec![
                DocumentRegion::new(1, 0..5, EditPolicy::Readonly),
                DocumentRegion::new(2, 4..8, EditPolicy::Editable),
            ],
            8,
        )
        .unwrap_err();

        assert_eq!(error, RegionError::Overlap { left: 0, right: 1 });
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let error = DocumentRegions::new(
            vec![
                DocumentRegion::new(1, 0..4, EditPolicy::Readonly),
                DocumentRegion::new(1, 4..8, EditPolicy::Editable),
            ],
            8,
        )
        .unwrap_err();

        assert_eq!(error, RegionError::DuplicateId { index: 1 });
    }
}
