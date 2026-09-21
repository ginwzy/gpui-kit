use std::ops::{Range, RangeBounds};

use gpui::Pixels;

/// Stable identity for a cursor within one retained selection set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub(crate) struct CursorId(usize);

impl CursorId {
    pub(crate) fn new(id: usize) -> Self {
        Self(id)
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) struct CursorSelection {
    pub(crate) id: CursorId,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) reversed: bool,
    pub(crate) column_anchor: Option<(Pixels, usize)>,
}

impl CursorSelection {
    pub(crate) fn new(id: CursorId, start: usize, end: usize) -> Self {
        Self {
            id,
            start,
            end,
            reversed: false,
            column_anchor: None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub(crate) fn clear(&mut self) {
        self.start = 0;
        self.end = 0;
    }

    pub(crate) fn contains(&self, offset: usize) -> bool {
        offset >= self.start && offset < self.end
    }

    pub(crate) fn cursor_offset(&self) -> usize {
        if self.reversed { self.start } else { self.end }
    }

    pub(crate) fn place_at(&mut self, offset: usize, column_anchor: Option<(Pixels, usize)>) {
        self.start = offset;
        self.end = offset;
        self.reversed = false;
        self.column_anchor = column_anchor;
    }

    pub(crate) fn is_collapsed(&self) -> bool {
        self.is_empty()
    }
}

impl From<Range<usize>> for CursorSelection {
    fn from(value: Range<usize>) -> Self {
        Self::new(CursorId::default(), value.start, value.end)
    }
}

impl From<CursorSelection> for Range<usize> {
    fn from(value: CursorSelection) -> Self {
        value.start..value.end
    }
}

impl RangeBounds<usize> for CursorSelection {
    fn start_bound(&self) -> std::ops::Bound<&usize> {
        std::ops::Bound::Included(&self.start)
    }

    fn end_bound(&self) -> std::ops::Bound<&usize> {
        std::ops::Bound::Excluded(&self.end)
    }
}

pub(crate) struct Selections {
    selections: Vec<CursorSelection>,
    next_id: usize,
}

impl Selections {
    pub(crate) fn new() -> Self {
        Self {
            selections: vec![CursorSelection::new(CursorId::new(0), 0, 0)],
            next_id: 1,
        }
    }

    /// Returns the active selection.
    pub(crate) fn active(&self) -> &CursorSelection {
        self.selections
            .first()
            .expect("Selections always has at least one selection")
    }

    /// Returns a mutable reference to the active selection.
    pub(crate) fn active_mut(&mut self) -> &mut CursorSelection {
        self.selections
            .first_mut()
            .expect("Selections always has at least one selection")
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &CursorSelection> {
        self.selections.iter()
    }

    /// Returns the number of selections (always `>= 1`).
    pub(crate) fn len(&self) -> usize {
        self.selections.len()
    }

    /// Returns true when there is exactly one selection.
    pub(crate) fn is_single(&self) -> bool {
        self.selections.len() == 1
    }

    /// Generates a new unique cursor id.
    pub(crate) fn generate_id(&mut self) -> CursorId {
        let id = CursorId::new(self.next_id);
        self.next_id += 1;
        id
    }

    /// Adds an additional selection.
    pub(crate) fn add(&mut self, selection: CursorSelection) {
        self.selections.push(selection);
    }

    /// Replaces all selections. Ignores an empty vec to keep the
    /// "always at least one selection" invariant.
    pub(crate) fn replace_all(&mut self, selections: Vec<CursorSelection>) {
        if !selections.is_empty() {
            self.selections = selections;
        }
    }

    /// Removes every selection except the active one (index 0).
    pub(crate) fn remove_all_but_active(&mut self) {
        self.selections.truncate(1);
    }

    /// Merges overlapping selections while preserving active cursor identity.
    pub(crate) fn merge_overlapping(&mut self) {
        if self.selections.len() <= 1 {
            return;
        }

        let active_id = self.active().id;
        self.selections.sort_by_key(|selection| selection.start);

        let mut merged: Vec<CursorSelection> = Vec::with_capacity(self.selections.len());
        for selection in &self.selections {
            if let Some(last) = merged.last_mut() {
                if selection.start <= last.end {
                    let did_merge = selection.start != last.start || selection.end != last.end;
                    last.end = last.end.max(selection.end);
                    if selection.id == active_id {
                        last.id = active_id;
                        last.reversed = selection.reversed;
                    }
                    if did_merge {
                        last.column_anchor = None;
                    }
                    continue;
                }
            }
            merged.push(*selection);
        }

        if let Some(position) = merged
            .iter()
            .position(|selection| selection.id == active_id)
        {
            merged.swap(0, position);
        }

        self.selections = merged;
    }
}

impl Default for Selections {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    #[test]
    fn cursor_offset_respects_selection_direction() {
        let mut selection = CursorSelection::new(CursorId::new(0), 5, 10);
        assert_eq!(selection.cursor_offset(), 10);
        selection.reversed = true;
        assert_eq!(selection.cursor_offset(), 5);
    }

    #[test]
    fn place_at_collapses_and_resets_direction() {
        let mut selection = CursorSelection::new(CursorId::new(0), 5, 10);
        selection.reversed = true;
        selection.place_at(7, Some((px(12.), 3)));
        assert_eq!(selection.start, 7);
        assert_eq!(selection.end, 7);
        assert!(selection.is_collapsed());
        assert!(!selection.reversed);
        assert_eq!(selection.column_anchor, Some((px(12.), 3)));
    }

    #[test]
    fn selection_set_is_never_empty() {
        let selections = Selections::new();
        assert_eq!(selections.len(), 1);
        assert_eq!(selections.active().id, CursorId::new(0));

        let default = Selections::default();
        assert_eq!(default.len(), 1);
    }

    #[test]
    fn active_selection_is_mutable() {
        let mut selections = Selections::new();
        selections.active_mut().place_at(4, None);
        assert_eq!(selections.active().cursor_offset(), 4);
    }

    #[test]
    fn overlapping_selections_merge_and_keep_active_identity() {
        let mut selections = Selections::new();
        let active_id = selections.generate_id();
        let overlapping_id = selections.generate_id();
        let separate_id = selections.generate_id();

        selections.replace_all(vec![
            CursorSelection::new(active_id, 0, 10),
            CursorSelection::new(overlapping_id, 5, 15),
            CursorSelection::new(separate_id, 20, 30),
        ]);
        selections.merge_overlapping();

        assert_eq!(selections.len(), 2);
        assert_eq!(selections.active().id, active_id);
        assert_eq!(
            (selections.active().start, selections.active().end),
            (0, 15)
        );
        let ranges: Vec<_> = selections
            .iter()
            .map(|selection| (selection.start, selection.end))
            .collect();
        assert!(ranges.contains(&(0, 15)));
        assert!(ranges.contains(&(20, 30)));
    }

    #[test]
    fn merging_keeps_active_selection_direction() {
        let mut selections = Selections::new();
        let active_id = selections.generate_id();
        let other_id = selections.generate_id();
        let mut active = CursorSelection::new(active_id, 5, 15);
        active.reversed = true;
        let other = CursorSelection::new(other_id, 0, 10);
        selections.replace_all(vec![active, other]);

        selections.merge_overlapping();

        assert_eq!(selections.active().id, active_id);
        assert!(selections.active().reversed);
    }
}
