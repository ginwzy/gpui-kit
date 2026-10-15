use super::*;
use crate::document::DocumentInlineStyle;

#[cfg(test)]
mod tests;

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
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
        styles: Vec<DocumentInlineStyle>,
        cx: &mut Context<Self>,
    ) -> Result<bool, DocumentStyleError> {
        let len = self
            .region(id)
            .ok_or(DocumentStyleError::UnknownNode)?
            .range()
            .len();
        self.set_node_inline_styles_in_range(id, 0..len, styles, cx)
    }

    /// Replace inline styles inside `range`, preserving all other presentation.
    /// Both the replacement range and style ranges use node-local UTF-8 offsets.
    /// Existing styles crossing either boundary are rejected atomically; clear
    /// a range with an empty vector. Text, selection, composition, revision,
    /// undo and the viewport are preserved. Only intersecting text is revisited.
    pub fn set_node_inline_styles_in_range(
        &mut self,
        id: &I,
        range: Range<usize>,
        mut styles: Vec<DocumentInlineStyle>,
        cx: &mut Context<Self>,
    ) -> Result<bool, DocumentStyleError> {
        let node = self
            .region(id)
            .ok_or(DocumentStyleError::UnknownNode)?
            .range();
        if range.start > range.end || range.end > node.len() {
            return Err(DocumentStyleError::OutOfBounds {
                range,
                source_len: node.len(),
            });
        }
        let source = node.start + range.start..node.start + range.end;
        if self.model.text.clip_offset(source.start, Bias::Left) != source.start
            || self.model.text.clip_offset(source.end, Bias::Right) != source.end
        {
            return Err(DocumentStyleError::InvalidBoundary(source));
        }
        for style in &styles {
            let style_range = style.source();
            if style_range.start > style_range.end || style_range.end > node.len() {
                return Err(DocumentStyleError::OutOfBounds {
                    range: style_range,
                    source_len: node.len(),
                });
            }
            if style_range.start < range.start || style_range.end > range.end {
                return Err(DocumentStyleError::Overlap(style_range));
            }
        }
        styles = styles
            .into_iter()
            .map(|style| style.shifted(node.start))
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
            resolved_inline.partition_point(|style| style.source.end <= source.start);
        let resolved_to = resolved_inline.partition_point(|style| style.source.start < source.end);

        self.capture_viewport_anchor();
        self.model.styles.inline.splice(from..to, styles);
        let added = resolved.inline.len();
        let mut restyled: Vec<Range<usize>> = self
            .model
            .resolved_styles
            .inline
            .splice(resolved_from..resolved_to, resolved.inline)
            .map(|style| style.source)
            .collect();
        if self.text_rows.is_some() {
            // Only lines under an old or a new style change. Rows depend on
            // fonts, so a restyled line can also move its breaks.
            restyled.extend(
                self.model.resolved_styles.inline[resolved_from..resolved_from + added]
                    .iter()
                    .map(|style| style.source.clone()),
            );
            restyled.sort_by_key(|range| range.start);
            self.relayout_restyled_lines(&restyled);
            cx.emit(DocumentEvent::PresentationChanged);
            cx.notify();
            return Ok(true);
        }
        let mut remeasure: Option<Range<usize>> = None;
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
            let mut next = self.model.text_layout_item(
                self.model.projection_map.display_text(),
                line.clone(),
                segment.clone(),
                line.clone(),
                0,
            );
            let key = super::text_rows::line_key(&next);
            if let DocumentLayoutItem::Text { line_key, .. } = &mut next {
                *line_key = key;
            }
            if !self.layout_items[ix].same_layout_identity(&next) {
                self.layout_items[ix] = next;
                match &mut remeasure {
                    Some(range) if range.end == ix => range.end = ix + 1,
                    pending => {
                        if let Some(range) = pending.replace(ix..ix + 1) {
                            self.list_state.remeasure_items(range);
                        }
                    }
                }
            }
        }
        if let Some(range) = remeasure {
            self.list_state.remeasure_items(range);
        }
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(true)
    }
}
