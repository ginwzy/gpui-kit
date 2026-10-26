//! The viewport is content, not a list index: which text is at its top, kept
//! through edits, splices and rewraps. Each layout resolves it once, after the
//! list has measured every item and before it lays out the visible ones, from
//! scrolling, following, reveals and the scroll range in that order.
use std::{hash::Hash, ops::RangeInclusive, rc::Rc};

use gpui::{Context, ListGeometry, ListOffset, ListState, Pixels, Window, px};

use super::{
    Affinity, AnchorBias, DocumentAnchor, DocumentEvent, DocumentLayoutItem, DocumentPosition,
    DocumentState, PositionError, ScrollPinError,
};

/// Pin moves at most this far in one layout are applied directly.
const PIN_EASE_SNAP: Pixels = px(24.);

pub(super) struct Viewport<I> {
    /// The viewport top sits `delta` below the top of the row that owns `anchor`.
    top: Option<TopAnchor>,
    /// The list's scroll top as this document last left it: after the last
    /// layout, a splice, or settling a scroll. Another one means the list
    /// scrolled itself (wheel, scrollbar, a child's autoscroll, a direct write).
    resolved: Option<ListOffset>,
    /// A splice since the last paint renumbered the items it recorded.
    painted_stale: bool,
    /// The top offset the last `ViewportTopChanged` reported.
    reported_top: Option<usize>,
    /// Scrolling asked for in pixels since the last layout.
    scroll: Pixels,
    pub(super) pin: Option<ScrollPin<I>>,
    pub(super) reveal: Option<Reveal>,
    pub(super) limit: Option<DocumentPosition<I>>,
    /// Where the last layout placed the items it rendered, for text without
    /// display rows, whose lines are placed from the last paint.
    painted: Option<Painted>,
}

impl<I> Default for Viewport<I> {
    fn default() -> Self {
        Self {
            top: None,
            resolved: None,
            painted_stale: false,
            reported_top: None,
            scroll: Pixels::ZERO,
            pin: None,
            reveal: None,
            limit: None,
            painted: None,
        }
    }
}

struct TopAnchor {
    anchor: DocumentAnchor,
    delta: Pixels,
}

struct Painted {
    /// The window y of content y 0.
    origin: Pixels,
    /// The content tops of the rendered items.
    tops: Vec<(usize, Pixels)>,
}

pub(super) struct ScrollPin<I> {
    pub(super) id: I,
    pub(super) anchor: DocumentAnchor,
    viewport_fraction: f32,
    /// The pin moved to another node or fraction; approach it over a few
    /// layouts instead of jumping, then track content growth directly.
    easing: bool,
    /// Released without an event once a layout has applied it.
    settling: bool,
    /// Holds the anchor lower, up to a fraction, while trailing content fits.
    trail: Option<ScrollPinTrail>,
    /// Scrolls only forward: an anchor above its hold stays where it is while
    /// it remains in the viewport.
    forward: bool,
}

/// Content after a pin's anchor that should end `clearance` above the viewport
/// bottom. The anchor sits between the pin's fraction and `max_fraction`.
struct ScrollPinTrail {
    anchor: DocumentAnchor,
    clearance: Pixels,
    max_fraction: f32,
}

/// Where a pin above trailing content would hold its position, as
/// [`DocumentState::scroll_hold`] measures it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollHold {
    fraction: f32,
    offset: f32,
    in_view: bool,
}

impl ScrollHold {
    /// The viewport fraction the position would be held at.
    pub fn fraction(&self) -> f32 {
        self.fraction
    }

    /// How far the position sits below its hold, as a viewport fraction;
    /// negative above it.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// Whether the position's line reaches below the viewport top. A pin that
    /// only scrolls forward keeps the viewport while its position is in view
    /// and not below its hold.
    pub fn is_in_view(&self) -> bool {
        self.in_view
    }
}

/// Item tops and heights in content coordinates, while a layout measures
/// them and after it.
trait ItemGeometry {
    fn item_top(&self, ix: usize) -> Option<Pixels>;
    fn item_height(&self, ix: usize) -> Option<Pixels>;
}

impl ItemGeometry for ListGeometry<'_> {
    fn item_top(&self, ix: usize) -> Option<Pixels> {
        ListGeometry::item_top(self, ix)
    }

    fn item_height(&self, ix: usize) -> Option<Pixels> {
        ListGeometry::item_height(self, ix)
    }
}

impl ItemGeometry for ListState {
    fn item_top(&self, ix: usize) -> Option<Pixels> {
        ListState::item_top(self, ix)
    }

    fn item_height(&self, ix: usize) -> Option<Pixels> {
        ListState::item_height(self, ix)
    }
}

/// Whether a line from content y `y`, `line` tall, reaches below `top`.
fn in_view(y: Pixels, line: Pixels, top: Pixels) -> bool {
    y + line > top
}

pub(super) enum Reveal {
    /// The caret, with a line of margin, once its line has its geometry.
    Caret,
    /// The row that owns a position, scrolled the least that shows it.
    Position(DocumentAnchor),
    /// The row that owns a position, at the viewport top.
    Top(DocumentAnchor),
}

impl Reveal {
    fn anchor(&self) -> Option<DocumentAnchor> {
        match self {
            Self::Caret => None,
            Self::Position(anchor) | Self::Top(anchor) => Some(*anchor),
        }
    }
}

impl<I> ScrollPin<I> {
    #[cfg(test)]
    pub(super) fn is_easing(&self) -> bool {
        self.easing
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// Lets the list ask this document for each layout's scroll top.
    pub(super) fn install_scroll_resolver(&self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        self.list_state.set_scroll_resolver(Some(Rc::new(
            move |geometry: &mut ListGeometry, window: &mut Window, cx: &mut gpui::App| {
                weak.upgrade()?
                    .update(cx, |state, cx| state.resolve_viewport(geometry, window, cx))
            },
        )));
    }

    /// Forgets the viewport of a replaced document; the list starts at its top.
    pub(super) fn reset_viewport(&mut self) {
        let viewport = std::mem::take(&mut self.viewport);
        self.viewport.limit = viewport.limit;
    }

    /// Makes a scroll the list made itself since the last layout (wheel,
    /// scrollbar, a direct write) content, while its items still describe
    /// the document: before an edit changes the text they were laid out from,
    /// and before the list splices them, which moves its scroll top.
    pub(super) fn settle_list_scroll(&mut self) {
        let list_top = self.list_state.logical_scroll_top();
        if self
            .viewport
            .resolved
            .is_none_or(|resolved| same_offset(resolved, list_top))
        {
            return;
        }
        self.viewport.resolved = Some(list_top);
        let Some(ix) = self.anchorable_item(list_top.item_ix) else {
            return;
        };
        // The anchored item may lie on either side of the list top's item.
        let delta = match (
            self.list_state.item_top(list_top.item_ix),
            self.list_state.item_top(ix),
        ) {
            (Some(top), Some(item)) => top + list_top.offset_in_item - item,
            _ => list_top.offset_in_item,
        };
        self.anchor_item(ix, delta);
    }

    /// Splices the list's items through `splice`. The scroll top it leaves is
    /// the splice's, not a scroll.
    pub(super) fn splice_list<R>(&mut self, splice: impl FnOnce(&ListState) -> R) -> R {
        self.settle_list_scroll();
        let result = splice(&self.list_state);
        if self.viewport.resolved.is_some() {
            self.viewport.resolved = Some(self.list_state.logical_scroll_top());
        }
        self.viewport.painted_stale = true;
        result
    }

    /// Scrolls by `delta` at the next layout, as input does.
    pub fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        self.viewport.scroll += delta;
        cx.notify();
    }

    /// Shortens the dynamic trailer so scrolling ends once the line of
    /// `position` reaches the viewport top, or the document end when what
    /// follows that line is taller than the viewport. `None` lets the document
    /// end scroll to the top.
    pub fn set_trailer_limit(
        &mut self,
        position: Option<DocumentPosition<I>>,
        cx: &mut Context<Self>,
    ) {
        if self.viewport.limit == position {
            return;
        }
        self.viewport.limit = position;
        cx.notify();
    }

    pub fn pin_scroll(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        viewport_fraction: f32,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        self.place_scroll_pin(pin_id, position, viewport_fraction, false, cx)
    }

    fn place_scroll_pin(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        viewport_fraction: f32,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        if !viewport_fraction.is_finite() || !(0.0..=1.0).contains(&viewport_fraction) {
            return Err(ScrollPinError::InvalidViewportFraction);
        }
        let offset = self
            .resolve_position(position)
            .map_err(ScrollPinError::InvalidPosition)?;
        // Re-pinning the same target follows its growth; moving an existing
        // pin elsewhere eases there unless the platform asks for reduced motion.
        let easing = match self.viewport.pin.take() {
            Some(previous) => {
                let moved = previous.viewport_fraction != viewport_fraction
                    || self
                        .model
                        .position_for_anchor(previous.anchor)
                        .map_or(true, |previous| previous.node_id() != position.node_id());
                self.remove_scroll_pin_anchors(&previous);
                (previous.easing || moved) && !cx.reduce_motion()
            }
            None => false,
        };
        let anchor = self.anchor_for(offset, position);
        self.viewport.pin = Some(ScrollPin {
            id: pin_id,
            anchor,
            viewport_fraction,
            easing,
            settling: false,
            trail: None,
            forward,
        });
        cx.notify();
        Ok(())
    }

    fn anchor_for(&mut self, offset: usize, position: &DocumentPosition<I>) -> DocumentAnchor {
        self.model.anchors.create_for_node(
            offset,
            if position.affinity() == Affinity::Before {
                AnchorBias::Left
            } else {
                AnchorBias::Right
            },
            Some(position.node_id().clone()),
            position.affinity(),
        )
    }

    pub fn pinned_scroll_id(&self) -> Option<&I> {
        self.viewport.pin.as_ref().map(|pin| &pin.id)
    }

    /// Pins `position` like [`Self::pin_scroll`], holding it as low within
    /// `fractions` as lets `trailing` end `clearance` above the viewport bottom.
    /// The hold is recomputed in each layout, so trailing growth moves it.
    pub fn pin_scroll_above(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        trailing: &DocumentPosition<I>,
        clearance: Pixels,
        fractions: RangeInclusive<f32>,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        self.place_scroll_pin_above(pin_id, position, trailing, clearance, fractions, false, cx)
    }

    /// Pins `position` like [`Self::pin_scroll_above`], but only scrolls
    /// forward: while `position` is above its hold and in the viewport, the
    /// viewport stays where it is, until growth carries `position` past the hold.
    pub fn pin_scroll_forward_above(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        trailing: &DocumentPosition<I>,
        clearance: Pixels,
        fractions: RangeInclusive<f32>,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        self.place_scroll_pin_above(pin_id, position, trailing, clearance, fractions, true, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn place_scroll_pin_above(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        trailing: &DocumentPosition<I>,
        clearance: Pixels,
        fractions: RangeInclusive<f32>,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        let (min, max) = (*fractions.start(), *fractions.end());
        if !max.is_finite() || !(min..=1.0).contains(&max) {
            return Err(ScrollPinError::InvalidViewportFraction);
        }
        let trailing_offset = self
            .resolve_position(trailing)
            .map_err(ScrollPinError::InvalidPosition)?;
        self.place_scroll_pin(pin_id, position, min, forward, cx)?;
        let anchor = self.anchor_for(trailing_offset, trailing);
        let pin = self.viewport.pin.as_mut().expect("pin was just set");
        pin.trail = Some(ScrollPinTrail {
            anchor,
            clearance,
            max_fraction: max,
        });
        Ok(())
    }

    /// Where [`Self::pin_scroll_above`] would hold `position` in the current
    /// layout, measured as each layout resolves the viewport, or `None` when
    /// `position` is not laid out.
    pub fn scroll_hold(
        &self,
        position: &DocumentPosition<I>,
        trailing: &DocumentPosition<I>,
        clearance: Pixels,
        fractions: RangeInclusive<f32>,
    ) -> Option<ScrollHold> {
        self.resolve_position(position).ok()?;
        let height = self.list_state.viewport_bounds().size.height;
        if height <= Pixels::ZERO {
            return None;
        }
        let list = &self.list_state;
        let painted = !self.viewport.painted_stale;
        let (target, line) = self.content_line(list, position, painted)?;
        let bottom = self
            .resolve_position(trailing)
            .ok()
            .and_then(|_| self.content_line(list, trailing, painted))
            .map(|(y, line)| y + line);
        let fraction = hold_fraction(target, bottom, clearance, fractions, height);
        let scroll_top = list.logical_scroll_top();
        let top = list.item_top(scroll_top.item_ix)? + scroll_top.offset_in_item;
        Some(ScrollHold {
            fraction,
            offset: (target - top) / height - fraction,
            in_view: in_view(target, line, top),
        })
    }

    fn remove_scroll_pin_anchors(&mut self, pin: &ScrollPin<I>) {
        self.model.anchors.remove(pin.anchor);
        if let Some(trail) = &pin.trail {
            self.model.anchors.remove(trail.anchor);
        }
    }

    /// Releases `pin_id` once a layout has applied it, so a final content
    /// change (such as collapsing) is still followed.
    pub fn settle_scroll_pin(&mut self, pin_id: &I, cx: &mut Context<Self>) -> bool {
        let Some(pin) = self.viewport.pin.as_mut().filter(|pin| &pin.id == pin_id) else {
            return false;
        };
        pin.settling = true;
        pin.easing = false;
        cx.notify();
        true
    }

    pub fn unpin_scroll(&mut self, pin_id: &I, cx: &mut Context<Self>) -> bool {
        if self
            .viewport
            .pin
            .as_ref()
            .is_none_or(|pin| &pin.id != pin_id)
        {
            return false;
        }
        let pin = self.viewport.pin.take().expect("scroll pin was present");
        self.remove_scroll_pin_anchors(&pin);
        cx.notify();
        true
    }

    /// Scrolls the row that owns `position` into view at the next layout.
    pub fn reveal_position(
        &mut self,
        position: &DocumentPosition<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), PositionError> {
        let anchor = self.reveal_anchor(position, cx)?;
        self.set_reveal(Some(Reveal::Position(anchor)));
        cx.notify();
        Ok(())
    }

    /// Scrolls so the row that owns `position` starts at the viewport top at
    /// the next layout, as far as the scroll range allows.
    pub fn reveal_position_at_top(
        &mut self,
        position: &DocumentPosition<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), PositionError> {
        let anchor = self.reveal_anchor(position, cx)?;
        self.set_reveal(Some(Reveal::Top(anchor)));
        cx.notify();
        Ok(())
    }

    fn reveal_anchor(
        &mut self,
        position: &DocumentPosition<I>,
        cx: &mut Context<Self>,
    ) -> Result<DocumentAnchor, PositionError> {
        let offset = self.resolve_position(position)?;
        self.stop_following(cx);
        Ok(self.model.anchors.create_for_node(
            offset,
            AnchorBias::Right,
            Some(position.node_id().clone()),
            position.affinity(),
        ))
    }

    /// Scrolls the caret into view at the next layout, which measures the
    /// line an edit changed. It keeps a scroll pin while the caret stays
    /// visible in an editable region, so typing beside followed content does
    /// not stop it.
    pub fn reveal_caret(&mut self, cx: &mut Context<Self>) {
        self.pause_blink_cursor(cx);
        self.set_reveal(Some(Reveal::Caret));
        cx.notify();
    }

    pub(super) fn cancel_caret_reveal(&mut self) {
        if matches!(self.viewport.reveal, Some(Reveal::Caret)) {
            self.viewport.reveal = None;
        }
    }

    pub(super) fn set_reveal(&mut self, reveal: Option<Reveal>) {
        if let Some(anchor) = std::mem::replace(&mut self.viewport.reveal, reveal)
            .as_ref()
            .and_then(Reveal::anchor)
        {
            self.model.anchors.remove(anchor);
        }
    }

    /// The position a reveal waits to place, which its rows wrap first.
    pub(super) fn reveal_target(&self) -> Option<usize> {
        self.viewport
            .reveal
            .as_ref()
            .and_then(Reveal::anchor)
            .and_then(|anchor| self.model.anchors.resolve(anchor))
    }

    /// The content at the viewport top, which visible rows wrap around.
    pub(super) fn viewport_top_offset(&self) -> Option<usize> {
        self.viewport
            .top
            .as_ref()
            .and_then(|top| self.model.anchors.resolve(top.anchor))
    }

    /// The start of the row at the viewport top as the last layout placed
    /// it, or `None` before the first layout. Rows that only partly show at
    /// the top count as the top row.
    pub fn viewport_top(&self) -> Option<DocumentPosition<I>> {
        let top = self.viewport.top.as_ref()?;
        self.model.position_for_anchor(top.anchor).ok()
    }

    pub(super) fn stop_following(&mut self, cx: &mut Context<Self>) {
        let Some(pin) = self.viewport.pin.take() else {
            return;
        };
        self.remove_scroll_pin_anchors(&pin);
        cx.emit(DocumentEvent::StopFollowingRequested { pin_id: pin.id });
        cx.notify();
    }

    /// The user scrolled the list: reveals and following give way.
    pub(super) fn list_scrolled(&mut self, cx: &mut Context<Self>) {
        let wrapping = self.is_laying_out_text();
        if matches!(self.viewport.reveal, Some(Reveal::Caret))
            || matches!(
                self.viewport.reveal,
                Some(Reveal::Position(_) | Reveal::Top(_))
            ) && wrapping
        {
            self.set_reveal(None);
        }
        self.stop_following(cx);
        cx.emit(DocumentEvent::ViewportScrolled);
    }

    /// The scroll top of this layout: every item is measured, none laid out.
    fn resolve_viewport(
        &mut self,
        geometry: &mut ListGeometry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ListOffset> {
        let height = geometry.viewport_height();
        if height <= Pixels::ZERO {
            self.text_layouts.clear();
            self.block_layouts.clear();
            self.trailer_layout = None;
            return None;
        }
        if self.scrollbar_input_pending.replace(false) {
            self.list_scrolled(cx);
        }
        let list_top = geometry.scroll_top();
        let list_moved = self
            .viewport
            .resolved
            .is_none_or(|resolved| !same_offset(resolved, list_top));
        let painted_valid = !self.viewport.painted_stale;
        self.viewport.painted_stale = false;
        let list_y = geometry.y_for_offset(&list_top);
        let mut top = if list_moved {
            list_y
        } else {
            self.viewport
                .top
                .as_ref()
                .and_then(|top| {
                    let position = self.model.position_for_anchor(top.anchor).ok()?;
                    let (ix, line_top, _) = self.scroll_line(&position)?;
                    Some(geometry.item_top(ix)? + line_top + top.delta)
                })
                .unwrap_or(list_y)
        };
        top += std::mem::take(&mut self.viewport.scroll);
        top = self.follow_in_layout(geometry, top, painted_valid, window);
        top = self.reveal_in_layout(geometry, top, painted_valid, cx);
        top = self.fit_scroll_range(geometry, top);
        self.anchor_viewport_top(geometry, top);
        let placed = self.viewport_top_offset();
        if std::mem::replace(&mut self.viewport.reported_top, placed) != placed {
            cx.emit(DocumentEvent::ViewportTopChanged);
        }
        self.record_painted(geometry, top);
        // This layout paints the records again.
        self.text_layouts.clear();
        self.block_layouts.clear();
        self.trailer_layout = None;
        let offset = geometry.offset_for_y(top);
        self.viewport.resolved = Some(offset);
        Some(offset)
    }

    /// Moves `top` so the pin holds its anchor where it asks.
    fn follow_in_layout(
        &mut self,
        geometry: &ListGeometry,
        top: Pixels,
        painted: bool,
        window: &mut Window,
    ) -> Pixels {
        let Some(pin) = self.viewport.pin.as_ref() else {
            return top;
        };
        let height = geometry.viewport_height();
        let target = self
            .model
            .position_for_anchor(pin.anchor)
            .ok()
            .and_then(|target| self.content_line(geometry, &target, painted));
        let Some((target, line)) = target else {
            return top;
        };
        let fraction = match &pin.trail {
            Some(trail) => {
                let bottom = self
                    .model
                    .position_for_anchor(trail.anchor)
                    .ok()
                    .and_then(|trailing| self.content_line(geometry, &trailing, painted))
                    .map(|(y, line)| y + line);
                hold_fraction(
                    target,
                    bottom,
                    trail.clearance,
                    pin.viewport_fraction..=trail.max_fraction,
                    height,
                )
            }
            None => pin.viewport_fraction,
        };
        let mut delta = target - height * fraction - top;
        // A forward pin leaves an anchor above its hold, unless it left the viewport.
        if pin.forward && delta < Pixels::ZERO && in_view(target, line, top) {
            delta = Pixels::ZERO;
        }
        let pin = self.viewport.pin.as_mut().expect("pin is present");
        if pin.easing {
            if delta.abs() <= PIN_EASE_SNAP {
                pin.easing = false;
            } else {
                // Halve the distance each layout; the next one continues.
                delta = delta * 0.5;
                window.request_animation_frame();
            }
        }
        if pin.settling {
            let pin = self.viewport.pin.take().expect("pin is present");
            self.remove_scroll_pin_anchors(&pin);
        }
        top + delta
    }

    /// Moves `top` the least that shows what a reveal asks for.
    fn reveal_in_layout(
        &mut self,
        geometry: &ListGeometry,
        top: Pixels,
        painted: bool,
        cx: &mut Context<Self>,
    ) -> Pixels {
        let height = geometry.viewport_height();
        let wrapping = self.is_laying_out_text();
        match &self.viewport.reveal {
            None => top,
            Some(Reveal::Caret) => {
                let Ok(caret) = self.model.position_for_anchor(self.model.selection.head()) else {
                    self.set_reveal(None);
                    return top;
                };
                let Some((y, line)) = self.content_line(geometry, &caret, painted) else {
                    return top;
                };
                // A line of margin above and below the caret's line.
                let delta = if y < top + line {
                    y - top - line
                } else if y + line > top + height - line {
                    y + line * 2. - top - height
                } else {
                    Pixels::ZERO
                };
                if delta.abs() >= px(0.5) || !self.is_editable_position(&caret) {
                    self.stop_following(cx);
                }
                if !wrapping {
                    self.set_reveal(None);
                }
                if delta.abs() >= px(0.5) {
                    top + delta
                } else {
                    top
                }
            }
            Some(reveal @ (Reveal::Position(anchor) | Reveal::Top(anchor))) => {
                let at_top = matches!(reveal, Reveal::Top(_));
                let row = self
                    .model
                    .position_for_anchor(*anchor)
                    .ok()
                    .and_then(|position| {
                        let (ix, line_top, height) = self.scroll_line(&position)?;
                        Some((
                            geometry.item_top(ix)? + line_top,
                            height.or_else(|| geometry.item_height(ix))?,
                        ))
                    });
                // The target's own rows may still be one unwrapped line.
                if !wrapping {
                    self.set_reveal(None);
                }
                match row {
                    Some((row_top, _)) if at_top || row_top < top => row_top,
                    // A row taller than the viewport shows its top.
                    Some((row_top, row)) if row_top + row > top + height => {
                        (row_top + row - height).min(row_top)
                    }
                    _ => top,
                }
            }
        }
    }

    /// Clamps `top` to the scroll range and sizes the dynamic trailer to it.
    fn fit_scroll_range(&mut self, geometry: &mut ListGeometry, top: Pixels) -> Pixels {
        let height = geometry.viewport_height();
        let trailer = matches!(self.layout_items.last(), Some(DocumentLayoutItem::Trailer))
            .then(|| self.layout_items.len() - 1);
        let trailer_height = trailer
            .and_then(|ix| geometry.item_height(ix))
            .unwrap_or_default();
        let end = geometry.content_height() - trailer_height;
        let max_top = match trailer {
            None => end - height,
            Some(_) => match self.limit_top(geometry) {
                Some(line) => line.max(end - height),
                None => end,
            },
        }
        .max(Pixels::ZERO);
        if let Some(ix) = trailer {
            let fitted = (max_top + height - end).max(Pixels::ZERO);
            if (fitted - trailer_height).abs() >= px(0.5) || self.trailer_height.is_none() {
                geometry.set_item_height(ix, fitted);
                self.trailer_height = Some(fitted);
            }
        }
        top.clamp(Pixels::ZERO, max_top)
    }

    /// The content y of the limit's line top.
    fn limit_top(&self, geometry: &ListGeometry) -> Option<Pixels> {
        let limit = self.viewport.limit.as_ref()?;
        self.resolve_position(limit).ok()?;
        geometry.item_top(self.position_item(limit)?)
    }

    /// Anchors the content at `top`: the first position of the item there,
    /// measured from that item's top, which needs no line geometry.
    fn anchor_viewport_top(&mut self, geometry: &ListGeometry, top: Pixels) {
        let start = geometry.offset_for_y(top).item_ix;
        let Some(ix) = self.anchorable_item(start) else {
            return;
        };
        let Some(item_top) = geometry.item_top(ix) else {
            return;
        };
        self.anchor_item(ix, top - item_top);
    }

    /// Rows anchored to a position and the trailer have no source of their
    /// own: the nearest text or block after `start`, else before it.
    fn anchorable_item(&self, start: usize) -> Option<usize> {
        let start = start.min(self.layout_items.len());
        (start..self.layout_items.len())
            .chain((0..start).rev())
            .find(|ix| {
                matches!(
                    self.layout_items[*ix],
                    DocumentLayoutItem::Text { .. } | DocumentLayoutItem::Block { .. }
                )
            })
    }

    /// Anchors the viewport top `delta` below the top of item `ix`.
    fn anchor_item(&mut self, ix: usize, delta: Pixels) {
        if let Some(previous) = self.viewport.top.take() {
            self.model.anchors.remove(previous.anchor);
        }
        let (offset, delta) = self.scroll_anchor(ix, delta);
        // The viewport holds content, not a node: content inserted before it
        // pushes it down, and replacing its text leaves the viewport where the
        // replacement starts.
        let anchor = self.model.anchors.create_for_content(offset);
        self.viewport.top = Some(TopAnchor { anchor, delta });
    }

    /// Remembers where this layout places the items it renders, for text
    /// without display rows; their lines are found in the paint they get.
    fn record_painted(&mut self, geometry: &ListGeometry, top: Pixels) {
        if self.text_rows.is_some() {
            self.viewport.painted = None;
            return;
        }
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let height = geometry.viewport_height();
        let mut tops = Vec::new();
        let mut ix = geometry.offset_for_y(top).item_ix;
        while ix < geometry.item_count()
            && let Some(item_top) = geometry
                .item_top(ix)
                .filter(|item_top| *item_top < top + height)
        {
            tops.push((ix, item_top));
            ix += 1;
        }
        self.viewport.painted = Some(Painted {
            origin: bounds.top() - top,
            tops,
        });
    }

    /// The item that owns `position`: its block, or its text row.
    /// The line `position` scrolls by: its item, the line's top below the
    /// item's top, and its height when not the item's own. A table row
    /// scrolls by its cells' wrapped lines; any other item is one line.
    fn scroll_line(
        &self,
        position: &DocumentPosition<I>,
    ) -> Option<(usize, Pixels, Option<Pixels>)> {
        match self.table_line_for_position(position) {
            Some((ix, line_top, height)) => Some((ix, line_top, Some(height))),
            None => Some((self.position_item(position)?, Pixels::ZERO, None)),
        }
    }

    /// The inverse of [`Self::scroll_line`]: the content `delta` below the
    /// top of item `ix`, as a source offset and its distance below the top
    /// of the line it scrolls by.
    fn scroll_anchor(&self, ix: usize, delta: Pixels) -> (usize, Pixels) {
        self.table_anchor_in_item(ix, delta)
            .unwrap_or_else(|| match &self.layout_items[ix] {
                DocumentLayoutItem::Text { source, .. }
                | DocumentLayoutItem::Block { source, .. } => (source.start, delta),
                _ => unreachable!("anchored items are text or blocks"),
            })
    }

    fn position_item(&self, position: &DocumentPosition<I>) -> Option<usize> {
        self.block_item(position.node_id())
            .or_else(|| self.text_item_for_position(position))
    }

    /// Node `id`'s block item, found near its source.
    fn block_item(&self, id: &I) -> Option<usize> {
        let region = self.region(id)?;
        self.items_around_source(region.range().start)
            .find(|ix| {
                matches!(&self.layout_items[*ix], DocumentLayoutItem::Block { id: block, .. } if block == id)
            })
    }

    /// The content y of the top of `position`'s line, and its height. A
    /// block's end is its bottom.
    fn content_line(
        &self,
        geometry: &impl ItemGeometry,
        position: &DocumentPosition<I>,
        painted: bool,
    ) -> Option<(Pixels, Pixels)> {
        if let Some(ix) = self.block_item(position.node_id()) {
            let top = geometry.item_top(ix)?;
            let height = geometry.item_height(ix)?;
            let len = self.region(position.node_id())?.range().len();
            return Some(if position.offset() == len && len > 0 {
                (top + height, Pixels::ZERO)
            } else {
                (top, height)
            });
        }
        let ix = self.text_item_for_position(position)?;
        let top = geometry.item_top(ix)?;
        let height = geometry.item_height(ix);
        let DocumentLayoutItem::Text {
            display,
            line,
            frame,
            presentation,
            ..
        } = &self.layout_items[ix]
        else {
            return Some((top, height.unwrap_or_default()));
        };
        if self.text_rows.is_some() {
            if let Some((_, in_row, line)) = self.table_line_for_position(position) {
                return Some((top + in_row, line));
            }
            // Each display row is one line of text: its frame places it.
            return Some(match frame.0 {
                Some(frame) => {
                    let (_, _, inset) = frame.inline_metrics(presentation.as_deref());
                    (
                        top + frame.text_top(display, line) + inset,
                        frame.line_height(),
                    )
                }
                None => (top, height.unwrap_or_default()),
            });
        }
        // A whole source line: its last paint places the position's line.
        let in_item = painted.then_some(()).and_then(|()| {
            let painted = self.viewport.painted.as_ref()?;
            let (_, painted_top) = painted.tops.iter().find(|(item, _)| *item == ix)?;
            let (point, line, _, _) = self.text_position_for_position(position)?;
            Some((point.y - painted.origin - *painted_top, line))
        });
        Some(match in_item {
            Some((offset, line)) => (top + offset, line),
            None => (top, height.unwrap_or_default()),
        })
    }
}

/// Where a pin holds its anchor, as a viewport fraction within `fractions`:
/// as low as lets content ending at `trailing_bottom` end `clearance` above
/// the viewport bottom. Without trailing geometry, the trailing content runs
/// past the viewport.
pub(super) fn hold_fraction(
    anchor_y: Pixels,
    trailing_bottom: Option<Pixels>,
    clearance: Pixels,
    fractions: RangeInclusive<f32>,
    height: Pixels,
) -> f32 {
    let Some(bottom) = trailing_bottom.filter(|_| height > Pixels::ZERO) else {
        return *fractions.start();
    };
    let held = (height - clearance - (bottom - anchor_y)) / height;
    held.clamp(*fractions.start(), *fractions.end())
}

fn same_offset(left: ListOffset, right: ListOffset) -> bool {
    left.item_ix == right.item_ix && left.offset_in_item == right.offset_in_item
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, IntoElement, ListOffset, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window, div, px, size,
    };

    use super::super::{DocumentState, text_rows::TextRows};
    use crate::{
        Theme,
        document::{Affinity, DocumentPosition, DocumentRegion, EditPolicy},
    };

    struct Root(Entity<DocumentState<&'static str>>);

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    /// 60 lines of history and a draft of `draft` lines, in a 300 px viewport.
    fn document(
        draft: usize,
        cx: &mut TestAppContext,
    ) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
        let history = (0..60).map(|ix| format!("line {ix}\n")).collect::<String>();
        let draft = vec!["draft"; draft].join("\n");
        let (history_len, len) = (history.len(), history.len() + draft.len());
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(|cx| {
                    let mut document = DocumentState::new(
                        history + &draft,
                        vec![
                            DocumentRegion::new("history", 0..history_len, EditPolicy::Readonly),
                            DocumentRegion::new("draft", history_len..len, EditPolicy::Editable),
                        ],
                        cx,
                    )
                    .unwrap();
                    document.set_text_rows(Some(TextRows::default()), cx);
                    document.set_dynamic_trailer(true, cx);
                    document
                }));
                cx.new(|_| Root(document.clone().unwrap()))
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(size(px(600.), px(300.)));
        draw(&mut cx);
        (document.unwrap(), cx)
    }

    /// One frame, which must already be right.
    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn fraction(
        document: &Entity<DocumentState<&'static str>>,
        node: &'static str,
        offset: usize,
        cx: &mut VisualTestContext,
    ) -> f32 {
        document.read_with(cx, |document, _| {
            document
                .viewport_fraction(&DocumentPosition::new(node, offset, Affinity::After))
                .unwrap()
        })
    }

    fn scroll_to_end(document: &Entity<DocumentState<&'static str>>, cx: &mut VisualTestContext) {
        document.update(cx, |document, cx| document.scroll_by(px(100_000.), cx));
        draw(cx);
    }

    #[gpui::test]
    fn scrolling_ends_with_the_limit_line_at_the_top_or_the_end_at_the_bottom(
        cx: &mut TestAppContext,
    ) {
        let draft = DocumentPosition::new("draft", 0, Affinity::After);
        for (lines, at_top) in [(1, true), (40, false)] {
            let (document, mut cx) = document(lines, cx);
            document.update(&mut cx, |document, cx| {
                document.set_trailer_limit(Some(draft.clone()), cx)
            });
            draw(&mut cx);
            scroll_to_end(&document, &mut cx);
            document.read_with(&cx, |document, _| {
                let list = document.list_state();
                let first = document.viewport_fraction(&draft);
                if at_top {
                    // A short draft fills the screen from its first line down.
                    assert_eq!(first, Some(0.));
                } else {
                    // A tall one scrolls only until its end reaches the bottom.
                    let end = list.scroll_px_offset_for_scrollbar().y.abs()
                        + list.viewport_bounds().size.height;
                    let content =
                        list.max_offset_for_scrollbar().y + list.viewport_bounds().size.height;
                    assert!((content - end).abs() < px(0.5), "{end:?} {content:?}");
                    assert_eq!(document.trailer_height, Some(px(0.)));
                }
            });
        }
    }

    // The list scrolled itself onto a row without text of its own, then an
    // edit before it spliced the list ahead of the next layout: the viewport
    // stays on that row.
    #[gpui::test]
    fn a_list_scroll_survives_a_splice_before_the_next_layout(cx: &mut TestAppContext) {
        use super::super::DocumentLayoutItem;
        use crate::document::{
            AnchoredBlock, DocumentProjection, DocumentSnapshot, DocumentStyles, EditOrigin,
            EditTransaction, TextEdit,
        };
        let (document, mut cx) = document(1, cx);
        let history = (0..60).map(|ix| format!("line {ix}\n")).collect::<String>();
        let line = |ix: usize| {
            (0..ix)
                .map(|ix| format!("line {ix}\n").len())
                .sum::<usize>()
        };
        let (len, text) = (history.len(), history.clone() + "draft");
        let note = |shift: usize| {
            vec![AnchoredBlock::new(
                "note",
                DocumentPosition::new("history", line(30) + shift, Affinity::Before),
            )]
        };
        document.update(&mut cx, |document, cx| {
            document.set_block_renderer(|_, _, _| div().h(px(45.)).into_any_element(), cx);
            document
                .reset(
                    DocumentSnapshot::new(
                        text.clone(),
                        vec![
                            DocumentRegion::new("history", 0..len, EditPolicy::Readonly),
                            DocumentRegion::new("draft", len..len + 5, EditPolicy::Editable),
                        ],
                        DocumentProjection::identity(len + 5),
                        vec![],
                        DocumentStyles::default(),
                    )
                    .anchored_blocks(note(0)),
                    cx,
                )
                .unwrap();
        });
        draw(&mut cx);
        draw(&mut cx);
        let row = document.read_with(&cx, |document, _| {
            let row = document
                .layout_items
                .iter()
                .position(|item| matches!(item, DocumentLayoutItem::AnchoredBlock { .. }))
                .unwrap();
            document.list_state().scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(10.),
            });
            row
        });
        document.update(&mut cx, |document, cx| {
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..0, "new\n")],
                document.text().len(),
            )
            .unwrap();
            document
                .apply_host_transaction_with_anchored_blocks(
                    transaction,
                    vec![
                        DocumentRegion::new("history", 0..len + 4, EditPolicy::Readonly),
                        DocumentRegion::new("draft", len + 4..len + 9, EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(len + 9),
                    vec![],
                    note(4),
                    DocumentStyles::default(),
                    cx,
                )
                .unwrap();
        });
        draw(&mut cx);
        document.read_with(&cx, |document, _| {
            // One row above it is new; the note is still 10px above the top.
            let top = document.list_state().logical_scroll_top();
            assert!(
                matches!(
                    document.layout_items[row + 1],
                    DocumentLayoutItem::AnchoredBlock { .. }
                ),
                "{row}"
            );
            assert_eq!((top.item_ix, top.offset_in_item), (row + 1, px(10.)));
        });
    }

    #[gpui::test]
    fn a_forward_pin_keeps_the_viewport_until_its_anchor_passes_the_hold(cx: &mut TestAppContext) {
        let (document, mut cx) = document(1, cx);
        document.read_with(&cx, |document, _| {
            document.list_state().scroll_to(ListOffset {
                item_ix: 20,
                offset_in_item: px(0.),
            })
        });
        draw(&mut cx);
        let pin = |offset: usize, cx: &mut VisualTestContext| {
            document.update(cx, |document, cx| {
                document
                    .pin_scroll_forward_above(
                        "run",
                        &DocumentPosition::new("history", offset, Affinity::After),
                        &DocumentPosition::new("draft", 5, Affinity::After),
                        px(0.),
                        0.3..=0.5,
                        cx,
                    )
                    .unwrap()
            });
            draw(cx);
        };
        // "line 22\n" is in view above its hold: nothing moves.
        let line = |ix: usize| {
            (0..ix)
                .map(|ix| format!("line {ix}\n").len())
                .sum::<usize>()
        };
        let hold = |ix: usize, cx: &mut VisualTestContext| {
            document.read_with(cx, |document, _| {
                document
                    .scroll_hold(
                        &DocumentPosition::new("history", line(ix), Affinity::After),
                        &DocumentPosition::new("draft", 5, Affinity::After),
                        px(0.),
                        0.3..=0.5,
                    )
                    .unwrap()
            })
        };
        // The hold reports what the pin will do: keep a line in view above it.
        let above = hold(22, &mut cx);
        assert!(above.is_in_view() && above.offset() < 0., "{above:?}");
        let before = fraction(&document, "history", line(22), &mut cx);
        pin(line(22), &mut cx);
        assert_eq!(fraction(&document, "history", line(22), &mut cx), before);
        assert!(!hold(19, &mut cx).is_in_view());
        // Below the hold, the first frame already holds it there.
        assert!(hold(50, &mut cx).offset() > 0.);
        pin(line(50), &mut cx);
        let held = fraction(&document, "history", line(50), &mut cx);
        let hold = hold(50, &mut cx);
        assert!((held - hold.fraction()).abs() < 0.01, "{held} {hold:?}");
        assert!(hold.offset().abs() < 0.01 && hold.is_in_view(), "{hold:?}");
    }

    #[gpui::test]
    fn a_top_reveal_starts_its_row_at_the_viewport_top_and_reports_the_move(
        cx: &mut TestAppContext,
    ) {
        use std::{cell::Cell, rc::Rc};

        use super::super::DocumentEvent;

        let (document, mut cx) = document(1, cx);
        let moves = Rc::new(Cell::new(0));
        let _subscription = cx.update(|_, cx| {
            let moves = moves.clone();
            cx.subscribe(&document, move |_, event, _| {
                if matches!(event, DocumentEvent::ViewportTopChanged) {
                    moves.set(moves.get() + 1);
                }
            })
        });
        let line = |ix: usize| {
            (0..ix)
                .map(|ix| format!("line {ix}\n").len())
                .sum::<usize>()
        };
        let top = |cx: &mut VisualTestContext| {
            document.read_with(cx, |document, _| {
                document
                    .viewport_top()
                    .map(|top| (*top.node_id(), top.offset()))
            })
        };
        assert_eq!(top(&mut cx), Some(("history", 0)));

        let reveal = |ix: usize, at_top: bool, cx: &mut VisualTestContext| {
            document.update(cx, |document, cx| {
                let position = DocumentPosition::new("history", line(ix), Affinity::After);
                if at_top {
                    document.reveal_position_at_top(&position, cx).unwrap();
                } else {
                    document.reveal_position(&position, cx).unwrap();
                }
            });
            draw(cx);
        };
        reveal(20, true, &mut cx);
        assert_eq!(fraction(&document, "history", line(20), &mut cx), 0.);
        assert_eq!(top(&mut cx), Some(("history", line(20))));
        assert_eq!(moves.get(), 1);

        // Another frame, or revealing a row already in view, moves nothing.
        draw(&mut cx);
        reveal(22, false, &mut cx);
        assert_eq!(top(&mut cx), Some(("history", line(20))));
        assert_eq!(moves.get(), 1);

        // A top reveal moves up as well as down; a scroll reports its move too.
        reveal(5, true, &mut cx);
        assert_eq!(top(&mut cx), Some(("history", line(5))));
        document.update(&mut cx, |document, cx| document.scroll_by(px(200.), cx));
        draw(&mut cx);
        assert_ne!(top(&mut cx), Some(("history", line(5))));
        assert_eq!(moves.get(), 3);
    }
}
