//! Only visible table text becomes elements; full geometry still owns source hit testing.
use std::{cell::Cell, hash::Hash, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, ParentElement, Pixels,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use super::inline::InlineRow;
use crate::document::{
    DocumentState,
    state::{
        DocumentTextPresentation, RowFrame,
        inline_objects::LineLayout,
        table::{self, SCROLLBAR, View},
    },
};

pub(in crate::document) struct Row<I: 'static> {
    state: Entity<DocumentState<I>>,
    item: usize,
    display: Range<usize>,
    text: SharedString,
    presentation: DocumentTextPresentation,
    frame: RowFrame,
    view: View<I>,
    content: Option<AnyElement>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl<I: Clone + Eq + Hash + 'static> Row<I> {
    pub(in crate::document) fn new(
        state: Entity<DocumentState<I>>,
        item: usize,
        display: Range<usize>,
        text: SharedString,
        presentation: DocumentTextPresentation,
        frame: RowFrame,
        view: View<I>,
    ) -> Self {
        Self {
            state,
            item,
            display,
            text,
            presentation,
            frame,
            view,
            content: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    fn content(&mut self, bounds: Bounds<Pixels>, window: &Window, cx: &mut App) -> AnyElement {
        let geometry = self.view.geometry.clone();
        let offset = self.view.offset;
        // The real prepaint mask and row origin are available here. No retained
        // viewport or guessed previous-frame scroll offset is needed.
        let mask = window.content_mask().bounds;
        let first = (f32::from((mask.top() - bounds.top() - geometry.padding).max(px(0.)))
            / f32::from(geometry.line_height))
        .floor() as usize;
        let last = (f32::from((mask.bottom() - bounds.top() - geometry.padding).max(px(0.)))
            / f32::from(geometry.line_height))
        .ceil() as usize;
        let first = first.saturating_sub(1);
        let last = last.saturating_add(1);
        let (thumb, maximum, _) = table::scrollbar(geometry.viewport, geometry.width);
        let drag_bounds = self.bounds.clone();
        let drag_key = self.view.key.clone();
        let key = self.view.key.clone();
        let state = self.state.clone();
        state.update(cx, |_, cx| {
            let border = crate::Theme::global(cx).tokens.colors.border;
            let cells =
                geometry
                    .cells
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| {
                        cell.x + cell.width > offset && cell.x < offset + geometry.viewport
                    })
                    .flat_map(|(column, cell)| {
                        // Neighbors share one border: every cell draws its right and
                        // bottom edges, the first column and first row the outer ones.
                        let frame = div()
                            .absolute()
                            .left(cell.x)
                            .top_0()
                            .w(cell.width)
                            .h(geometry.height)
                            .border_r_1()
                            .border_b_1()
                            .when(column == 0, |this| this.border_l_1())
                            .when(geometry.first, |this| this.border_t_1())
                            .border_color(border)
                            .into_any_element();
                        let lines = cell.lines.iter().enumerate().take(last).skip(first).map(
                            |(row, line)| {
                                let x = cell.text_x(line, geometry.padding);
                                let y = geometry.padding + geometry.line_height * row as f32;
                                let part = InlineRow::new(
                                    self.state.clone(),
                                    self.item,
                                    (
                                        self.display.start + line.range.start
                                            ..self.display.start + line.range.end,
                                        self.text[line.range.clone()].to_owned().into(),
                                    ),
                                    slice(&self.presentation, line.range.clone()),
                                    self.frame,
                                    Vec::new(),
                                    window,
                                )
                                .with_origin(bounds.origin + gpui::point(x - offset, y));
                                div()
                                    .absolute()
                                    .left(x)
                                    .top(y)
                                    .h(geometry.line_height)
                                    .child(part)
                                    .into_any_element()
                            },
                        );
                        std::iter::once(frame).chain(lines)
                    });
            div()
                .relative()
                .w(geometry.viewport)
                .h(geometry.height)
                .overflow_hidden()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener({
                        let width = geometry.width;
                        let scrollbar = geometry.scrollbar;
                        move |state, event: &gpui::MouseDownEvent, _, cx| {
                            let bounds = drag_bounds.get();
                            if scrollbar && event.position.y >= bounds.bottom() - SCROLLBAR {
                                state.begin_table_drag(
                                    drag_key.clone(),
                                    event.position,
                                    bounds,
                                    width,
                                    cx,
                                );
                                cx.stop_propagation();
                            }
                        }
                    }),
                )
                .on_scroll_wheel(cx.listener(
                    move |state, event: &gpui::ScrollWheelEvent, _, cx| {
                        let delta = event.delta.pixel_delta(px(20.));
                        let horizontal = event.modifiers.shift || delta.x.abs() > delta.y.abs();
                        let amount = if event.modifiers.shift {
                            delta.x + delta.y
                        } else {
                            delta.x
                        };
                        if horizontal && amount != px(0.) && maximum > px(0.) {
                            state.scroll_table(&key, -amount, maximum, cx);
                            cx.stop_propagation();
                        }
                    },
                ))
                .child(
                    div()
                        .relative()
                        .w(geometry.width)
                        .h(geometry.height)
                        .ml(-offset)
                        .children(cells),
                )
                .when(geometry.scrollbar, |this| {
                    let x = (geometry.viewport - thumb) * (f32::from(offset) / f32::from(maximum));
                    this.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .w(geometry.viewport)
                            .h(SCROLLBAR - px(1.))
                            .bg(border.opacity(0.35))
                            .child(
                                div()
                                    .absolute()
                                    .left(x)
                                    .top(px(1.))
                                    .w(thumb)
                                    .h(SCROLLBAR - px(3.))
                                    .rounded(px(2.))
                                    .bg(border),
                            ),
                    )
                })
                .into_any_element()
        })
    }
}

fn slice(presentation: &DocumentTextPresentation, range: Range<usize>) -> DocumentTextPresentation {
    let clip = |span: &Range<usize>| {
        let start = span.start.max(range.start);
        let end = span.end.min(range.end);
        (start < end).then(|| start - range.start..end - range.start)
    };
    DocumentTextPresentation {
        text_style: presentation.text_style.clone(),
        highlights: presentation
            .highlights
            .iter()
            .filter_map(|(span, value)| Some((clip(span)?, *value)))
            .collect(),
        font_family_overrides: presentation
            .font_family_overrides
            .iter()
            .filter_map(|(span, value)| Some((clip(span)?, value.clone())))
            .collect(),
        font_scales: presentation
            .font_scales
            .iter()
            .filter_map(|(span, value)| Some((clip(span)?, *value)))
            .collect(),
        ..Default::default()
    }
}

impl<I: Clone + Eq + Hash + 'static> IntoElement for Row<I> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl<I: Clone + Eq + Hash + 'static> Element for Row<I> {
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
        (
            window.request_layout(
                gpui::Style {
                    size: gpui::size(
                        self.view.geometry.viewport.into(),
                        self.view.geometry.height.into(),
                    ),
                    flex_shrink: 0.,
                    ..Default::default()
                },
                [],
                cx,
            ),
            (),
        )
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
        self.bounds.set(bounds);
        let mut content = self.content(bounds, window, cx);
        content.layout_as_root(
            gpui::size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        content.prepaint_at(bounds.origin, window, cx);
        self.content = Some(content);
        self.state.update(cx, |state, _| {
            state.update_text_layout(
                self.item,
                self.display.clone(),
                LineLayout::Grid {
                    geometry: self.view.geometry.clone(),
                    offset: self.view.offset,
                    bounds,
                },
                bounds,
            )
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(content) = &mut self.content {
            content.paint(window, cx);
        }
    }
}
