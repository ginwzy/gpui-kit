use std::hash::Hash;

use gpui::Context;
use sum_tree::Bias;

use crate::input::RopeExt as _;

use super::{
    DocumentEvent, DocumentModel, DocumentPosition, DocumentRegion, DocumentRevision,
    DocumentSnapshot, DocumentState, DocumentStructure, EditOrigin, EditPolicy,
    HostTransactionError, PresentationError, StructureError, StructureReplacement, UndoKind,
    UndoRecord, splice::Splice,
};

impl<I: Clone + Eq + Hash> DocumentStructure<I> {
    fn suffix(model: &DocumentModel<I>, start: usize) -> Option<Self> {
        if start > model.text.len() || model.text.clip_offset(start, Bias::Left) != start {
            return None;
        }
        let first = model
            .regions
            .as_slice()
            .partition_point(|region| region.range().start < start);
        if first > 0 && model.regions.as_slice()[first - 1].range().end > start {
            return None;
        }
        Self::capture(
            model,
            start,
            model.regions.as_slice()[first..].iter().filter(|region| {
                // Empty readonly nodes at the boundary belong to the host
                // prefix, which acceptance may remove before user undo.
                !(region.range().start == start
                    && region.range().is_empty()
                    && region.policy() == EditPolicy::Readonly)
            }),
        )
    }

    fn node_suffix(model: &DocumentModel<I>, first: usize) -> Option<Self> {
        let regions = model.regions.as_slice().get(first..)?;
        if regions
            .iter()
            .any(|region| region.policy() == EditPolicy::Routed)
        {
            return None;
        }
        let start = regions.first()?.range().start;
        Self::capture(model, start, regions.iter())
    }

    fn capture<'a>(
        model: &DocumentModel<I>,
        start: usize,
        regions: impl Iterator<Item = &'a DocumentRegion<I>>,
    ) -> Option<Self>
    where
        I: 'a,
    {
        let regions = regions
            .filter(|region| {
                matches!(
                    region.policy(),
                    EditPolicy::Editable | EditPolicy::Atomic | EditPolicy::Readonly
                )
            })
            .map(|region| {
                let range = region.range();
                DocumentRegion::new(
                    region.id().clone(),
                    range.start - start..range.end - start,
                    region.policy(),
                )
            })
            .collect::<Vec<_>>();
        let mut end = 0;
        for region in &regions {
            let range = region.range();
            if range.start != end {
                return None;
            }
            end = range.end;
        }
        (!regions.is_empty() && end == model.text.len() - start).then(|| Self {
            text: model.text.slice(start..model.text.len()).to_string(),
            regions,
        })
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Replace a consumer-owned suffix as one user edit. Its inverse stays in
    /// the same stack as text edits; the consumer supplies current presentation
    /// when replay is requested, so host-owned history is never snapshotted.
    pub fn apply_structure_transaction(
        &mut self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let selection_before = self.selected_positions().ok();
        let (next, before, after, selection_after) =
            self.prepare_structure(snapshot, start, None, None)?;
        self.undo.record(
            UndoRecord {
                sequence: 0,
                kind: UndoKind::Structure { before, after },
                selection_before,
                selection_after: selection_after.clone(),
            },
            EditOrigin::User,
        );
        self.commit_structure(next, start, selection_after, EditOrigin::User, cx);
        Ok(())
    }

    /// Complete the top pending structure replay only after its full next
    /// presentation has been validated. Failure leaves both stacks unchanged.
    pub fn apply_structure_replay(
        &mut self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        undo: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let record = if undo {
            self.undo.undo.last()
        } else {
            self.undo.redo.last()
        };
        let Some(UndoRecord {
            kind: UndoKind::Structure { before, after },
            ..
        }) = record
        else {
            return Err(StructureError::StaleReplay);
        };
        let (current, target) = if undo {
            (after, before)
        } else {
            (before, after)
        };
        let (next, _, _, selection) =
            self.prepare_structure(snapshot, start, Some(current), Some(target))?;
        let (source_stack, target_stack, origin) = if undo {
            (&mut self.undo.undo, &mut self.undo.redo, EditOrigin::Undo)
        } else {
            (&mut self.undo.redo, &mut self.undo.undo, EditOrigin::Redo)
        };
        let record = source_stack
            .pop()
            .expect("validated replay record is present");
        target_stack.push(record);
        self.undo.break_coalescing();
        self.commit_structure(next, start, selection, origin, cx);
        Ok(())
    }

    /// IDs referenced by reversible suffix edits, for consumer-owned object
    /// snapshots which should be discarded when undo records are evicted.
    pub fn retained_structure_ids(&self) -> Vec<I> {
        self.undo
            .undo
            .iter()
            .chain(&self.undo.redo)
            .filter_map(|record| match &record.kind {
                UndoKind::Structure { before, after } => {
                    Some(before.regions.iter().chain(&after.regions))
                }
                UndoKind::Text { .. } => None,
            })
            .flatten()
            .map(|region| region.id().clone())
            .collect()
    }

    fn prepare_structure(
        &self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        expected_current: Option<&DocumentStructure<I>>,
        expected_target: Option<&DocumentStructure<I>>,
    ) -> Result<
        (
            DocumentModel<I>,
            DocumentStructure<I>,
            DocumentStructure<I>,
            Option<(DocumentPosition<I>, DocumentPosition<I>)>,
        ),
        StructureError,
    > {
        if self.model.marked.is_some() {
            return Err(StructureError::MarkedText);
        }
        self.validate_inline_layout(&snapshot.blocks)
            .map_err(StructureError::Presentation)?;
        let before =
            DocumentStructure::suffix(&self.model, start).ok_or(StructureError::InvalidBoundary)?;
        if expected_current.is_some_and(|expected| expected != &before) {
            return Err(StructureError::StaleReplay);
        }
        let DocumentSnapshot {
            text,
            regions,
            projection,
            blocks,
            anchored_blocks,
            styles,
            selection,
        } = snapshot;
        if text
            .get(..start)
            .is_none_or(|prefix| self.model.text.slice(..start) != prefix)
        {
            return Err(StructureError::PrefixChanged);
        }
        let mut next = DocumentModel::new(text, regions)
            .map_err(|error| StructureError::Presentation(PresentationError::Regions(error)))?;
        if self
            .model
            .regions
            .as_slice()
            .iter()
            .filter(|region| region.range().start < start)
            .ne(next
                .regions
                .as_slice()
                .iter()
                .filter(|region| region.range().start < start))
        {
            return Err(StructureError::PrefixChanged);
        }
        next.set_rich_presentation_with_anchors(projection, blocks, anchored_blocks, styles)
            .map_err(StructureError::Presentation)?;
        let after =
            DocumentStructure::suffix(&next, start).ok_or(StructureError::InvalidBoundary)?;
        if expected_target.is_some_and(|expected| expected != &after) {
            return Err(StructureError::StaleReplay);
        }
        let selection = selection.or_else(|| self.selected_positions().ok());
        if let Some((anchor, head)) = &selection {
            for position in [anchor, head] {
                next.resolve_position(position).map_err(|error| {
                    StructureError::Presentation(PresentationError::Position(error))
                })?;
            }
        }
        next.revision = self.model.revision.next();
        Ok((next, before, after, selection))
    }

    fn commit_structure(
        &mut self,
        mut next: DocumentModel<I>,
        start: usize,
        selection: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
        origin: EditOrigin,
        cx: &mut Context<Self>,
    ) {
        self.settle_list_scroll();
        let first_item = self.layout_item_for_source(start);
        next.anchors = self.model.anchors.clone();
        next.selection = self.model.selection;
        next.anchors
            .apply_edit(&(start..self.model.text.len()), next.text.len() - start);
        next.reconcile_anchor_nodes();
        if let Some((anchor, head)) = selection {
            next.replace_selection_positions(anchor, head)
                .expect("validated structure selection resolves");
        }
        self.model = next;
        self.reconcile_layout_items_from(first_item);
        self.finish_user_edit(origin, cx);
    }

    /// Replace an editor-owned suffix with a locally addressed snapshot.
    /// The retained prefix is neither copied nor reparsed. Validation completes
    /// before the model, geometry, selection or undo stack changes.
    pub fn apply_structure_splice(
        &mut self,
        revision: DocumentRevision,
        first: &I,
        snapshot: DocumentSnapshot<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let selection_before = self.selected_positions().ok();
        let (splice, before, after) = self.prepare_structure_splice(revision, first, snapshot)?;
        self.undo.record(
            UndoRecord {
                sequence: 0,
                kind: UndoKind::Structure { before, after },
                selection_before,
                selection_after: splice.selection.clone(),
            },
            EditOrigin::User,
        );
        self.commit_splice(splice);
        self.finish_user_edit(EditOrigin::User, cx);
        Ok(())
    }

    /// Replay a structure undo using only the current suffix's presentation.
    pub fn apply_structure_splice_replay(
        &mut self,
        revision: DocumentRevision,
        first: &I,
        snapshot: DocumentSnapshot<I>,
        undo: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let (splice, current, target) = self.prepare_structure_splice(revision, first, snapshot)?;
        let record = if undo {
            self.undo.undo.last()
        } else {
            self.undo.redo.last()
        };
        let Some(UndoRecord {
            kind: UndoKind::Structure { before, after },
            ..
        }) = record
        else {
            return Err(StructureError::StaleReplay);
        };
        let (expected_current, expected_target) = if undo {
            (after, before)
        } else {
            (before, after)
        };
        if &current != expected_current || &target != expected_target {
            return Err(StructureError::StaleReplay);
        }
        let (source, target, origin) = if undo {
            (&mut self.undo.undo, &mut self.undo.redo, EditOrigin::Undo)
        } else {
            (&mut self.undo.redo, &mut self.undo.undo, EditOrigin::Redo)
        };
        target.push(source.pop().expect("validated structure replay exists"));
        self.undo.break_coalescing();
        self.commit_splice(splice);
        self.finish_user_edit(origin, cx);
        Ok(())
    }

    /// Complete a routed inline-object replacement without reading the prefix.
    pub fn apply_structure_splice_replacement(
        &mut self,
        request: &StructureReplacement<I>,
        first: &I,
        snapshot: DocumentSnapshot<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        if request.transaction().revision() != self.revision()
            || self.inline_replacement(request.transaction()).as_ref() != Some(request)
        {
            return Err(StructureError::StaleReplay);
        }
        let start = self
            .region(first)
            .ok_or(StructureError::InvalidBoundary)?
            .range()
            .start;
        let edit = &request.transaction().edits()[0];
        if edit.range().start < start {
            return Err(StructureError::InvalidBoundary);
        }
        let mut expected = self
            .model
            .text
            .slice(start..self.model.text.len())
            .to_string();
        expected.replace_range(
            edit.range().start - start..edit.range().end - start,
            edit.replacement(),
        );
        if expected != snapshot.text {
            return Err(StructureError::PrefixChanged);
        }
        self.apply_structure_splice(self.revision(), first, snapshot, cx)
    }

    fn prepare_structure_splice(
        &self,
        revision: DocumentRevision,
        first: &I,
        mut snapshot: DocumentSnapshot<I>,
    ) -> Result<(Splice<I>, DocumentStructure<I>, DocumentStructure<I>), StructureError> {
        if self.model.marked.is_some() {
            return Err(StructureError::MarkedText);
        }
        if revision != self.revision() {
            return Err(StructureError::StaleReplay);
        }
        let first_ix = self
            .model
            .region_index(first)
            .ok_or(StructureError::InvalidBoundary)?;
        let before = DocumentStructure::node_suffix(&self.model, first_ix)
            .ok_or(StructureError::InvalidBoundary)?;
        snapshot.selection = snapshot
            .selection
            .or_else(|| self.selected_positions().ok());
        let splice = self
            .prepare_splice(revision, Some(first), None, snapshot)
            .map_err(|error| match error {
                HostTransactionError::InvalidFragment(error) => StructureError::Presentation(error),
                HostTransactionError::InvalidBlocks(error) => {
                    StructureError::Presentation(super::PresentationError::Blocks(error))
                }
                HostTransactionError::StaleRevision => StructureError::StaleReplay,
                _ => StructureError::InvalidBoundary,
            })?;
        let after = DocumentStructure::node_suffix(&splice.candidate, 0)
            .ok_or(StructureError::InvalidBoundary)?;
        Ok((splice, before, after))
    }

    /// Change permissions on named nodes without rebuilding their presentation.
    /// Empty nodes also update the local caret row they may own.
    pub fn set_node_policies(
        &mut self,
        revision: DocumentRevision,
        policies: &[(I, EditPolicy)],
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        if revision != self.revision() {
            return Err(StructureError::StaleReplay);
        }
        let mut changes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (id, policy) in policies {
            let ix = self
                .model
                .region_index(id)
                .ok_or(StructureError::InvalidBoundary)?;
            if !seen.insert(ix) {
                return Err(StructureError::InvalidBoundary);
            }
            let old = &self.model.regions.as_slice()[ix];
            if old.policy() == *policy {
                continue;
            }
            if !matches!(
                (old.policy(), policy),
                (EditPolicy::Editable, EditPolicy::Readonly)
                    | (EditPolicy::Readonly, EditPolicy::Editable)
            ) {
                return Err(StructureError::PrefixChanged);
            }
            changes.push((ix, *policy, old.range().is_empty()));
        }
        changes.sort_by_key(|(ix, _, _)| *ix);
        for (ix, policy, empty) in changes.into_iter().rev() {
            self.model.regions.set_policy(ix, policy);
            if empty {
                self.relayout_edited_node(ix, self.model.is_layout_seam(ix + 1), 0, 0, usize::MAX);
            }
        }
        self.undo.break_coalescing();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests;
