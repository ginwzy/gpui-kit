use super::*;
use crate::document::DocumentInlineStyle;

#[cfg(test)]
mod tests;

impl<I: Clone + Eq + 'static> DocumentState<I> {
    /// Current source-relative presentation styles, including editable text.
    pub fn styles(&self) -> &DocumentStyles {
        &self.model.styles
    }

    /// Replace a node's inline styles using node-local UTF-8 ranges. This is
    /// presentation only: selection, composition, revision and undo are preserved.
    /// Styles crossing the node boundary are rejected rather than partly removed.
    /// Only affected text items are remeasured; unchanged input is a no-op.
    pub fn set_node_inline_styles(
        &mut self,
        id: &I,
        mut styles: Vec<DocumentInlineStyle>,
        cx: &mut Context<Self>,
    ) -> Result<bool, DocumentStyleError> {
        let source = self
            .region(id)
            .ok_or(DocumentStyleError::UnknownNode)?
            .range();
        for style in &styles {
            let range = style.source();
            if range.start > range.end || range.end > source.len() {
                return Err(DocumentStyleError::OutOfBounds {
                    range,
                    source_len: source.len(),
                });
            }
        }
        styles = styles
            .into_iter()
            .map(|style| style.shifted(source.start))
            .collect();
        styles.sort_by_key(|style| (style.source().start, style.source().end));
        validate_style_ranges(
            &self.model.text,
            &self.model.regions,
            styles.iter().map(|style| style.source()),
            true,
        )?;
        styles.retain(|style| !style.source().is_empty());

        // Include collapsed styles at the boundary so deleting a token can clear
        // its decoration before another edit expands the collapsed source range.
        let inline = &self.model.styles.inline;
        let from = inline.partition_point(|style| {
            let range = style.source();
            range.end < source.start || (range.end == source.start && !range.is_empty())
        });
        let to = from
            + inline[from..].partition_point(|style| {
                let range = style.source();
                range.start < source.end || (range.start == source.end && range.is_empty())
            });
        for style in &inline[from..to] {
            let range = style.source();
            if range.start < source.start || range.end > source.end {
                return Err(DocumentStyleError::Overlap(range));
            }
        }
        if inline[from..to] == styles {
            return Ok(false);
        }
        let resolved = resolve_document_styles(
            &self.model.text,
            &self.model.regions,
            &self.model.projection_map,
            &DocumentStyles::new(Vec::new(), styles.clone()),
        )?;
        let display = self
            .model
            .projection_map
            .source_to_display(source.start, Affinity::Before)
            .expect("node start maps to display")
            ..self
                .model
                .projection_map
                .source_to_display(source.end, Affinity::After)
                .expect("node end maps to display");
        let resolved_inline = &self.model.resolved_styles.inline;
        let resolved_from =
            resolved_inline.partition_point(|style| style.display.end <= display.start);
        let resolved_to =
            resolved_inline.partition_point(|style| style.display.start < display.end);

        self.capture_viewport_anchor();
        self.model.styles.inline.splice(from..to, styles);
        self.model
            .resolved_styles
            .inline
            .splice(resolved_from..resolved_to, resolved.inline);
        for ix in 0..self.layout_items.len() {
            let DocumentLayoutItem::Text {
                display: line,
                source: segment,
                ..
            } = &self.layout_items[ix]
            else {
                continue;
            };
            if line.start >= display.end || line.end <= display.start {
                continue;
            }
            let next = self.model.text_layout_item(
                self.model.projection_map.display_text(),
                line.clone(),
                segment.clone(),
            );
            if !self.layout_items[ix].same_layout_identity(&next) {
                self.layout_items[ix] = next;
                self.list_state.remeasure_items(ix..ix + 1);
            }
        }
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(true)
    }
}
