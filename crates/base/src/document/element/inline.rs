//! One unwrapped mixed display row. Ordinary flex layout places text runs and
//! object boxes on their shared baseline. Its actual prepaint geometry feeds
//! the document's existing selection, hit testing, input handler and reveal.
use std::{cell::RefCell, hash::Hash, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, ParentElement, Pixels, SharedString, Styled, StyledText, Window, div,
    fill,
};

use crate::document::{
    DocumentState,
    state::{
        DocumentTextPresentation, RowFrame,
        inline_objects::{InlinePartLayout, LineLayout},
    },
};

pub(in crate::document) struct InlineRow<I: 'static> {
    state: Entity<DocumentState<I>>,
    item_ix: usize,
    display: Range<usize>,
    content: AnyElement,
    parts: Rc<RefCell<Vec<InlinePartLayout>>>,
}

impl<I: Clone + Eq + Hash + 'static> InlineRow<I> {
    pub(in crate::document) fn new(
        state: Entity<DocumentState<I>>,
        item_ix: usize,
        display: Range<usize>,
        text: SharedString,
        presentation: DocumentTextPresentation,
        frame: RowFrame,
        objects: Vec<(Range<usize>, AnyElement)>,
    ) -> Self {
        let parts = Rc::new(RefCell::new(Vec::new()));
        let (height, baseline, text_top) = frame.inline_metrics(Some(&presentation));
        let mut content = div()
            .flex()
            .h(height)
            .min_w_0()
            .pl(frame.inline_indent(&presentation));
        let text_part = |range: Range<usize>| {
            let clip = |span: &Range<usize>| {
                let start = span.start.max(range.start);
                let end = span.end.min(range.end);
                (start < end).then(|| start - range.start..end - range.start)
            };
            let styled = StyledText::new(text[range.clone()].to_owned())
                .with_highlights(
                    presentation
                        .highlights
                        .iter()
                        .filter_map(|(span, style)| Some((clip(span)?, *style)))
                        .collect::<Vec<_>>(),
                )
                .with_font_family_overrides(
                    presentation
                        .font_family_overrides
                        .iter()
                        .filter_map(|(span, font)| Some((clip(span)?, font.clone()))),
                );
            div().flex_shrink_0().mt(text_top).child(Part {
                range,
                content: PartContent::Text(styled),
                parts: parts.clone(),
                style: presentation.text_style.clone(),
            })
        };
        let mut start = 0;
        for ((range, element), (_, object)) in objects.into_iter().zip(&presentation.objects) {
            if start < range.start {
                content = content.child(text_part(start..range.start));
            }
            let (size, object_baseline) = object.resolve(frame.rem_size());
            start = range.end;
            content = content.child(
                div()
                    .flex_shrink_0()
                    .mt(baseline - object_baseline)
                    .child(Part {
                        range,
                        content: PartContent::Object(
                            div()
                                .w(size.width)
                                .h(size.height)
                                .overflow_hidden()
                                .child(element)
                                .into_any_element(),
                        ),
                        parts: parts.clone(),
                        style: None,
                    }),
            );
        }
        if start < text.len() {
            content = content.child(text_part(start..text.len()));
        }
        Self {
            state,
            item_ix,
            display,
            content: content.into_any_element(),
            parts,
        }
    }
}

impl<I: Clone + Eq + Hash + 'static> IntoElement for InlineRow<I> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl<I: Clone + Eq + Hash + 'static> Element for InlineRow<I> {
    type RequestLayoutState = ();
    type PrepaintState = LineLayout;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.content.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> LineLayout {
        self.parts.borrow_mut().clear();
        self.content.prepaint(window, cx);
        let layout = LineLayout::Inline {
            parts: self.parts.borrow().clone(),
            bounds,
        };
        self.state.update(cx, |state, _| {
            state.update_text_layout(self.item_ix, self.display.clone(), layout.clone(), bounds)
        });
        layout
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        layout: &mut LineLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let state = self.state.read(cx);
        let (_, selection, cursor) = state.segment_paint_snapshot(self.item_ix, &self.display);
        let cursor = cursor.filter(|_| state.show_cursor(window, cx));
        let color = crate::Theme::global(cx).tokens.colors.selection;
        if let Some(range) = selection.as_ref().filter(|range| !range.is_empty()) {
            if let (Some(start), Some(end)) = (
                layout.position_for_index(range.start),
                layout.position_for_index(range.end),
            ) {
                window.paint_quad(fill(
                    Bounds::from_corners(start, gpui::point(end.x, end.y + layout.line_height())),
                    color,
                ));
            }
        }
        self.content.paint(window, cx);
        if let Some(range) = selection {
            for part in self.parts.borrow().iter().filter(|part| {
                part.text.is_none()
                    && range.start <= part.display.start
                    && part.display.end <= range.end
            }) {
                window.paint_quad(fill(part.bounds, color.opacity(0.35)));
            }
        }
        if let Some(cursor) = cursor.and_then(|cursor| layout.position_for_index(cursor)) {
            let colors = crate::Theme::global(cx).tokens.colors;
            window.paint_quad(fill(
                crate::editing::blink_cursor::caret_bounds(cursor, layout.line_height()),
                colors.caret.unwrap_or(colors.foreground),
            ));
        }
    }
}

enum PartContent {
    Text(StyledText),
    Object(AnyElement),
}
struct Part {
    range: Range<usize>,
    content: PartContent,
    parts: Rc<RefCell<Vec<InlinePartLayout>>>,
    style: Option<gpui::TextStyleRefinement>,
}
impl IntoElement for Part {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Part {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        window.with_text_style(self.style.clone(), |window| match &mut self.content {
            PartContent::Text(text) => text.request_layout(None, None, window, cx),
            PartContent::Object(element) => (element.request_layout(window, cx), ()),
        })
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = match &mut self.content {
            PartContent::Text(text) => {
                text.prepaint(None, None, bounds, &mut (), window, cx);
                Some(text.layout().clone())
            }
            PartContent::Object(element) => {
                element.prepaint(window, cx);
                None
            }
        };
        self.parts.borrow_mut().push(InlinePartLayout {
            display: self.range.clone(),
            bounds,
            text,
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        match &mut self.content {
            PartContent::Text(text) => text.paint(None, None, bounds, &mut (), &mut (), window, cx),
            PartContent::Object(element) => element.paint(window, cx),
        }
    }
}
