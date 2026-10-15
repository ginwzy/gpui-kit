use std::{hash::Hash, ops::Range};

use super::{DocumentLayoutItem, DocumentState, host::forget_touched_geometry};

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Lays out again the nodes around editable node `target` after an edit
    /// inside it, instead of the whole document. The edit moved later source
    /// by `source_delta` and later display text by `display_delta`.
    /// `next_was_seam` tells whether the node after `target` started a layout
    /// seam before the edit; only that seam can move with an edit in `target`.
    /// Items before `first_changed_item` that compare equal keep their place.
    pub(super) fn relayout_edited_node(
        &mut self,
        target: usize,
        next_was_seam: bool,
        source_delta: isize,
        display_delta: isize,
        first_changed_item: usize,
    ) {
        let model = &self.model;
        let count = model.regions.as_slice().len();
        // Text before the edit is unchanged, so are the seams at or before it.
        let mut start = target;
        while !model.is_layout_seam(start) {
            start -= 1;
        }
        let mut end = target + 1;
        if !(next_was_seam && model.is_layout_seam(end)) {
            end = (end + 1).min(count);
            while !model.is_layout_seam(end) {
                end += 1;
            }
        }
        let old_start = self.layout_boundary(start);
        let old_end = match model.regions.as_slice().get(end) {
            // Items after the edit still hold their offsets from before it.
            Some(region) => self.layout_boundary_at(
                end,
                region
                    .range()
                    .start
                    .checked_add_signed(-source_delta)
                    .expect("a later node moved with the edit"),
            ),
            None => {
                self.layout_items.len()
                    - usize::from(matches!(
                        self.layout_items.last(),
                        Some(DocumentLayoutItem::Trailer)
                    ))
            }
        };
        let replacement = model
            .layout_items_of_nodes(start..end, self.text_rows.as_ref().map(|rows| &rows.cache));
        self.replace_layout_window(
            old_start..old_end,
            replacement,
            source_delta,
            display_delta,
            first_changed_item.saturating_sub(old_start),
        );
        #[cfg(debug_assertions)]
        self.assert_layout_matches_document();
    }

    /// Lays out again the display lines that the source ranges `restyled`,
    /// sorted by start, touch after their inline styles changed. A line that
    /// keeps its rows is replaced in place and keeps its geometry unless its
    /// fonts changed; the list only changes for lines whose rows did.
    pub(super) fn relayout_restyled_lines(&mut self, restyled: &[Range<usize>]) {
        let rows = self.text_rows.as_ref().map(|rows| &rows.cache);
        let mut lines = Vec::new();
        let mut next = 0;
        for source in restyled {
            let mut ix = self.layout_item_for_source(source.start).max(next);
            while let Some(item) = self.layout_items.get(ix)
                && item.source_start() <= source.end
            {
                let DocumentLayoutItem::Text { line, .. } = item else {
                    ix += 1;
                    continue;
                };
                let items = self.line_rows(ix);
                // An empty line carries no inline style.
                if !line.is_empty() {
                    let (
                        DocumentLayoutItem::Text { source: first, .. },
                        DocumentLayoutItem::Text { source: last, .. },
                    ) = (
                        &self.layout_items[items.start],
                        &self.layout_items[items.end - 1],
                    )
                    else {
                        unreachable!("line rows are text items");
                    };
                    let mut replacement = Vec::with_capacity(items.len());
                    self.model.push_text_layout_items(
                        &mut replacement,
                        line.clone(),
                        first.start..last.end,
                        true,
                        rows,
                        &mut super::LayoutCursor::default(),
                    );
                    lines.push((items.clone(), replacement));
                }
                ix = items.end;
                next = ix;
            }
        }
        let mut refit = false;
        // From the end, so earlier lines keep their indices.
        for (items, replacement) in lines.into_iter().rev() {
            if replacement.len() == items.len() {
                for (ix, mut new) in items.zip(replacement) {
                    let old = &self.layout_items[ix];
                    if old.same_layout_identity(&new) {
                        continue;
                    }
                    if line_key_of(old) == line_key_of(&new) {
                        new.keep_geometry(old);
                    } else {
                        refit = true;
                    }
                    self.layout_items.update(ix, |item| *item = new);
                }
                continue;
            }
            refit = true;
            self.list_state.splice(items.clone(), replacement.len());
            let retired = self.layout_items.splice(items, replacement);
            self.retire_text_layout_items(retired);
        }
        if refit {
            self.list_state.clone().measure_all();
            self.layout_items_changed();
        }
        #[cfg(debug_assertions)]
        self.assert_layout_matches_document();
    }

    /// Lays out again the display lines around a host edit that replaced
    /// source `edit` with `len` bytes, where nothing else changed. Returns
    /// false, changing nothing, when the edit does not begin and end on text
    /// lines; the whole layout must then be reconciled.
    pub(super) fn relayout_host_edit(
        &mut self,
        edit: Range<usize>,
        len: usize,
        display_delta: isize,
        first_changed_item: usize,
    ) -> bool {
        let source_delta = len as isize - edit.len() as isize;
        let first = self.layout_item_for_source(edit.start);
        let last = self
            .layout_items
            .partition_point(|item| item.source_start() <= edit.end);
        if first >= last {
            return false;
        }
        let (Some(DocumentLayoutItem::Text { .. }), Some(DocumentLayoutItem::Text { .. })) = (
            self.layout_items.get(first),
            self.layout_items.get(last - 1),
        ) else {
            return false;
        };
        let old = self.line_rows(first).start..self.line_rows(last - 1).end;
        let (
            DocumentLayoutItem::Text {
                line: first_line,
                source: first_source,
                ..
            },
            DocumentLayoutItem::Text {
                line: last_line,
                source: last_source,
                ..
            },
        ) = (
            &self.layout_items[old.start],
            &self.layout_items[old.end - 1],
        )
        else {
            unreachable!("line rows are text items");
        };
        let shift = |offset: usize, delta: isize| offset.checked_add_signed(delta).unwrap();
        let mut replacement = Vec::new();
        self.model.push_text_layout_items(
            &mut replacement,
            first_line.start..shift(last_line.end, display_delta),
            first_source.start..shift(last_source.end, source_delta),
            true,
            self.text_rows.as_ref().map(|rows| &rows.cache),
            &mut super::LayoutCursor::default(),
        );
        self.replace_layout_window(
            old.clone(),
            replacement,
            source_delta,
            display_delta,
            first_changed_item.saturating_sub(old.start),
        );
        #[cfg(debug_assertions)]
        self.assert_layout_matches_document();
        true
    }

    /// Replaces layout items `old` with `replacement`, the items of the same
    /// nodes after an edit that moved later source by `source_delta` and later
    /// display text by `display_delta`. Later items shift. Up to `keep_prefix`
    /// leading items and any trailing items that keep their identity keep
    /// their geometry and list slots; the list splices only the rest.
    pub(super) fn replace_layout_window(
        &mut self,
        old: Range<usize>,
        mut replacement: Vec<DocumentLayoutItem<I>>,
        source_delta: isize,
        display_delta: isize,
        keep_prefix: usize,
    ) {
        self.layout_items
            .map_range(old.end..self.layout_items.len(), |item| {
                item.shift(source_delta, display_delta);
            });
        let old_items = self.layout_items.clone();
        let prefix = old_items
            .range(old.clone())
            .zip(&replacement)
            .take(keep_prefix)
            .take_while(|(old, new)| old.same_layout_identity(new))
            .count();
        let suffix = old_items
            .range(old.start + prefix..old.end)
            .rev()
            .zip(replacement[prefix..].iter().rev())
            .take_while(|(old, new)| old.same_layout_identity(new))
            .count();
        let changed = old.start + prefix..old.end - suffix;
        let count = replacement.len() - prefix - suffix;
        if !changed.is_empty() || count != 0 {
            self.list_state.splice(changed, count);
            self.list_state.clone().measure_all();
        }
        let split = replacement.len() - suffix;
        let (head, tail) = replacement.split_at_mut(split);
        for (old, new) in old_items.range(old.start..old.start + prefix).zip(head) {
            new.keep_geometry(old);
        }
        for (old, new) in old_items.range(old.end - suffix..old.end).zip(tail) {
            new.keep_geometry(old);
        }
        // Geometry invalidation asks only which nodes were touched. One
        // representative per semantic line is sufficient, even when that
        // line previously occupied hundreds of thousands of display rows.
        let mut touched = Vec::new();
        let mut ix = old.start + prefix;
        while ix < old.end - suffix {
            touched.push(old_items[ix].clone());
            ix = match &old_items[ix] {
                DocumentLayoutItem::Text { .. } => self.line_rows(ix).end,
                _ => ix + 1,
            };
        }
        forget_touched_geometry(&touched, &mut replacement, prefix..split);
        let removed = self.layout_items.splice_prepared(old, replacement.into());
        self.retire_layout_items(removed);
        self.layout_items_changed();
    }

    /// Checks that patched layout items are the document's layout items. Rows
    /// can lag behind the wrap cache, so text compares by display line.
    #[cfg(debug_assertions)]
    fn assert_layout_matches_document(&self) {
        assert_eq!(
            self.list_state.item_count(),
            self.layout_items.len(),
            "the list has one item per layout item"
        );
        let document = self.next_layout_items();
        let retained: Vec<_> = self.layout_items.iter().cloned().collect();
        let actual = layout_lines(&retained);
        let expected = layout_lines(&document);
        if let Some(ix) =
            (0..actual.len().max(expected.len())).find(|ix| actual.get(*ix) != expected.get(*ix))
        {
            panic!(
                "patched layout differs from the document at line {ix} of {} (expected {})",
                actual.len(),
                expected.len()
            );
        }
    }
}

/// The key of the line a text item belongs to.
fn line_key_of<I>(item: &DocumentLayoutItem<I>) -> Option<u64> {
    match item {
        DocumentLayoutItem::Text { line_key, .. } => Some(*line_key),
        _ => None,
    }
}

/// An item's place in the layout, with the rows of a display line as one.
#[cfg(debug_assertions)]
#[derive(PartialEq)]
enum LayoutLine<'a, I> {
    Text {
        node: Option<&'a I>,
        line: Range<usize>,
        source: Range<usize>,
        key: u64,
    },
    Other(&'a DocumentLayoutItem<I>),
}

#[cfg(debug_assertions)]
fn layout_lines<I: PartialEq>(items: &[DocumentLayoutItem<I>]) -> Vec<LayoutLine<'_, I>> {
    let mut lines: Vec<LayoutLine<'_, I>> = Vec::with_capacity(items.len());
    for item in items {
        let DocumentLayoutItem::Text {
            node_id,
            line,
            source,
            line_key,
            ..
        } = item
        else {
            lines.push(LayoutLine::Other(item));
            continue;
        };
        if let Some(LayoutLine::Text {
            node,
            line: last,
            source: last_source,
            ..
        }) = lines.last_mut()
            && !line.is_empty()
            && last == line
            && *node == node_id.as_ref()
        {
            last_source.end = source.end;
            continue;
        }
        lines.push(LayoutLine::Text {
            node: node_id.as_ref(),
            line: line.clone(),
            source: source.clone(),
            key: *line_key,
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, EntityInputHandler as _, FontWeight, HighlightStyle,
        IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext,
        Window, div, red,
    };

    use super::super::{DocumentState, text_rows::TextRows};
    use crate::{
        Theme,
        document::{
            Affinity, AnchoredBlock, DocumentBlock, DocumentInlineStyle, DocumentPosition,
            DocumentProjection, DocumentRegion, DocumentStyles, EditPolicy, ProjectionSpan,
        },
    };

    struct Root(Entity<DocumentState<&'static str>>);

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    // Debug builds compare every patched layout with the whole document's, so
    // these edits fail there if a seam lays out differently on its own.
    #[gpui::test]
    fn edits_lay_out_their_nodes_as_the_whole_document_would(cx: &mut TestAppContext) {
        let text = "intro\n[b]one\ntwo\ntail\n";
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(|cx| {
                    let mut document = DocumentState::new(
                        text,
                        vec![
                            DocumentRegion::new("intro", 0..6, EditPolicy::Readonly),
                            DocumentRegion::new("block", 6..9, EditPolicy::Atomic),
                            DocumentRegion::new("draft", 9..16, EditPolicy::Editable),
                            // Starts with the tail: the two can trade places.
                            DocumentRegion::new("empty", 16..16, EditPolicy::Editable),
                            DocumentRegion::new("tail", 16..22, EditPolicy::Readonly),
                        ],
                        cx,
                    )
                    .unwrap();
                    document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
                    document
                        .set_rich_presentation_with_anchors(
                            DocumentProjection::new(22, vec![ProjectionSpan::hide(6..9)]).unwrap(),
                            vec![DocumentBlock::new("block", 6..9)],
                            vec![
                                AnchoredBlock::new(
                                    "intro note",
                                    DocumentPosition::new("intro", 6, Affinity::Before),
                                ),
                                AnchoredBlock::new(
                                    "tail note",
                                    DocumentPosition::new("tail", 1, Affinity::Before),
                                ),
                            ],
                            DocumentStyles::default(),
                            cx,
                        )
                        .unwrap();
                    document.set_text_rows(Some(TextRows::default()), cx);
                    document
                }));
                cx.new(|_| Root(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let edit = |node, range: std::ops::Range<usize>, text: &str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    document
                        .set_selection_positions(
                            DocumentPosition::new(node, range.start, Affinity::After),
                            DocumentPosition::new(node, range.end, Affinity::Before),
                            cx,
                        )
                        .unwrap();
                    document.replace_text_in_range(None, text, window, cx);
                });
                let _ = window.draw(cx);
            });
        };
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // Into the first line, after an anchored block and a block.
        edit("draft", 0..0, "x", &mut cx);
        // A line break at the end of a node moves the seam after it.
        edit("draft", 8..8, "\n", &mut cx);
        // An empty node at a line start gains and loses text next to the node
        // it starts with.
        edit("empty", 0..0, "e", &mut cx);
        edit("empty", 0..1, "", &mut cx);
        edit("draft", 8..9, "", &mut cx);
        // Joining and splitting lines inside the node.
        edit("draft", 4..5, "", &mut cx);
        edit("draft", 4..4, "\n", &mut cx);
        // And one in the middle of a line.
        edit("empty", 0..0, "e", &mut cx);
        edit("empty", 0..1, "", &mut cx);
        // A long line wraps into rows, and the edit keeps them.
        edit("draft", 0..0, &"wrapping words ".repeat(40), &mut cx);
        edit("draft", 3..3, "y", &mut cx);
        // Bold words wrap differently; a color keeps the rows; then neither.
        let style = |highlight: HighlightStyle, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    let styles = (0..40)
                        .map(|word| DocumentInlineStyle::new(word * 15..word * 15 + 8, highlight))
                        .collect();
                    document
                        .set_node_inline_styles(&"draft", styles, cx)
                        .unwrap();
                });
                let _ = window.draw(cx);
            });
        };
        style(
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
            &mut cx,
        );
        style(red().into(), &mut cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .set_node_inline_styles(&"draft", Vec::new(), cx)
                    .unwrap();
            });
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(
                document.text(),
                format!(
                    "intro\n[b]wray{}xone\ntwo\ntail\n",
                    &"wrapping words ".repeat(40)[3..]
                )
            );
            assert_eq!(
                document.list_state.item_count(),
                document.layout_items.len()
            );
        });
    }
}
