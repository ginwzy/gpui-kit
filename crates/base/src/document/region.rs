use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
};

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
    // Buckets retain indices, not copies of the consumer's IDs. Equality is
    // always checked in the bucket, including when distinct IDs hash alike.
    indices: HashMap<u64, Vec<usize>>,
}

impl<I: Eq + Hash> DocumentRegions<I> {
    pub fn new(
        mut regions: Vec<DocumentRegion<I>>,
        source_len: usize,
    ) -> Result<Self, RegionError> {
        let mut ids = HashSet::with_capacity(regions.len());
        for (index, region) in regions.iter().enumerate() {
            if region.range.start > region.range.end {
                return Err(RegionError::InvalidRange { index });
            }
            if region.range.end > source_len {
                return Err(RegionError::OutOfBounds { index, source_len });
            }
            if !ids.insert(&region.id) {
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

        let mut result = Self {
            regions,
            indices: HashMap::new(),
        };
        result.index_from(0);
        Ok(result)
    }

    pub(super) fn index(&self, id: &I) -> Option<usize> {
        self.indices
            .get(&id_hash(id))?
            .iter()
            .copied()
            .find(|&ix| self.regions[ix].id() == id)
    }

    fn index_from(&mut self, start: usize) {
        for (ix, region) in self.regions.iter().enumerate().skip(start) {
            self.indices
                .entry(id_hash(region.id()))
                .or_default()
                .push(ix);
        }
    }

    pub(super) fn splice(
        &mut self,
        range: Range<usize>,
        mut replacement: Self,
        start: usize,
        delta: isize,
    ) {
        for region in &self.regions[range.start..] {
            let hash = id_hash(region.id());
            if let Some(bucket) = self.indices.get_mut(&hash) {
                bucket.retain(|ix| *ix < range.start);
                if bucket.is_empty() {
                    self.indices.remove(&hash);
                }
            }
        }
        for region in &mut replacement.regions {
            region.range = region.range.start + start..region.range.end + start;
        }
        for region in &mut self.regions[range.end..] {
            region.range = region.range.start.checked_add_signed(delta).unwrap()
                ..region.range.end.checked_add_signed(delta).unwrap();
        }
        let first = range.start;
        self.regions.splice(range, replacement.regions);
        self.index_from(first);
    }
}

fn id_hash(id: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

impl<I> DocumentRegions<I> {
    pub fn as_slice(&self) -> &[DocumentRegion<I>] {
        &self.regions
    }

    pub fn into_vec(self) -> Vec<DocumentRegion<I>> {
        self.regions
    }

    /// Moves the end of region `index` and the regions after it by `delta`,
    /// as an edit inside that region does.
    pub(super) fn resize(&mut self, index: usize, delta: isize) {
        let shift = |offset: usize| offset.checked_add_signed(delta).unwrap();
        let region = &mut self.regions[index];
        region.range.end = shift(region.range.end);
        for region in &mut self.regions[index + 1..] {
            region.range = shift(region.range.start)..shift(region.range.end);
        }
    }

    pub(super) fn set_policy(&mut self, index: usize, policy: EditPolicy) {
        self.regions[index].policy = policy;
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

    #[test]
    fn identity_index_checks_collisions_and_follows_splices() {
        use std::hash::{Hash, Hasher};

        #[derive(Debug, PartialEq, Eq)]
        struct Id(u8);
        impl Hash for Id {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0u8.hash(state);
            }
        }
        let mut regions = DocumentRegions::new(
            (0..4)
                .map(|ix| {
                    DocumentRegion::new(Id(ix), ix as usize..ix as usize + 1, EditPolicy::Readonly)
                })
                .collect(),
            4,
        )
        .unwrap();
        let replacement = DocumentRegions::new(
            vec![
                DocumentRegion::new(Id(9), 0..1, EditPolicy::Readonly),
                DocumentRegion::new(Id(8), 1..2, EditPolicy::Readonly),
            ],
            2,
        )
        .unwrap();
        regions.splice(1..2, replacement, 1, 1);
        assert_eq!(regions.index(&Id(1)), None);
        for (ix, id) in [0, 9, 8, 2, 3].into_iter().enumerate() {
            assert_eq!(regions.index(&Id(id)), Some(ix));
            assert_eq!(regions.as_slice()[ix].range(), ix..ix + 1);
        }
        regions.splice(2..5, DocumentRegions::new(Vec::new(), 0).unwrap(), 2, -3);
        assert_eq!(regions.index(&Id(0)), Some(0));
        assert_eq!(regions.index(&Id(9)), Some(1));
        assert_eq!(regions.index(&Id(8)), None);
        assert_eq!(regions.index(&Id(3)), None);
    }
}
