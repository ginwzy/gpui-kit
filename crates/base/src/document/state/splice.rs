use std::{collections::HashSet, hash::Hash, ops::Range};

use crate::input::RopeExt as _;

use super::{
    Affinity, BlockError, DocumentLayoutItem, DocumentModel, DocumentPosition, DocumentRevision,
    DocumentSnapshot, DocumentState, HostTransactionError, PresentationError, RegionError,
    ResolvedDocumentStyles, TextEdit,
};

pub(super) struct Splice<I> {
    pub candidate: DocumentModel<I>,
    pub selection: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
    pub nodes: Range<usize>,
    pub source: Range<usize>,
    display: Range<usize>,
    layout: Range<usize>,
    layout_nodes: Range<usize>,
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    pub(super) fn prepare_splice(
        &self,
        revision: DocumentRevision,
        first: Option<&I>,
        end: Option<&I>,
        snapshot: DocumentSnapshot<I>,
    ) -> Result<Splice<I>, HostTransactionError> {
        if revision != self.model.revision {
            return Err(HostTransactionError::StaleRevision);
        }
        self.validate_inline_layout(&snapshot.blocks)
            .map_err(HostTransactionError::InvalidFragment)?;
        let boundary = |id: Option<&I>| match id {
            Some(id) => self
                .model
                .region_index(id)
                .ok_or(HostTransactionError::FragmentBoundary),
            None => Ok(self.model.regions.as_slice().len()),
        };
        let nodes = boundary(first)?..boundary(end)?;
        let regions = self.model.regions.as_slice();
        if nodes.start > nodes.end {
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
        if [source.start, source.end].into_iter().any(|offset| {
            crosses(self.model.projection.spans(), offset, |span| span.source())
                || crosses(self.model.styles.paragraphs(), offset, |style| {
                    style.source()
                })
                || crosses(self.model.styles.inline(), offset, |style| style.source())
        }) {
            return Err(HostTransactionError::FragmentBoundary);
        }
        if self.block_renderer.is_none()
            && (!snapshot.blocks.is_empty() || !snapshot.anchored_blocks.is_empty())
        {
            return Err(HostTransactionError::InvalidBlocks(
                BlockError::MissingRenderer,
            ));
        }
        let selection = snapshot.selection;
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
        let fragment_ids = candidate
            .regions
            .as_slice()
            .iter()
            .map(|region| region.id())
            .chain(candidate.anchored_blocks.iter().map(|block| block.id()));
        for id in fragment_ids {
            let owner = self.model.region_index(id).or_else(|| {
                self.model.anchored_indices.get(id).and_then(|&ix| {
                    self.model
                        .region_index(self.model.anchored_blocks[ix].position().node_id())
                })
            });
            if owner.is_some_and(|ix| !nodes.contains(&ix)) {
                return Err(HostTransactionError::InvalidFragment(
                    PresentationError::Regions(RegionError::DuplicateId { index: nodes.start }),
                ));
            }
        }
        if let Some((anchor, head)) = &selection {
            for position in [anchor, head] {
                if candidate.region_index(position.node_id()).is_some() {
                    candidate.resolve_position(position)
                } else if self
                    .model
                    .region_index(position.node_id())
                    .is_some_and(|ix| !nodes.contains(&ix))
                {
                    self.model.resolve_position(position)
                } else {
                    return Err(HostTransactionError::FragmentBoundary);
                }
                .map_err(|error| {
                    HostTransactionError::InvalidFragment(PresentationError::Position(error))
                })?;
            }
        }
        self.validate_splice_anchors(&nodes, &source, &display, &candidate)?;
        let mut layout_nodes = nodes.start..(nodes.end + 1).min(regions.len());
        // EOF ceases to be a seam when an insertion continues its last line.
        if layout_nodes.start == regions.len()
            && !self.model.display_text().is_empty()
            && !self.model.display_text().ends_with('\n')
        {
            layout_nodes.start = layout_nodes.start.saturating_sub(1);
        }
        while !self.model.is_layout_seam(layout_nodes.start) {
            layout_nodes.start -= 1;
        }
        while !self.model.is_layout_seam(layout_nodes.end) {
            layout_nodes.end += 1;
        }
        let layout_end = if layout_nodes.end == regions.len() {
            self.layout_items.len()
                - usize::from(matches!(
                    self.layout_items.last(),
                    Some(DocumentLayoutItem::Trailer)
                ))
        } else {
            self.layout_boundary(layout_nodes.end)
        };
        // A window starting at the first node also rebuilds any unowned prefix.
        let layout_start = if layout_nodes.start == 0 {
            0
        } else {
            self.layout_boundary(layout_nodes.start)
        };
        let layout = layout_start..layout_end;
        layout_nodes.end = layout_nodes.end - nodes.len() + candidate.regions.as_slice().len();
        Ok(Splice {
            candidate,
            selection,
            nodes,
            source,
            display,
            layout,
            layout_nodes,
        })
    }

    fn validate_splice_anchors(
        &self,
        nodes: &Range<usize>,
        source: &Range<usize>,
        display: &Range<usize>,
        candidate: &DocumentModel<I>,
    ) -> Result<(), HostTransactionError> {
        if self.model.anchored_blocks.is_empty() && candidate.anchored_blocks.is_empty() {
            return Ok(());
        }
        let source_delta = candidate.text.len() as isize - source.len() as isize;
        let display_delta = candidate.display_text().len() as isize - display.len() as isize;
        let parts = [
            &self.model.display_text()[..display.start],
            candidate.display_text(),
            &self.model.display_text()[display.end..],
        ];
        let length = parts.iter().map(|part| part.len()).sum::<usize>();
        let invalid = || {
            HostTransactionError::InvalidFragment(PresentationError::AnchoredBlocks(
                super::AnchoredBlockError::NotLineBoundary,
            ))
        };
        let validate = |offset: usize, affinity| {
            #[cfg(test)]
            super::structure::tests::record(|work| work.anchor_checks += 1);
            // Read the prospective projection through its three borrowed pieces.
            // Source boundaries belong to the piece on their right, just as in
            // ProjectionMap, including a retained prefix anchor at the seam.
            let offset = if offset < source.start {
                self.model.source_to_display(offset, affinity)
            } else if offset < source.start + candidate.text.len() {
                candidate
                    .source_to_display(offset - source.start, affinity)
                    .map(|offset| display.start + offset)
            } else {
                offset
                    .checked_add_signed(-source_delta)
                    .and_then(|offset| self.model.source_to_display(offset, affinity))
                    .and_then(|offset| offset.checked_add_signed(display_delta))
            }
            .ok_or_else(invalid)?;
            let previous = offset.checked_sub(1).and_then(|mut ix| {
                parts.iter().find_map(|part| {
                    if ix < part.len() {
                        Some(part.as_bytes()[ix])
                    } else {
                        ix -= part.len();
                        None
                    }
                })
            });
            if offset == 0 || offset == length || previous == Some(b'\n') {
                Ok(())
            } else {
                Err(invalid())
            }
        };
        for block in &candidate.anchored_blocks {
            let offset = candidate
                .resolve_position(block.position())
                .map_err(|_| invalid())?;
            validate(source.start + offset, block.position().affinity())?;
        }
        // Only anchors beside the two display seams can lose a line boundary.
        // Use published item order so collapsed source spans and empty nodes
        // at the same display position are included without scanning history.
        let left = self.items_around_display(display.start);
        let mut right = self.items_around_display(display.end);
        right.start = right.start.max(left.end);
        for ix in left.chain(right) {
            let DocumentLayoutItem::AnchoredBlock { id, .. } = &self.layout_items[ix] else {
                continue;
            };
            let block = &self.model.anchored_blocks[self.model.anchored_indices[id]];
            let owner = self.model.region_index(block.position().node_id()).unwrap();
            if nodes.contains(&owner) {
                continue;
            }
            let mut offset = self
                .model
                .resolve_position(block.position())
                .map_err(|_| invalid())?;
            if owner >= nodes.end {
                offset = offset
                    .checked_add_signed(source_delta)
                    .ok_or_else(invalid)?;
            }
            validate(offset, block.position().affinity())?;
        }
        Ok(())
    }

    pub(super) fn commit_splice(&mut self, splice: Splice<I>) {
        self.settle_list_scroll();
        let Splice {
            candidate,
            selection: final_selection,
            nodes,
            source,
            display,
            layout,
            layout_nodes,
        } = splice;
        let source_delta = candidate.text.len() as isize - source.len() as isize;
        let display_delta = candidate.display_text().len() as isize - display.len() as isize;
        let old_text = self.model.text.slice(source.clone()).to_string();
        let new_text = candidate.text();
        let edit = fragment_edit(source.start, &old_text, &new_text);
        let selection = self.model.capture_active_selection_in_editable_region();
        let marked = self.model.capture_marked_in_editable_region();
        let removed: Vec<_> = self.model.regions.as_slice()[nodes.clone()]
            .iter()
            .map(|region| region.id().clone())
            .collect();

        // All fallible work is complete. Publish source, presentation and layout together.
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
            block.shift(source_delta);
        }
        self.model.blocks.splice(
            block_start..block_end,
            candidate.blocks.into_iter().map(|mut block| {
                block.shift(source.start as isize);
                block
            }),
        );
        self.model
            .splice_anchored_blocks(&removed, candidate.anchored_blocks);
        self.model
            .styles
            .splice(source.clone(), new_text.len(), candidate.styles);
        self.model.resolved_styles.splice(
            source,
            source_delta,
            display,
            display_delta,
            candidate.resolved_styles,
        );
        self.model.reconcile_anchor_nodes();
        self.model.revision = self.model.revision.next();
        let replacement = self.model.layout_items_of_nodes(
            layout_nodes,
            self.text_rows.as_ref().map(|rows| &rows.cache),
        );
        self.replace_layout_window(layout, replacement, source_delta, display_delta, usize::MAX);
        if let Some(selection) = &selection {
            self.model.restore_active_selection(selection);
        }
        self.model.restore_marked(marked.as_ref());
        if let Some((anchor, head)) = final_selection {
            self.model
                .replace_selection_positions(anchor, head)
                .expect("validated splice selection resolves");
        }
    }

    pub(super) fn layout_boundary(&self, region_ix: usize) -> usize {
        let regions = self.model.regions.as_slice();
        let source = regions
            .get(region_ix)
            .map_or(self.model.text.len(), |region| region.range().start);
        self.layout_boundary_at(region_ix, source)
    }

    /// The first layout item of node `region_ix` or a later node, among items
    /// that place the node's start at `source`.
    pub(super) fn layout_boundary_at(&self, region_ix: usize, source: usize) -> usize {
        let mut ix = self
            .layout_items
            .partition_point(|item| item.source_start() < source);
        while let Some(item) = self.layout_items.get(ix) {
            let node = match item {
                DocumentLayoutItem::Text { node_id, .. } => node_id.as_ref(),
                DocumentLayoutItem::Block { id, .. } => Some(id),
                DocumentLayoutItem::AnchoredBlock { id, .. } => self
                    .model
                    .anchored_indices
                    .get(id)
                    .map(|&ix| &self.model.anchored_blocks[ix])
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

fn crosses<T>(items: &[T], offset: usize, range: impl Fn(&T) -> Range<usize>) -> bool {
    let ix = items.partition_point(|item| range(item).start < offset);
    ix > 0 && range(&items[ix - 1]).end > offset
}

pub(super) fn shifted(range: Range<usize>, delta: isize) -> Range<usize> {
    range.start.checked_add_signed(delta).unwrap()..range.end.checked_add_signed(delta).unwrap()
}

impl<I> DocumentLayoutItem<I> {
    /// The node a text or block item belongs to.
    pub(super) fn node(&self) -> Option<&I> {
        match self {
            Self::Text { node_id, .. } => node_id.as_ref(),
            Self::Block { id, .. } | Self::AnchoredBlock { id, .. } => Some(id),
            Self::Trailer => None,
        }
    }

    /// Drops the complete layout derived for this item. Tables can recover
    /// compatible cell measurements from their input-keyed cache at prepare.
    pub(super) fn forget_geometry(&mut self) {
        match self {
            Self::Text { layout, .. } => {
                layout.0 = None;
            }
            Self::Block { height, .. } | Self::AnchoredBlock { height, .. } => {
                height.0 = Default::default()
            }
            Self::Trailer => {}
        }
    }

    /// Takes the geometry layout derived for `old`, an item of the same identity.
    pub(super) fn keep_geometry(&mut self, old: &Self) {
        match (self, old) {
            (Self::Text { layout, .. }, Self::Text { layout: old, .. }) => {
                *layout = old.clone();
            }
            (
                Self::Block { height, .. } | Self::AnchoredBlock { height, .. },
                Self::Block { height: old, .. } | Self::AnchoredBlock { height: old, .. },
            ) => *height = *old,
            _ => {}
        }
    }

    pub(super) fn source_start(&self) -> usize {
        match self {
            Self::Text { source, .. } | Self::Block { source, .. } => source.start,
            Self::AnchoredBlock { source, .. } => *source,
            Self::Trailer => usize::MAX,
        }
    }

    pub(super) fn source_end(&self) -> usize {
        match self {
            Self::Text { source, .. } | Self::Block { source, .. } => source.end,
            Self::AnchoredBlock { source, .. } => *source,
            Self::Trailer => usize::MAX,
        }
    }

    pub(super) fn shift(&mut self, source_delta: isize, display_delta: isize) {
        match self {
            Self::Text {
                source,
                display,
                line,
                ..
            } => {
                *source = shifted(source.clone(), source_delta);
                *display = shifted(display.clone(), display_delta);
                *line = shifted(line.clone(), display_delta);
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
    fn splice(
        &mut self,
        source: Range<usize>,
        source_delta: isize,
        range: Range<usize>,
        delta: isize,
        mut replacement: Self,
    ) {
        let from = self
            .paragraphs
            .partition_point(|style| style.display.end <= range.start);
        let to = self
            .paragraphs
            .partition_point(|style| style.display.start < range.end);
        for style in &mut replacement.paragraphs {
            style.shift(source.start as isize, range.start as isize);
        }
        for style in &mut self.paragraphs[to..] {
            style.shift(source_delta, delta);
        }
        self.paragraphs.splice(from..to, replacement.paragraphs);
        let from = self
            .inline
            .partition_point(|style| style.source.end <= source.start);
        let to = self
            .inline
            .partition_point(|style| style.source.start < source.end);
        for style in &mut replacement.inline {
            style.source = shifted(style.source.clone(), source.start as isize);
            style.display = shifted(style.display.clone(), range.start as isize);
        }
        for style in &mut self.inline[to..] {
            style.source = shifted(style.source.clone(), source_delta);
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

/// Retained items keep the geometry derived for them, but a line's frame can
/// depend on where it sits in its node. `items[changed]` replaced `replaced`;
/// retained lines of the nodes involved may have moved within them, so their
/// geometry is derived again.
pub(super) fn forget_touched_geometry<I: Clone + Eq + Hash>(
    replaced: &[DocumentLayoutItem<I>],
    items: &mut [DocumentLayoutItem<I>],
    changed: Range<usize>,
) {
    let (head, rest) = items.split_at_mut(changed.start);
    let (changed, tail) = rest.split_at_mut(changed.len());
    let mut touched = TouchedNodes::new(replaced, changed);
    for item in head.iter_mut().rev() {
        if !item.node().is_some_and(|node| touched.contains(node)) {
            break;
        }
        item.forget_geometry();
    }
    for item in tail.iter_mut() {
        if !item.node().is_some_and(|node| touched.contains(node)) {
            break;
        }
        item.forget_geometry();
    }
}

/// The nodes of a replaced fragment and its replacement. Only the nodes next
/// to the change are asked about, usually one or two, so each is found by a
/// scan; indexing every node of a large fragment would cost more.
struct TouchedNodes<'a, I> {
    replaced: &'a [DocumentLayoutItem<I>],
    changed: &'a [DocumentLayoutItem<I>],
    last: Option<(I, bool)>,
    scans: usize,
    index: Option<HashSet<&'a I>>,
}

impl<'a, I: Clone + Eq + Hash> TouchedNodes<'a, I> {
    const SCANS: usize = 4;

    fn new(replaced: &'a [DocumentLayoutItem<I>], changed: &'a [DocumentLayoutItem<I>]) -> Self {
        Self {
            replaced,
            changed,
            last: None,
            scans: 0,
            index: None,
        }
    }

    fn contains(&mut self, node: &I) -> bool {
        if let Some((last, found)) = &self.last
            && last == node
        {
            return *found;
        }
        let mut nodes = self
            .replaced
            .iter()
            .chain(self.changed)
            .filter_map(DocumentLayoutItem::node);
        let found = if let Some(index) = &self.index {
            index.contains(node)
        } else if self.scans < Self::SCANS {
            self.scans += 1;
            nodes.any(|other| other == node)
        } else {
            let index = self.index.insert(nodes.collect());
            index.contains(node)
        };
        self.last = Some((node.clone(), found));
        found
    }
}
