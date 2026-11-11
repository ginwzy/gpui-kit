use std::{hash::Hash, ops::Range};

use gpui::Context;

use super::{
    Affinity, DocumentBlock, DocumentEvent, DocumentModel, DocumentRevision, DocumentSnapshot,
    DocumentState, EditOrigin, EditPolicy, HostTransactionError,
    splice::{Splice, shifted},
};

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Replace the nodes in `first..end` with a locally addressed readonly snapshot.
    /// Equal boundaries insert before a node. `None` denotes the document end.
    /// Projection, styles and anchored blocks must stay inside the replaced nodes;
    /// both seams must be display-line boundaries. Rejection changes no state.
    /// Editable nodes, their undo, marked text and scroll ownership are preserved.
    pub fn apply_host_splice(
        &mut self,
        revision: DocumentRevision,
        first: Option<&I>,
        end: Option<&I>,
        snapshot: DocumentSnapshot<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        if revision != self.revision() {
            return Err(HostTransactionError::StaleRevision);
        }
        let offset = |id: Option<&I>, default| match id {
            Some(id) => self
                .region(id)
                .map(|region| region.range().start)
                .ok_or(HostTransactionError::FragmentBoundary),
            None => Ok(default),
        };
        let source = offset(first, self.model.text.len())?..offset(end, self.model.text.len())?;
        let line_boundary = |source| {
            self.model
                .source_to_display(source, Affinity::Before)
                .is_some_and(|display| {
                    display == 0
                        || self.model.display_text().as_bytes().get(display - 1) == Some(&b'\n')
                })
        };
        if !line_boundary(source.start)
            || (source.end != self.model.text.len() && !line_boundary(source.end))
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        let splice = self.prepare_readonly_splice(revision, first, end, snapshot)?;
        if splice.source.end != self.model.text.len()
            && !splice.candidate.display_text().is_empty()
            && !splice.candidate.display_text().ends_with('\n')
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        self.publish_host_splice(splice, cx);
        Ok(())
    }

    /// Replace complete readonly nodes, including seams inside a display line.
    /// Unlike the line-fragment API, this also lays out the adjoining nodes of
    /// that line. Editable nodes, selection, composition and undo are retained.
    pub fn apply_host_node_splice(
        &mut self,
        revision: DocumentRevision,
        first: Option<&I>,
        end: Option<&I>,
        snapshot: DocumentSnapshot<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        let splice = self.prepare_readonly_splice(revision, first, end, snapshot)?;
        self.publish_host_splice(splice, cx);
        Ok(())
    }

    fn publish_host_splice(&mut self, splice: Splice<I>, cx: &mut Context<Self>) {
        self.commit_splice(splice);
        cx.emit(DocumentEvent::Changed {
            revision: self.model.revision,
            origin: EditOrigin::Host,
        });
        cx.notify();
    }

    fn prepare_readonly_splice(
        &self,
        revision: DocumentRevision,
        first: Option<&I>,
        end: Option<&I>,
        snapshot: DocumentSnapshot<I>,
    ) -> Result<Splice<I>, HostTransactionError> {
        let splice = self.prepare_splice(revision, first, end, snapshot)?;
        if self.model.regions.as_slice()[splice.nodes.clone()]
            .iter()
            .chain(splice.candidate.regions.as_slice())
            .any(|region| region.policy() == EditPolicy::Editable)
            || splice.selection.is_some()
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        Ok(splice)
    }
}

/// What a host edit replacing source `edit` with `len` bytes leaves of the
/// presentation around it, when only the display lines it touches can lay out
/// differently: no block or anchored block is near, and every region, span and
/// style is unchanged before the edit and moved with it after it.
pub(super) struct HostEditPresentation<'a, I> {
    pub(super) edit: Range<usize>,
    pub(super) len: usize,
    pub(super) regions: &'a [super::DocumentRegion<I>],
    pub(super) projection: &'a super::DocumentProjection,
    pub(super) blocks: &'a [DocumentBlock<I>],
    pub(super) anchored_blocks: &'a [super::AnchoredBlock<I>],
    pub(super) styles: &'a super::DocumentStyles,
}

impl<I: Eq> HostEditPresentation<'_, I> {
    pub(super) fn keeps_lines_apart(&self, model: &DocumentModel<I>) -> bool {
        let edit = &self.edit;
        let delta = self.len as isize - edit.len() as isize;
        let new_edit = edit.start..edit.start + self.len;
        let touches = |source: Range<usize>, edit: &Range<usize>| {
            source.start <= edit.end && edit.start <= source.end
        };
        // An offset before the edit stays, one after it moves, one inside
        // stays inside.
        let moved = |old: usize, new: usize| {
            if old < edit.start {
                new == old
            } else if old > edit.end {
                new as isize == old as isize + delta
            } else {
                new_edit.start <= new && new <= new_edit.end
            }
        };
        let old_regions = model.regions.as_slice();
        if old_regions.len() != self.regions.len()
            || !old_regions.iter().zip(self.regions).all(|(old, new)| {
                old.id() == new.id()
                    && old.policy() == new.policy()
                    && moved(old.range().start, new.range().start)
                    && moved(old.range().end, new.range().end)
                    // An empty editable region lays out a line of its own.
                    && !(old.policy() == EditPolicy::Editable
                        && (old.range().is_empty() || new.range().is_empty())
                        && touches(new.range(), &new_edit))
            })
        {
            return false;
        }
        if !model.anchored_blocks.is_empty()
            || !self.anchored_blocks.is_empty()
            || model
                .blocks
                .iter()
                .any(|block| touches(block.source(), edit))
            || self
                .blocks
                .iter()
                .any(|block| touches(block.source(), &new_edit))
            || !moved_with_edit(
                &model.blocks,
                self.blocks,
                edit,
                &new_edit,
                |old, new, shift| {
                    old.id() == new.id() && shifted(old.source(), shift) == new.source()
                },
                DocumentBlock::source,
            )
        {
            return false;
        }
        moved_with_edit(
            model.projection.spans(),
            self.projection.spans(),
            edit,
            &new_edit,
            |old, new, shift| {
                let mut old = old.clone();
                old.shift_source(shift);
                old == *new
            },
            super::super::ProjectionSpan::source,
        ) && moved_with_edit(
            model.styles.paragraphs(),
            self.styles.paragraphs(),
            edit,
            &new_edit,
            |old, new, shift| {
                shifted(old.source(), shift) == new.source()
                    && old.text_style() == new.text_style()
                    && old.hanging_marker() == new.hanging_marker()
            },
            super::DocumentParagraphStyle::source,
        ) && moved_with_edit(
            model.styles.inline(),
            self.styles.inline(),
            edit,
            &new_edit,
            |old, new, shift| {
                shifted(old.source(), shift) == new.source()
                    && old.highlight() == new.highlight()
                    && old.font_family_override() == new.font_family_override()
                    && old.link() == new.link()
            },
            super::DocumentInlineStyle::source,
        )
    }
}

/// Whether ordered, nonoverlapping `new` is `old` after an edit replaced
/// `edit` with `new_edit`: items before the edit are the same, items after it
/// moved, and items inside it may differ. An item across the edit fails.
fn moved_with_edit<T>(
    old: &[T],
    new: &[T],
    edit: &Range<usize>,
    new_edit: &Range<usize>,
    same: impl Fn(&T, &T, isize) -> bool,
    source: impl Fn(&T) -> Range<usize>,
) -> bool {
    let delta = new_edit.len() as isize - edit.len() as isize;
    let split = |items: &[T], edit: &Range<usize>| {
        let before = items.partition_point(|item| source(item).end < edit.start);
        let after = items.partition_point(|item| source(item).start <= edit.end);
        let inside = items[before..after].iter().all(|item| {
            let source = source(item);
            edit.start <= source.start && source.end <= edit.end
        });
        inside.then_some((before, after))
    };
    let (Some((old_before, old_after)), Some((new_before, new_after))) =
        (split(old, edit), split(new, new_edit))
    else {
        return false;
    };
    old_before == new_before
        && old.len() - old_after == new.len() - new_after
        && old[..old_before]
            .iter()
            .zip(&new[..new_before])
            .all(|(old, new)| same(old, new, 0))
        && old[old_after..]
            .iter()
            .zip(&new[new_after..])
            .all(|(old, new)| same(old, new, delta))
}
