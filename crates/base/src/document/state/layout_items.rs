//! Persistent display-row descriptors. A wrap attaches a prepared subtree;
//! unrelated rows are never copied to make room for a long line.
use std::{
    ops::{Index, Range},
    sync::Arc,
};
use sum_tree::{Bias, SumTree};

use super::DocumentLayoutItem;

#[derive(Clone, Default, Debug)]
pub(super) struct Count(usize);

impl sum_tree::ContextLessSummary for Count {
    fn zero() -> Self {
        Self(0)
    }
    fn add_summary(&mut self, other: &Self) {
        self.0 += other.0;
    }
}

impl<'a> sum_tree::Dimension<'a, Count> for usize {
    fn zero(_: ()) -> Self {
        0
    }
    fn add_summary(&mut self, summary: &'a Count, _: ()) {
        *self += summary.0;
    }
}

// Small independently owned arrays also bound retirement: releasing a slice
// cannot unexpectedly destroy an entire document-sized backing allocation.
const CHUNK_ROWS: usize = 256;
struct Row<I> {
    items: Arc<[DocumentLayoutItem<I>]>,
    range: Range<usize>,
}
impl<I> Clone for Row<I> {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            range: self.range.clone(),
        }
    }
}
impl<I> Row<I> {
    fn new(items: Vec<DocumentLayoutItem<I>>) -> Self {
        Self {
            range: 0..items.len(),
            items: items.into(),
        }
    }
    fn items(&self) -> &[DocumentLayoutItem<I>] {
        &self.items[self.range.clone()]
    }
}
impl<I> sum_tree::Item for Row<I> {
    type Summary = Count;
    fn summary(&self, _: ()) -> Count {
        Count(self.range.len())
    }
}

pub(super) struct LayoutItems<I>(SumTree<Row<I>>);

impl<I> Clone for LayoutItems<I> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<I: Clone> Default for LayoutItems<I> {
    fn default() -> Self {
        Self(SumTree::default())
    }
}

impl<I: Clone> From<Vec<DocumentLayoutItem<I>>> for LayoutItems<I> {
    fn from(items: Vec<DocumentLayoutItem<I>>) -> Self {
        let mut tree = Self::default();
        tree.extend(items);
        tree
    }
}

impl<I: Clone> LayoutItems<I> {
    pub fn len(&self) -> usize {
        self.0.summary().0
    }
    pub fn get(&self, ix: usize) -> Option<&DocumentLayoutItem<I>> {
        let (start, _, row) = self.0.find::<usize, _>((), &ix, Bias::Right);
        row.map(|row| &row.items()[ix - start])
    }
    pub fn last(&self) -> Option<&DocumentLayoutItem<I>> {
        self.0.last().and_then(|row| row.items().last())
    }
    pub fn iter(&self) -> impl Iterator<Item = &DocumentLayoutItem<I>> {
        self.0.iter().flat_map(Row::items)
    }
    pub fn range(
        &self,
        range: Range<usize>,
    ) -> impl DoubleEndedIterator<Item = &DocumentLayoutItem<I>> + ExactSizeIterator {
        range.map(|ix| &self[ix])
    }
    pub fn partition_point(
        &self,
        mut predicate: impl FnMut(&DocumentLayoutItem<I>) -> bool,
    ) -> usize {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if predicate(&self[mid]) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
    pub fn extend(&mut self, items: impl IntoIterator<Item = DocumentLayoutItem<I>>) {
        let mut items = items.into_iter();
        loop {
            let chunk: Vec<_> = items.by_ref().take(CHUNK_ROWS).collect();
            if chunk.is_empty() {
                break;
            }
            self.0.push(Row::new(chunk), ());
        }
    }
    pub fn splice(
        &mut self,
        range: Range<usize>,
        items: impl IntoIterator<Item = DocumentLayoutItem<I>>,
    ) -> Self {
        let mut prepared = Self::default();
        prepared.extend(items);
        self.splice_prepared(range, prepared)
    }
    fn split_at(&self, index: usize) -> (Self, Self) {
        let mut cursor = self.0.cursor::<usize>(());
        let mut before = cursor.slice(&index, Bias::Right);
        let mut after = SumTree::default();
        let cut = index - cursor.start();
        if cut > 0 {
            let chunk = cursor.item().expect("split inside a row chunk");
            before.push(
                Row {
                    items: chunk.items.clone(),
                    range: chunk.range.start..chunk.range.start + cut,
                },
                (),
            );
            after.push(
                Row {
                    items: chunk.items.clone(),
                    range: chunk.range.start + cut..chunk.range.end,
                },
                (),
            );
            cursor.next();
        }
        after.append(cursor.suffix(), ());
        (Self(before), Self(after))
    }
    pub fn splice_prepared(&mut self, range: Range<usize>, items: Self) -> Self {
        let (Self(mut next), rest) = self.split_at(range.start);
        let (removed, tail) = rest.split_at(range.len());
        next.append(items.0, ());
        next.append(tail.0, ());
        self.0 = next;
        removed
    }
    pub fn update(&mut self, ix: usize, update: impl FnOnce(&mut DocumentLayoutItem<I>)) {
        let mut item = self[ix].clone();
        update(&mut item);
        self.splice(ix..ix + 1, [item]);
    }
    pub fn map_range(
        &mut self,
        range: Range<usize>,
        mut update: impl FnMut(&mut DocumentLayoutItem<I>),
    ) {
        let (_, rest) = self.split_at(range.start);
        let (old, _) = rest.split_at(range.len());
        let mut new = Self::default();
        new.extend(old.iter().cloned().map(|mut item| {
            update(&mut item);
            item
        }));
        self.splice_prepared(range, new);
    }
    pub fn retire(&mut self, count: usize) {
        self.splice(0..count.min(self.len()), []);
    }
}

impl<I: Clone> Index<usize> for LayoutItems<I> {
    type Output = DocumentLayoutItem<I>;
    fn index(&self, ix: usize) -> &Self::Output {
        self.get(ix).expect("document row index")
    }
}

impl<I: Clone + std::fmt::Debug> std::fmt::Debug for LayoutItems<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
impl<I: Clone + PartialEq> PartialEq for LayoutItems<I> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<I: Clone + PartialEq> PartialEq<Vec<DocumentLayoutItem<I>>> for LayoutItems<I> {
    fn eq(&self, other: &Vec<DocumentLayoutItem<I>>) -> bool {
        self.iter().eq(other.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn chunk_splices_preserve_rows_at_every_boundary() {
        let row = |id| DocumentLayoutItem::Block {
            id,
            source: id..id + 1,
            height: super::super::Derived::default(),
        };
        let mut expected: Vec<_> = (0..1030).map(row).collect();
        let mut actual: LayoutItems<_> = expected.clone().into();
        let mut seed = 41usize;
        for step in 0..500 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let start = seed % (expected.len() + 1);
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let end = start + seed % (expected.len() - start + 1);
            let inserted = (0..step % 17)
                .map(|n| row(2000 + step * 17 + n))
                .collect::<Vec<_>>();
            expected.splice(start..end, inserted.clone());
            actual.splice(start..end, inserted);
            assert_eq!(actual, expected, "splice {step} {start}..{end}");
        }
    }

    #[test]
    fn attaching_geometry_shares_rows_without_cloning_domain_ids() {
        struct Id(Arc<AtomicUsize>);
        impl Clone for Id {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, Ordering::Relaxed);
                Self(self.0.clone())
            }
        }
        let clones = Arc::new(AtomicUsize::new(0));
        let row = || DocumentLayoutItem::Block {
            id: Id(clones.clone()),
            source: 0..0,
            height: super::super::Derived::default(),
        };
        let mut live: LayoutItems<_> = vec![row(), row()].into();
        let prepared: LayoutItems<_> = (0..100_000).map(|_| row()).collect::<Vec<_>>().into();
        let removed = live.splice_prepared(1..1, prepared);
        assert_eq!(removed.len(), 0);
        assert_eq!(live.len(), 100_002);
        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }
}
