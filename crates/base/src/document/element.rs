use std::ops::Range;

use gpui::{
    AnyElement, App, BorderStyle, Bounds, Corners, CursorStyle, Edges, Element, ElementId,
    ElementInputHandler, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, PaintQuad, Pixels, Point, SharedString, Styled as _, StyledText, TextStyleRefinement,
    Window, fill, px, size, transparent_black,
};

use super::{DocumentState, state::DocumentTextPresentation};

pub(super) enum DocumentChild<I: 'static> {
    Text {
        item_ix: usize,
        display: Range<usize>,
        text: SharedString,
        presentation: Box<DocumentTextPresentation>,
    },
    Block {
        item_ix: usize,
        id: I,
        source: Range<usize>,
        element: AnyElement,
    },
    Trailer {
        item_ix: usize,
        height: Pixels,
    },
}

pub struct DocumentElement<I: 'static> {
    state: gpui::Entity<DocumentState<I>>,
    content: AnyElement,
}

impl<I: Clone + Eq + 'static> DocumentElement<I> {
    pub(super) fn new(state: gpui::Entity<DocumentState<I>>, content: AnyElement) -> Self {
        Self { state, content }
    }

    pub(super) fn render_child(
        state: gpui::Entity<DocumentState<I>>,
        child: DocumentChild<I>,
    ) -> AnyElement {
        match child {
            DocumentChild::Text {
                item_ix,
                display,
                text,
                presentation,
            } => DocumentTextElement::new(state, item_ix, display, text, *presentation)
                .into_any_element(),
            DocumentChild::Block {
                item_ix,
                id,
                source,
                element,
            } => DocumentBlockElement::new(state, item_ix, id, source, element).into_any_element(),
            DocumentChild::Trailer { item_ix, height } => {
                DocumentTrailerElement::new(state, item_ix, height).into_any_element()
            }
        }
    }
}

impl<I: Clone + Eq + 'static> IntoElement for DocumentElement<I> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<I: Clone + Eq + 'static> Element for DocumentElement<I> {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some("document-content".into())
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.state.update(cx, |state, cx| {
            state.prepare_layout(bounds, window, cx);
        });
        self.content.prepaint(window, cx);
        self.state.update(cx, |state, cx| {
            state.extend_pointer_selection(cx);
        });
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.state.read(cx).focus_handle_snapshot();
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.state.clone()),
            cx,
        );
        self.content.paint(window, cx);
        // A selection drag belongs to the document that received the press,
        // including moves/releases over embedded controls or outside its bounds.
        let state = self.state.clone();
        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, window, cx| {
            if !phase.bubble() {
                state.update(cx, |state, cx| state.mouse_move(event, window, cx));
            }
        });
        let state = self.state.clone();
        window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, window, cx| {
            if !phase.bubble() {
                state.update(cx, |state, cx| state.mouse_up(event, window, cx));
            }
        });
    }
}

struct DocumentTrailerElement<I: 'static> {
    state: gpui::Entity<DocumentState<I>>,
    item_ix: usize,
    content: AnyElement,
}

impl<I: 'static> DocumentTrailerElement<I> {
    fn new(state: gpui::Entity<DocumentState<I>>, item_ix: usize, height: Pixels) -> Self {
        Self {
            state,
            item_ix,
            content: gpui::div().w_full().h(height).into_any_element(),
        }
    }
}

impl<I: Clone + Eq + 'static> IntoElement for DocumentTrailerElement<I> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<I: Clone + Eq + 'static> Element for DocumentTrailerElement<I> {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
        self.state.update(cx, |state, _| {
            state.update_trailer_layout(self.item_ix, bounds);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
    }
}

struct DocumentTextElement<I: 'static> {
    state: gpui::Entity<DocumentState<I>>,
    item_ix: usize,
    display: Range<usize>,
    text: StyledText,
    text_style: Option<TextStyleRefinement>,
}

impl<I: 'static> DocumentTextElement<I> {
    fn new(
        state: gpui::Entity<DocumentState<I>>,
        item_ix: usize,
        display: Range<usize>,
        text: SharedString,
        presentation: DocumentTextPresentation,
    ) -> Self {
        Self {
            state,
            item_ix,
            display,
            text: StyledText::new(text)
                .with_highlights(presentation.highlights)
                .with_font_family_overrides(presentation.font_family_overrides),
            text_style: presentation.text_style,
        }
    }
}

impl<I: Clone + Eq + 'static> IntoElement for DocumentTextElement<I> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<I: Clone + Eq + 'static> Element for DocumentTextElement<I> {
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
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(style) = self.text_style.clone() {
            window.with_text_style(Some(style), |window| {
                self.text
                    .request_layout(global_id, inspector_id, window, cx)
            })
        } else {
            self.text
                .request_layout(global_id, inspector_id, window, cx)
        }
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let layout = self.text.layout().clone();
        self.state.update(cx, |state, cx| {
            state.update_text_layout(self.item_ix, self.display.clone(), layout, bounds, cx);
        });
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus_handle, selected_range, cursor) = self
            .state
            .read(cx)
            .segment_paint_snapshot(self.item_ix, &self.display);
        let layout = self.text.layout().clone();
        if let Some(selected_range) = selected_range.filter(|range| !range.is_empty()) {
            let selection_color = crate::Theme::global(cx).tokens.colors.selection;
            paint_selection(&layout, selected_range, selection_color, window);
        }

        self.text.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );

        if focus_handle.is_focused(window)
            && let Some(cursor) = cursor
            && let Some(position) = layout.position_for_index(cursor)
        {
            window.paint_quad(fill(
                Bounds::new(position, size(px(1.5), layout.line_height())),
                window.text_style().color,
            ));
        }
    }
}

struct DocumentBlockElement<I: 'static> {
    state: gpui::Entity<DocumentState<I>>,
    item_ix: usize,
    id: I,
    source: Range<usize>,
    content: AnyElement,
}

impl<I: 'static> DocumentBlockElement<I> {
    fn new(
        state: gpui::Entity<DocumentState<I>>,
        item_ix: usize,
        id: I,
        source: Range<usize>,
        content: AnyElement,
    ) -> Self {
        Self {
            state,
            item_ix,
            id,
            source,
            content,
        }
    }
}

impl<I: Clone + Eq + 'static> IntoElement for DocumentBlockElement<I> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<I: Clone + Eq + 'static> Element for DocumentBlockElement<I> {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
        self.state.update(cx, |state, cx| {
            state.update_block_layout(
                self.item_ix,
                self.id.clone(),
                self.source.clone(),
                bounds,
                cx,
            );
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
        let selection = self.state.read(cx).selected_range();
        if selection.start <= self.source.start && self.source.end <= selection.end {
            // Objects participate in the same document selection as text.
            // Tint after painting so opaque embedded surfaces remain legible.
            let color = crate::Theme::global(cx)
                .tokens
                .colors
                .selection
                .opacity(0.35);
            window.paint_quad(fill(bounds, color));
        }
    }
}

fn paint_selection(
    layout: &gpui::TextLayout,
    range: Range<usize>,
    color: gpui::Hsla,
    window: &mut Window,
) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    for bounds in selection_quad_bounds(start, end, layout.bounds(), layout.line_height()) {
        window.paint_quad(PaintQuad {
            bounds,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

fn selection_quad_bounds(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }

    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}
