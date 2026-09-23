use std::ops::Range;

use gpui::Context;

use crate::input::RopeExt as _;

use super::{
    Affinity, BlockError, DocumentBlock, DocumentEvent, DocumentLayoutItem, DocumentModel,
    DocumentRevision, DocumentSnapshot, DocumentState, EditOrigin, EditPolicy,
    HostTransactionError, PresentationError, RegionError, ResolvedDocumentStyles, TextEdit,
};

impl<I: Clone + Eq + 'static> DocumentState<I> {
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
        if revision != self.model.revision {
            return Err(HostTransactionError::StaleRevision);
        }
        let boundary = |id: Option<&I>| match id {
            Some(id) => self
                .model
                .region_index(id)
                .ok_or(HostTransactionError::FragmentBoundary),
            None => Ok(self.model.regions.as_slice().len()),
        };
        let nodes = boundary(first)?..boundary(end)?;
        let regions = self.model.regions.as_slice();
        if nodes.start > nodes.end
            || regions[nodes.clone()]
                .iter()
                .any(|region| region.policy() == EditPolicy::Editable)
            || snapshot
                .regions
                .iter()
                .any(|region| region.policy() == EditPolicy::Editable)
            || snapshot.selection.is_some()
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        let source_at = |ix: usize| {
            regions
                .get(ix)
                .map_or(self.model.text.len(), |region| region.range().start)
        };
        let source = source_at(nodes.start)..source_at(nodes.end);
        let display = self
            .model
            .source_to_display(source.start, Affinity::Before)
            .unwrap()
            ..self
                .model
                .source_to_display(source.end, Affinity::Before)
                .unwrap();
        let crosses = |range: Range<usize>| {
            [source.start, source.end]
                .iter()
                .any(|offset| range.start < *offset && *offset < range.end)
        };
        let line_boundary = |offset: usize| {
            offset == 0 || self.model.display_text().as_bytes().get(offset - 1) == Some(&b'\n')
        };
        if !line_boundary(display.start)
            || (source.end != self.model.text.len() && !line_boundary(display.end))
            || self
                .model
                .projection
                .spans()
                .iter()
                .any(|span| crosses(span.source()))
            || self
                .model
                .styles
                .paragraphs()
                .iter()
                .any(|style| crosses(style.source()))
            || self
                .model
                .styles
                .inline()
                .iter()
                .any(|style| crosses(style.source()))
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        if self.block_renderer.is_none()
            && (!snapshot.blocks.is_empty() || !snapshot.anchored_blocks.is_empty())
        {
            return Err(HostTransactionError::InvalidBlocks(
                BlockError::MissingRenderer,
            ));
        }
        let removed = |id: &I| {
            regions[nodes.clone()]
                .iter()
                .any(|region| region.id() == id)
        };
        let mut candidate =
            DocumentModel::new(snapshot.text, snapshot.regions).map_err(|error| {
                HostTransactionError::InvalidFragment(PresentationError::Regions(error))
            })?;
        candidate
            .set_rich_presentation_with_anchors(
                snapshot.projection,
                snapshot.blocks,
                snapshot.anchored_blocks,
                snapshot.styles,
            )
            .map_err(HostTransactionError::InvalidFragment)?;
        let retained_ids = regions[..nodes.start]
            .iter()
            .chain(&regions[nodes.end..])
            .map(|region| region.id())
            .chain(
                self.model
                    .anchored_blocks
                    .iter()
                    .filter(|block| !removed(block.position().node_id()))
                    .map(|block| block.id()),
            );
        for id in retained_ids {
            if candidate.region_index(id).is_some()
                || candidate
                    .anchored_blocks
                    .iter()
                    .any(|block| block.id() == id)
            {
                return Err(HostTransactionError::InvalidFragment(
                    PresentationError::Regions(RegionError::DuplicateId { index: nodes.start }),
                ));
            }
        }
        if source.end != self.model.text.len()
            && !candidate.display_text().is_empty()
            && !candidate.display_text().ends_with('\n')
        {
            return Err(HostTransactionError::FragmentBoundary);
        }
        let old_layout = self.layout_boundary(nodes.start)..self.layout_boundary(nodes.end);
        let mut replacement = candidate.layout_items();
        // The following retained node owns the caret row at the fragment end.
        if nodes.end != regions.len()
            && matches!(replacement.last(), Some(DocumentLayoutItem::Text { display, .. }) if display.is_empty())
        {
            replacement.pop();
        }
        for item in &mut replacement {
            item.shift(source.start as isize, display.start as isize);
        }
        let source_delta = candidate.text.len() as isize - source.len() as isize;
        let display_delta = candidate.display_text().len() as isize - display.len() as isize;
        let old_text = self.model.text.slice(source.clone()).to_string();
        let new_text = candidate.text();
        let edit = fragment_edit(source.start, &old_text, &new_text);
        let selection = self.model.capture_active_selection_in_editable_region();
        let marked = self.model.capture_marked_in_editable_region();
        let retained_anchors = self
            .model
            .anchored_blocks
            .iter()
            .filter(|block| !removed(block.position().node_id()))
            .cloned()
            .collect::<Vec<_>>();

        // All fallible work is complete. Publish source, presentation and layout together.
        self.capture_viewport_anchor();
        if let Some(edit) = edit {
            self.model
                .anchors
                .apply_edit(&edit.range(), edit.replacement().len());
            self.model.text.replace(edit.range(), edit.replacement());
        }
        self.model
            .regions
            .splice(nodes, candidate.regions, source.start, source_delta);
        self.model
            .projection
            .splice(source.clone(), candidate.projection);
        self.model
            .projection_map
            .splice(source.clone(), display.clone(), candidate.projection_map);
        let block_start = self
            .model
            .blocks
            .partition_point(|block| block.source().end <= source.start);
        let block_end = self
            .model
            .blocks
            .partition_point(|block| block.source().start < source.end);
        for block in &mut self.model.blocks[block_end..] {
            *block = DocumentBlock::new(block.id().clone(), shifted(block.source(), source_delta));
        }
        self.model.blocks.splice(
            block_start..block_end,
            candidate.blocks.into_iter().map(|block| {
                DocumentBlock::new(
                    block.id().clone(),
                    shifted(block.source(), source.start as isize),
                )
            }),
        );
        self.model.anchored_blocks = retained_anchors;
        self.model.anchored_blocks.extend(candidate.anchored_blocks);
        self.model
            .styles
            .splice(source, new_text.len(), candidate.styles);
        self.model
            .resolved_styles
            .splice(display, display_delta, candidate.resolved_styles);
        self.model.reconcile_anchor_nodes();
        self.model.revision = self.model.revision.next();
        for item in &mut self.layout_items[old_layout.end..] {
            item.shift(source_delta, display_delta);
        }
        // Keep matching measurements inside the changed fragment as well.
        let old_items = &self.layout_items[old_layout.clone()];
        let prefix = old_items
            .iter()
            .zip(&replacement)
            .take_while(|(old, new)| old.same_layout_identity(new))
            .count();
        let suffix = old_items[prefix..]
            .iter()
            .rev()
            .zip(replacement[prefix..].iter().rev())
            .take_while(|(old, new)| old.same_layout_identity(new))
            .count();
        let changed = old_layout.start + prefix..old_layout.end - suffix;
        let count = replacement.len() - prefix - suffix;
        if !changed.is_empty() || count != 0 {
            self.list_state.splice(changed, count);
            self.list_state.clone().measure_all();
        }
        self.layout_items.splice(old_layout, replacement);
        if let Some(selection) = &selection {
            self.model.restore_active_selection(selection);
        }
        self.model.restore_marked(marked.as_ref());
        cx.emit(DocumentEvent::Changed {
            revision: self.model.revision,
            origin: EditOrigin::Host,
        });
        cx.notify();
        Ok(())
    }

    fn layout_boundary(&self, region_ix: usize) -> usize {
        let regions = self.model.regions.as_slice();
        let source = regions
            .get(region_ix)
            .map_or(self.model.text.len(), |region| region.range().start);
        let mut ix = self
            .layout_items
            .partition_point(|item| item.source_start() < source);
        while let Some(item) = self.layout_items.get(ix) {
            let node = match item {
                DocumentLayoutItem::Text { node_id, .. } => node_id.as_ref(),
                DocumentLayoutItem::Block { id, .. } => Some(id),
                DocumentLayoutItem::AnchoredBlock { id, .. } => self
                    .model
                    .anchored_blocks
                    .iter()
                    .find(|block| block.id() == id)
                    .map(|block| block.position().node_id()),
                DocumentLayoutItem::Trailer => None,
            };
            if node
                .and_then(|id| self.model.region_index(id))
                .is_some_and(|owner| owner < region_ix)
            {
                ix += 1;
            } else {
                break;
            }
        }
        ix
    }
}

fn shifted(range: Range<usize>, delta: isize) -> Range<usize> {
    range.start.checked_add_signed(delta).unwrap()..range.end.checked_add_signed(delta).unwrap()
}

impl<I> DocumentLayoutItem<I> {
    fn source_start(&self) -> usize {
        match self {
            Self::Text { source, .. } | Self::Block { source, .. } => source.start,
            Self::AnchoredBlock { source, .. } => *source,
            Self::Trailer => usize::MAX,
        }
    }

    fn shift(&mut self, source_delta: isize, display_delta: isize) {
        match self {
            Self::Text {
                source, display, ..
            } => {
                *source = shifted(source.clone(), source_delta);
                *display = shifted(display.clone(), display_delta);
            }
            Self::Block { source, .. } => *source = shifted(source.clone(), source_delta),
            Self::AnchoredBlock { source, .. } => {
                *source = source.checked_add_signed(source_delta).unwrap()
            }
            Self::Trailer => {}
        }
    }
}

impl ResolvedDocumentStyles {
    fn splice(&mut self, range: Range<usize>, delta: isize, mut replacement: Self) {
        let from = self
            .paragraphs
            .partition_point(|(display, _)| display.end <= range.start);
        let to = self
            .paragraphs
            .partition_point(|(display, _)| display.start < range.end);
        for (display, _) in &mut replacement.paragraphs {
            *display = shifted(display.clone(), range.start as isize);
        }
        for (display, _) in &mut self.paragraphs[to..] {
            *display = shifted(display.clone(), delta);
        }
        self.paragraphs.splice(from..to, replacement.paragraphs);
        let from = self
            .inline
            .partition_point(|style| style.display.end <= range.start);
        let to = self
            .inline
            .partition_point(|style| style.display.start < range.end);
        for style in &mut replacement.inline {
            style.display = shifted(style.display.clone(), range.start as isize);
        }
        for style in &mut self.inline[to..] {
            style.display = shifted(style.display.clone(), delta);
        }
        self.inline.splice(from..to, replacement.inline);
    }
}

fn fragment_edit(start: usize, old: &str, new: &str) -> Option<TextEdit> {
    if old == new {
        return None;
    }
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    Some(TextEdit::new(
        start + prefix..start + old.len() - suffix,
        &new[prefix..new.len() - suffix],
    ))
}
