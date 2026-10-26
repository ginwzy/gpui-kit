//! One unwrapped mixed display row. Ordinary flex layout places text runs and
//! object boxes on their shared baseline. Its actual prepaint geometry feeds
//! the document's existing selection, hit testing, input handler and reveal.
use std::{
    cell::{Cell, RefCell},
    hash::Hash,
    ops::Range,
    rc::Rc,
};

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
    baseline: Rc<Cell<Pixels>>,
    paragraph_baseline: Pixels,
    line_height: Pixels,
    /// Full-size text determines the row's strut, including actual fallback glyphs.
    text_layouts: Vec<gpui::TextLayout>,
    has_objects: bool,
    origin: Option<gpui::Point<Pixels>>,
}

impl<I: Clone + Eq + Hash + 'static> InlineRow<I> {
    pub(in crate::document) fn new(
        state: Entity<DocumentState<I>>,
        item_ix: usize,
        segment: (Range<usize>, SharedString),
        presentation: DocumentTextPresentation,
        frame: RowFrame,
        objects: Vec<(Range<usize>, AnyElement)>,
        window: &Window,
    ) -> Self {
        let (display, text) = segment;
        let parts = Rc::new(RefCell::new(Vec::new()));
        let (height, paragraph_baseline, _) = frame.inline_metrics(Some(&presentation));
        let baseline = Rc::new(Cell::new(Pixels::ZERO));
        let mut text_layouts = Vec::new();
        let mut paragraph_style = window.text_style();
        if let Some(style) = &presentation.text_style {
            gpui::Refineable::refine(&mut paragraph_style, style);
        }
        let paragraph_size = paragraph_style.font_size.to_pixels(window.rem_size());
        let mut content = div()
            .flex()
            .h(height)
            .min_w_0()
            .pl(frame.inline_indent(&presentation));
        let mut text_part = |range: Range<usize>, scale: u8| {
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
            let mut style = presentation.text_style.clone().unwrap_or_default();
            if scale != 100 {
                style.font_size = Some((paragraph_size * (f32::from(scale) / 100.)).into());
                style.line_height = Some(
                    window
                        .pixel_snap(frame.line_height() * (f32::from(scale) / 100.))
                        .into(),
                );
            } else {
                text_layouts.push(styled.layout().clone());
            }
            div().flex_shrink_0().child(Part {
                range,
                content: PartContent::Text(styled),
                parts: parts.clone(),
                baseline: baseline.clone(),
                style: Some(style),
            })
        };
        let mut text_parts = |range: Range<usize>| {
            let mut pieces = Vec::new();
            let mut start = range.start;
            for (span, scale) in &presentation.font_scales {
                let from = span.start.max(range.start);
                let to = span.end.min(range.end);
                if from >= to {
                    continue;
                }
                if start < from {
                    pieces.push(text_part(start..from, 100));
                }
                pieces.push(text_part(from..to, *scale));
                start = to;
            }
            if start < range.end {
                pieces.push(text_part(start..range.end, 100));
            }
            pieces
        };
        let mut start = 0;
        for ((range, element), (_, object)) in objects.into_iter().zip(&presentation.objects) {
            if start < range.start {
                content = content.children(text_parts(start..range.start));
            }
            let (size, object_baseline) = object.resolve(frame.rem_size());
            start = range.end;
            content = content.child(
                div().flex_shrink_0().child(Part {
                    range,
                    content: PartContent::Object(
                        div()
                            .w(size.width)
                            .h(size.height)
                            .overflow_hidden()
                            .child(element)
                            .into_any_element(),
                        object_baseline,
                    ),
                    parts: parts.clone(),
                    baseline: baseline.clone(),
                    style: None,
                }),
            );
        }
        if start < text.len() {
            content = content.children(text_parts(start..text.len()));
        }
        Self {
            state,
            item_ix,
            display,
            content: content.into_any_element(),
            parts,
            baseline,
            paragraph_baseline,
            line_height: frame.line_height(),
            text_layouts,
            has_objects: !presentation.objects.is_empty(),
            origin: None,
        }
    }

    /// A table places text using the same unsnapped advances it measured.
    pub(super) fn with_origin(mut self, origin: gpui::Point<Pixels>) -> Self {
        self.origin = Some(origin);
        self
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
        // Layout has shaped every fragment. Align from those glyph metrics,
        // not a guessed family metric or a rounded flex margin.
        let baseline = if !self.has_objects && !self.text_layouts.is_empty() {
            let (ascent, descent) = self
                .text_layouts
                .iter()
                .filter_map(|layout| layout.line_layout_for_index(0))
                .fold((Pixels::ZERO, Pixels::ZERO), |(ascent, descent), line| {
                    (ascent.max(line.ascent()), descent.max(line.descent()))
                });
            (self.line_height - ascent - descent) / 2. + ascent
        } else {
            self.paragraph_baseline
        };
        let mut drawn = bounds;
        if let Some(origin) = self.origin {
            drawn.origin = origin;
        }
        self.baseline.set(drawn.top() + baseline);
        window.with_element_offset(drawn.origin - bounds.origin, |window| {
            self.content.prepaint(window, cx)
        });
        let layout = LineLayout::Inline {
            parts: self.parts.borrow().clone(),
            bounds: drawn,
        };
        // Table subrows retain their paint geometry locally; the enclosing
        // table registers the one Grid used for document hit testing.
        if self.origin.is_none() {
            self.state.update(cx, |state, _| {
                state.update_text_layout(self.item_ix, self.display.clone(), layout.clone(), drawn)
            });
        }
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
    Object(AnyElement, Pixels),
}
struct Part {
    range: Range<usize>,
    content: PartContent,
    parts: Rc<RefCell<Vec<InlinePartLayout>>>,
    style: Option<gpui::TextStyleRefinement>,
    baseline: Rc<Cell<Pixels>>,
}
impl IntoElement for Part {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Part {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;
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
            PartContent::Object(element, _) => (element.request_layout(window, cx), ()),
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
    ) -> Bounds<Pixels> {
        // Flex leaf measurements round up to device pixels. Text advances do
        // not: accumulate actual glyph widths so many sized spans cannot drift
        // away from the widths used by wrapping and hit testing.
        let mut drawn = bounds;
        if let Some(previous) = self.parts.borrow().last() {
            drawn.origin.x = previous.bounds.right();
        }
        let text = match &mut self.content {
            PartContent::Text(text) => {
                let layout = text.layout();
                let line = layout.line_layout_for_index(0).unwrap();
                let text_baseline =
                    (layout.line_height() - line.ascent() - line.descent()) / 2. + line.ascent();
                drawn.origin.y = self.baseline.get() - text_baseline;
                drawn.size.width = line.width();
                drawn.size.height = layout.line_height();
                text.prepaint(None, None, drawn, &mut (), window, cx);
                Some(text.layout().clone())
            }
            PartContent::Object(element, baseline) => {
                drawn.origin.y = self.baseline.get() - *baseline;
                window.with_element_offset(drawn.origin - bounds.origin, |window| {
                    element.prepaint(window, cx);
                });
                None
            }
        };
        self.parts.borrow_mut().push(InlinePartLayout {
            display: self.range.clone(),
            bounds: drawn,
            text,
        });
        drawn
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        drawn: &mut Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        match &mut self.content {
            PartContent::Text(text) => text.paint(None, None, *drawn, &mut (), &mut (), window, cx),
            PartContent::Object(element, _) => element.paint(window, cx),
        }
    }
}
