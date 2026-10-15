use std::{collections::HashSet, hash::Hash, ops::Range};

use gpui::Context;

use crate::input::RopeExt as _;

use super::{
    Affinity, BlockError, DocumentBlock, DocumentEvent, DocumentLayoutItem, DocumentModel,
    DocumentRevision, DocumentSnapshot, DocumentState, EditOrigin, EditPolicy,
    HostTransactionError, PresentationError, RegionError, ResolvedDocumentStyles, TextEdit,
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
        let removed: HashSet<&I> = regions[nodes.clone()]
            .iter()
            .map(|region| region.id())
            .collect();
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
                    .filter(|block| !removed.contains(block.position().node_id()))
                    .map(|block| block.id()),
            );
        let fragment_ids: HashSet<&I> = candidate
            .regions
            .as_slice()
            .iter()
            .map(|region| region.id())
            .chain(candidate.anchored_blocks.iter().map(|block| block.id()))
            .collect();
        for id in retained_ids {
            if fragment_ids.contains(id) {
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
        // Lines wrapped before keep their rows; others wrap at the next layout.
        let mut replacement =
            candidate.layout_items(self.text_rows.as_ref().map(|rows| &rows.cache));
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
            .filter(|block| !removed.contains(block.position().node_id()))
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
        // Keep matching measurements inside the changed fragment as well.
        self.replace_layout_window(
            old_layout,
            replacement,
            source_delta,
            display_delta,
            usize::MAX,
        );
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
    /// The node a text or block item belongs to.
    pub(super) fn node(&self) -> Option<&I> {
        match self {
            Self::Text { node_id, .. } => node_id.as_ref(),
            Self::Block { id, .. } | Self::AnchoredBlock { id, .. } => Some(id),
            Self::Trailer => None,
        }
    }

    /// Drops the geometry layout derived for this item.
    pub(super) fn forget_geometry(&mut self) {
        match self {
            Self::Text { frame, wrapped, .. } => {
                frame.0 = None;
                wrapped.0 = None;
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
            (
                Self::Text { frame, wrapped, .. },
                Self::Text {
                    frame: old_frame,
                    wrapped: old_wrapped,
                    ..
                },
            ) => {
                *frame = *old_frame;
                *wrapped = *old_wrapped;
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
            style.display = shifted(style.display.clone(), range.start as isize);
        }
        for style in &mut self.paragraphs[to..] {
            style.display = shifted(style.display.clone(), delta);
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
