//! Inline object projection and layout stay part of the document owner.
use std::{hash::Hash, ops::Range};

use super::*;

#[cfg(test)]
mod tests;

/// A request to replace editable text and complete, explicitly replaceable
/// inline objects. Source and selection are unchanged until the host responds.
/// Revision checking prevents a deferred response from replacing newer input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureReplacement<I> {
    transaction: EditTransaction,
    objects: Vec<I>,
}

impl<I> StructureReplacement<I> {
    pub fn transaction(&self) -> &EditTransaction {
        &self.transaction
    }
    pub fn objects(&self) -> &[I] {
        &self.objects
    }
}

#[derive(Clone)]
pub(in crate::document) struct InlinePartLayout {
    pub display: Range<usize>,
    pub bounds: Bounds<Pixels>,
    pub text: Option<gpui::TextLayout>,
}

#[derive(Clone)]
pub(in crate::document) enum LineLayout {
    Text(gpui::TextLayout),
    Inline {
        parts: Vec<InlinePartLayout>,
        bounds: Bounds<Pixels>,
    },
}

impl From<gpui::TextLayout> for LineLayout {
    fn from(layout: gpui::TextLayout) -> Self {
        Self::Text(layout)
    }
}

impl LineLayout {
    #[cfg(test)]
    pub(super) fn text(&self) -> &gpui::TextLayout {
        let Self::Text(layout) = self else {
            panic!("expected a plain text layout")
        };
        layout
    }
    pub(in crate::document) fn position_for_index(&self, ix: usize) -> Option<Point<Pixels>> {
        match self {
            Self::Text(layout) => layout.position_for_index(ix),
            Self::Inline { parts, bounds } => {
                let part = parts
                    .iter()
                    .find(|part| part.display.contains_inclusive(ix))?;
                let x = if let Some(text) = &part.text {
                    text.position_for_index(ix - part.display.start)?.x
                } else if ix == part.display.start {
                    part.bounds.left()
                } else {
                    part.bounds.right()
                };
                Some(point(x, bounds.top()))
            }
        }
    }

    pub(in crate::document) fn line_height(&self) -> Pixels {
        match self {
            Self::Text(layout) => layout.line_height(),
            Self::Inline { bounds, .. } => bounds.size.height,
        }
    }

    pub(super) fn closest_index(&self, point: Point<Pixels>, text: &str) -> usize {
        match self {
            Self::Text(layout) => layout.line_layout_for_index(0).map_or(0, |line| {
                closest_caret_index(
                    &line,
                    point - layout.bounds().origin,
                    layout.line_height(),
                    text,
                )
            }),
            Self::Inline { parts, .. } => {
                let Some(part) = parts.iter().min_by(|a, b| {
                    let distance = |bounds: Bounds<Pixels>| {
                        (bounds.left() - point.x)
                            .max(point.x - bounds.right())
                            .max(px(0.))
                    };
                    f32::from(distance(a.bounds)).total_cmp(&f32::from(distance(b.bounds)))
                }) else {
                    return 0;
                };
                if let Some(layout) = &part.text {
                    part.display.start
                        + layout.line_layout_for_index(0).map_or(0, |line| {
                            closest_caret_index(
                                &line,
                                gpui::point(
                                    point.x - part.bounds.left(),
                                    layout.line_height() / 2.,
                                ),
                                layout.line_height(),
                                &text[part.display.clone()],
                            )
                        })
                } else if point.x < part.bounds.center().x {
                    part.display.start
                } else {
                    part.display.end
                }
            }
        }
    }
}

impl<I: Clone + Eq + Hash> DocumentModel<I> {
    pub(super) fn inline_objects_in(
        &self,
        display: Range<usize>,
    ) -> Vec<(Range<usize>, super::super::InlineSize)> {
        if self.blocks.is_empty() || display.is_empty() {
            return Vec::new();
        }
        let source_start = self
            .display_to_source(display.start, Affinity::Before)
            .unwrap_or(0);
        let from = self
            .blocks
            .partition_point(|block| block.source().end <= source_start);
        self.blocks[from..]
            .iter()
            .take_while(|block| {
                self.source_to_display(block.source().start, Affinity::Before)
                    .is_some_and(|at| at < display.end)
            })
            .filter_map(|block| {
                let size = block.inline_size()?;
                let start = self.source_to_display(block.source().start, Affinity::Before)?;
                let end = self.source_to_display(block.source().end, Affinity::After)?;
                (display.start <= start && end <= display.end)
                    .then_some((start - display.start..end - display.start, size))
            })
            .collect()
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Prefer a real editable text node at an inline edge, including an empty
    /// node between adjacent objects. This changes no block or readonly edge.
    pub(super) fn inline_boundary_position(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> Option<DocumentPosition<I>> {
        let at = self
            .model
            .blocks
            .partition_point(|block| block.source().start <= offset);
        if !self.model.blocks[at.saturating_sub(2)..at]
            .iter()
            .any(|block| {
                block.inline_size().is_some()
                    && (block.source().start == offset || block.source().end == offset)
            })
        {
            return None;
        }
        let region = self
            .regions()
            .iter()
            .filter(|region| {
                region.policy() == EditPolicy::Editable && region.range().contains_inclusive(offset)
            })
            .min_by_key(|region| !region.range().is_empty())?;
        Some(DocumentPosition::new(
            region.id().clone(),
            offset - region.range().start,
            affinity,
        ))
    }

    pub(super) fn inline_replacement(
        &self,
        transaction: &EditTransaction,
    ) -> Option<StructureReplacement<I>> {
        if self.model.marked.is_some()
            || !matches!(transaction.origin(), EditOrigin::User | EditOrigin::Paste)
        {
            return None;
        }
        let [edit] = transaction.edits() else {
            return None;
        };
        let range = edit.range();
        if range.is_empty() {
            return None;
        }
        let mut covered = range.start;
        let mut objects = Vec::new();
        for region in
            self.model.regions.as_slice().iter().filter(|region| {
                region.range().start < range.end && range.start < region.range().end
            })
        {
            let source = region.range();
            if source.start > covered {
                return None;
            }
            match region.policy() {
                EditPolicy::Editable => {}
                EditPolicy::Atomic if range.start <= source.start && source.end <= range.end => {
                    let block = self
                        .model
                        .blocks
                        .iter()
                        .find(|block| block.id() == region.id())?;
                    if !block.is_replaceable() {
                        return None;
                    }
                    objects.push(block.id().clone());
                }
                _ => return None,
            }
            covered = source.end.min(range.end);
        }
        (covered == range.end && !objects.is_empty()).then(|| StructureReplacement {
            transaction: transaction.clone(),
            objects,
        })
    }

    /// Commit the host's response as one undoable structure edit. In addition
    /// to the normal snapshot validation, require the exact requested text and
    /// a still-current revision. The host supplies stable remaining node IDs,
    /// object presentation, and the caret after insertion.
    pub fn apply_structure_replacement(
        &mut self,
        request: &StructureReplacement<I>,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        if request.transaction.revision() != self.revision()
            || self.inline_replacement(&request.transaction).as_ref() != Some(request)
        {
            return Err(StructureError::StaleReplay);
        }
        let edit = &request.transaction.edits()[0];
        let mut expected = self.model.text.clone();
        expected.replace(edit.range(), edit.replacement());
        if expected != snapshot.text.as_str() {
            return Err(StructureError::PrefixChanged);
        }
        self.apply_structure_transaction(snapshot, start, cx)
    }

    pub(super) fn validate_inline_layout(
        &self,
        blocks: &[DocumentBlock<I>],
    ) -> Result<(), PresentationError> {
        if self.text_rows.is_none() && blocks.iter().any(|block| block.inline_size().is_some()) {
            return Err(PresentationError::Blocks(
                BlockError::InlineRequiresTextRows,
            ));
        }
        Ok(())
    }
}
