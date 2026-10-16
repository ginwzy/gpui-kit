//! Persistent display-row descriptors. A wrap attaches a prepared subtree;
//! unrelated rows are never copied to make room for a long line.
use std::{
    collections::VecDeque,
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
    /// Items from `start` on, in order. Each step is amortized O(1), unlike
    /// indexing, which descends the tree for every item.
    pub fn iter_from(&self, start: usize) -> impl Iterator<Item = &DocumentLayoutItem<I>> {
        let mut cursor = self.0.cursor::<usize>(());
        cursor.seek(&start, Bias::Right);
        let skip = start.saturating_sub(*cursor.start());
        cursor
            .enumerate()
            .flat_map(move |(ix, row)| &row.items()[if ix == 0 { skip } else { 0 }..])
    }
    pub fn range(
        &self,
        range: Range<usize>,
    ) -> impl DoubleEndedIterator<Item = &DocumentLayoutItem<I>> + ExactSizeIterator {
        // Collect the chunk slices once; iteration then never descends the tree.
        let mut slices = VecDeque::new();
        let mut at = range.start;
        if at < range.end {
            let mut cursor = self.0.cursor::<usize>(());
            cursor.seek(&at, Bias::Right);
            while at < range.end {
                let Some(row) = cursor.item() else {
                    break;
                };
                let offset = at - *cursor.start();
                let take = (row.range.len() - offset).min(range.end - at);
                slices.push_back(&row.items()[offset..offset + take]);
                at += take;
                cursor.next();
            }
        }
        debug_assert_eq!(
            at,
            range.end.max(range.start),
            "layout item range in bounds"
        );
        RangeItems {
            len: at - range.start.min(at),
            slices,
        }
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
        let mut update = Some(update);
        self.map_range(ix..ix + 1, |item| {
            if let Some(update) = update.take() {
                update(item);
            }
        });
    }
    /// Updates items `range` in place. The whole chunks that hold them are
    /// taken out, so no chunk is split; a chunk no other tree shares changes
    /// where it is, and only shared chunks are copied. Updating rows thus
    /// clones neither their domain IDs nor their styles.
    pub fn map_range(
        &mut self,
        range: Range<usize>,
        mut update: impl FnMut(&mut DocumentLayoutItem<I>),
    ) {
        if range.is_empty() {
            return;
        }
        let chunk_start = |ix: usize| {
            let mut cursor = self.0.cursor::<usize>(());
            cursor.seek(&ix, Bias::Right);
            (
                *cursor.start(),
                cursor.item().map_or(0, |row| row.range.len()),
            )
        };
        let (lo, _) = chunk_start(range.start);
        let (last, last_len) = chunk_start(range.end - 1);
        let hi = last + last_len;
        let tree = std::mem::take(self);
        let (before, rest) = tree.split_at(lo);
        let (middle, after) = rest.split_at(hi - lo);
        // Release every other reference to the middle's chunks first.
        drop((tree, rest));
        let rows: Vec<Row<I>> = middle.0.iter().cloned().collect();
        drop(middle);
        let mut changed = SumTree::default();
        let mut at = lo;
        for mut row in rows {
            let len = row.range.len();
            // The part of this chunk inside `range`, relative to the chunk.
            let from = range.start.max(at) - at;
            let to = range.end.min(at + len).max(at) - at;
            at += len;
            if let Some(items) = Arc::get_mut(&mut row.items) {
                let offset = row.range.start;
                items[offset + from..offset + to]
                    .iter_mut()
                    .for_each(&mut update);
                changed.push(row, ());
            } else {
                let mut items = row.items().to_vec();
                items[from..to.max(from)].iter_mut().for_each(&mut update);
                changed.push(Row::new(items), ());
            }
        }
        let Self(mut next) = before;
        next.append(changed, ());
        next.append(after.0, ());
        self.0 = next;
    }
    pub fn retire(&mut self, count: usize) {
        self.splice(0..count.min(self.len()), []);
    }
}

/// The items of a range, read from the chunk slices that hold them.
struct RangeItems<'a, I> {
    slices: VecDeque<&'a [DocumentLayoutItem<I>]>,
    len: usize,
}

impl<'a, I> Iterator for RangeItems<'a, I> {
    type Item = &'a DocumentLayoutItem<I>;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(slice) = self.slices.front_mut() {
            if let Some((item, rest)) = slice.split_first() {
                *slice = rest;
                self.len -= 1;
                return Some(item);
            }
            self.slices.pop_front();
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len, Some(self.len))
    }
}

impl<I> DoubleEndedIterator for RangeItems<'_, I> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(slice) = self.slices.back_mut() {
            if let Some((item, rest)) = slice.split_last() {
                *slice = rest;
                self.len -= 1;
                return Some(item);
            }
            self.slices.pop_back();
        }
        None
    }
}

impl<I> ExactSizeIterator for RangeItems<'_, I> {}

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
            let (from, to) = (start.min(expected.len()), end.min(expected.len()));
            assert!(actual.range(from..to).eq(&expected[from..to]));
            assert!(
                actual
                    .range(from..to)
                    .rev()
                    .eq(expected[from..to].iter().rev())
            );
            assert!(actual.iter_from(from).eq(&expected[from..]));
            let mark = |item: &mut DocumentLayoutItem<usize>| {
                if let DocumentLayoutItem::Block { source, .. } = item {
                    *source = step..step + 1;
                }
            };
            actual.map_range(from..to, mark);
            expected[from..to].iter_mut().for_each(mark);
            assert_eq!(actual, expected, "map {step} {from}..{to}");
        }
    }

    #[test]
    fn updating_unshared_rows_clones_no_domain_ids() {
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
        let moved = |item: &mut DocumentLayoutItem<Id>| {
            if let DocumentLayoutItem::Block { source, .. } = item {
                *source = 1..2;
            }
        };
        let mut live: LayoutItems<_> = (0..4 * CHUNK_ROWS)
            .map(|_| row())
            .collect::<Vec<_>>()
            .into();
        live.map_range(CHUNK_ROWS..3 * CHUNK_ROWS, moved);
        live.update(3 * CHUNK_ROWS + 1, moved);
        assert_eq!(clones.load(Ordering::Relaxed), 0);
        let source = |items: &LayoutItems<Id>, ix: usize| match &items[ix] {
            DocumentLayoutItem::Block { source, .. } => source.clone(),
            _ => unreachable!(),
        };
        assert_eq!(source(&live, CHUNK_ROWS), 1..2);
        assert_eq!(source(&live, 3 * CHUNK_ROWS + 1), 1..2);
        assert_eq!(source(&live, 3 * CHUNK_ROWS + 2), 0..0);
        // A tree that still shares the rows keeps them as they were.
        let shared = live.clone();
        live.map_range(0..CHUNK_ROWS, moved);
        assert_eq!(clones.load(Ordering::Relaxed), CHUNK_ROWS);
        assert_eq!(source(&shared, 0), 0..0);
        assert_eq!(source(&live, 0), 1..2);
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
