//! Table geometry belongs to the document: each row's on its layout item,
//! column scrolling and layout scheduling here.
mod layout;

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{AppContext as _, Bounds, Context, Pixels, Task, Window, WindowTextSystem, px};

use super::{Affinity, DocumentLayoutItem, DocumentPosition, DocumentState};
pub(in crate::document) use layout::Geometry;
pub(super) use layout::Resolved;
use layout::{Input, Row};

pub(in crate::document) type Key<I> = (Option<I>, usize);

/// Height reserved below an overflowing table's last row. The painted track,
/// its thumb and the pointer target all derive from it.
pub(in crate::document) const SCROLLBAR: Pixels = px(8.);

/// Changed row text laid out synchronously per prepare, to the table's
/// previous column widths, while the complete table is laid out again.
const PROVISIONAL_BYTES: usize = 2048;

/// One table row as the element paints it.
pub(in crate::document) struct View<I> {
    pub geometry: Arc<Geometry>,
    pub key: Key<I>,
    pub offset: Pixels,
}

/// Table state beyond any one row. Each row's geometry lives on its layout
/// item, so edits move it with the row; this keeps what spans a table.
pub(super) struct Tables<I> {
    /// Horizontal scroll offsets, by table.
    offsets: HashMap<Key<I>, Pixels>,
    /// Row counts at the last plan: a table that lost a row needs new widths.
    counts: HashMap<Key<I>, usize>,
    /// Geometry by table and row content, as of the last plan or install.
    /// Rebuilt layout items recover the cells of rows they left unchanged.
    cache: HashMap<(Key<I>, u64), Arc<Resolved>>,
    drag: Option<Drag<I>>,
    /// Some table row may lack geometry for its current text, fonts or width.
    stale: bool,
    work: Vec<Vec<Row>>,
    ready: Option<Ready>,
    pending: Option<Task<()>>,
    request: u64,
    cancel: Arc<AtomicBool>,
}

struct Ready {
    request: u64,
    rows: Vec<(usize, Arc<Resolved>)>,
}

pub(super) struct Drag<I> {
    key: Key<I>,
    start: Pixels,
    offset: Pixels,
    ratio: f32,
    maximum: Pixels,
}

impl<I> Default for Tables<I> {
    fn default() -> Self {
        Self {
            offsets: HashMap::new(),
            counts: HashMap::new(),
            cache: HashMap::new(),
            drag: None,
            stale: true,
            work: Vec::new(),
            ready: None,
            pending: None,
            request: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl<I> Tables<I> {
    /// Table rows were added, replaced or reframed; the next prepare lays
    /// out what no longer matches.
    pub(super) fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(super) fn is_pending(&self) -> bool {
        !self.work.is_empty() || self.pending.is_some() || self.ready.is_some()
    }

    /// Ends a scrollbar drag, reporting whether one was in progress.
    pub(super) fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// Cancels queued and running layout; a later prepare starts again.
    pub(super) fn suspend(&mut self) {
        self.restart();
        self.stale = true;
    }

    fn restart(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.request += 1;
        self.work.clear();
        self.pending = None;
        self.ready = None;
    }

    fn complete(&mut self, request: u64, rows: Option<Vec<(usize, Arc<Resolved>)>>) -> bool {
        if request != self.request {
            return false;
        }
        if let Some(task) = self.pending.take() {
            task.detach();
        }
        self.ready = rows.map(|rows| Ready { request, rows });
        true
    }

    fn offset(&self, key: &Key<I>) -> Pixels
    where
        I: Eq + Hash,
    {
        self.offsets.get(key).copied().unwrap_or_default()
    }
}

impl<I> Drop for Tables<I> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The thumb width, the maximum offset and the offset per thumb pixel.
pub(in crate::document) fn scrollbar(viewport: Pixels, width: Pixels) -> (Pixels, Pixels, f32) {
    let thumb = (viewport * (f32::from(viewport) / f32::from(width.max(px(1.)))))
        .max(px(20.))
        .min(viewport);
    let maximum = (width - viewport).max(px(0.));
    let ratio = f32::from(maximum) / f32::from((viewport - thumb).max(px(1.)));
    (thumb, maximum, ratio)
}

impl<I> DocumentLayoutItem<I> {
    pub(super) fn is_table_row(&self) -> bool {
        matches!(self, DocumentLayoutItem::Text { presentation: Some(presentation), .. }
            if presentation.table_row.is_some())
    }
}

/// A layout item's row of a table.
struct TableItem<'a, I> {
    key: Key<I>,
    /// The line's text and fonts: the row's identity across rebuilt items.
    content: u64,
    geometry: Option<&'a Arc<Resolved>>,
}

fn table_item<I: Clone>(item: &DocumentLayoutItem<I>) -> Option<TableItem<'_, I>> {
    let DocumentLayoutItem::Text {
        node_id,
        presentation: Some(presentation),
        line_key,
        table,
        ..
    } = item
    else {
        return None;
    };
    let row = presentation.table_row.as_ref()?;
    Some(TableItem {
        key: (node_id.clone(), row.table()),
        content: *line_key,
        geometry: table.0.as_ref(),
    })
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    /// The row as the element paints it, once it has geometry.
    pub(super) fn table_view(&self, item: &DocumentLayoutItem<I>) -> Option<View<I>> {
        let row = table_item(item)?;
        Some(View {
            geometry: row.geometry?.geometry.clone(),
            offset: self.tables.offset(&row.key),
            key: row.key,
        })
    }

    pub(super) fn table_line_for_position(
        &self,
        position: &DocumentPosition<I>,
    ) -> Option<(usize, Pixels, Pixels)> {
        let ix = self.text_item_for_position(position)?;
        let item = &self.layout_items[ix];
        let row = table_item(item)?.geometry?;
        let DocumentLayoutItem::Text {
            line,
            display,
            frame,
            ..
        } = item
        else {
            return None;
        };
        let source = self.resolve_position(position).ok()?;
        let at = self
            .model
            .source_to_display(source, position.affinity())?
            .checked_sub(line.start)?;
        let point = row.geometry.position_for_index(at)?;
        Some((
            ix,
            frame
                .0
                .map_or(px(0.), |frame| frame.text_top(display, line))
                + point.y,
            row.geometry.line_height,
        ))
    }

    pub(super) fn table_anchor_in_item(&self, ix: usize, delta: Pixels) -> Option<(usize, Pixels)> {
        let item = &self.layout_items[ix];
        let table = table_item(item)?;
        let row = table.geometry?;
        let DocumentLayoutItem::Text {
            line,
            display,
            frame,
            ..
        } = item
        else {
            return None;
        };
        let geometry = &row.geometry;
        let inset = frame
            .0
            .map_or(px(0.), |frame| frame.text_top(display, line));
        let visual = (f32::from((delta - inset - geometry.padding).max(px(0.)))
            / f32::from(geometry.line_height))
        .floor() as usize;
        let offset = self.tables.offset(&table.key);
        let cell = geometry
            .cells
            .iter()
            .find(|cell| {
                cell.x + cell.width > offset
                    && cell.x < offset + geometry.viewport
                    && cell.lines.len() > visual
            })
            .or_else(|| geometry.cells.iter().max_by_key(|cell| cell.lines.len()))?;
        let visual = visual.min(cell.lines.len().saturating_sub(1));
        let source = self.model.display_to_source(
            line.start + cell.lines.get(visual)?.range.start,
            Affinity::After,
        )?;
        Some((
            source,
            delta - inset - geometry.padding - geometry.line_height * visual as f32,
        ))
    }

    pub(super) fn prepare_tables(
        &mut self,
        bounds: Bounds<Pixels>,
        resized: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if resized || self.tables.stale {
            self.plan_tables(bounds, window, cx);
        }
        if let Some(ready) = self.tables.ready.take()
            && ready.request == self.tables.request
        {
            self.install_table_rows(ready.rows, window);
        }
        let mut bytes = 0;
        let mut cells = 0;
        while self.tables.pending.is_none()
            && let Some(rows) = self.tables.work.pop()
        {
            bytes += rows.iter().map(|row| row.input.text.len()).sum::<usize>();
            cells += rows.iter().map(|row| row.input.cells.len()).sum::<usize>();
            let cancel = self.tables.cancel.clone();
            if bytes <= 2048 && cells <= 32 {
                if let Some(rows) =
                    smol::block_on(layout::compute(rows, window.text_system(), &cancel))
                {
                    self.install_table_rows(rows, window);
                }
            } else {
                let system = cx.text_system().clone();
                let request = self.tables.request;
                self.tables.pending = Some(cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            let system = WindowTextSystem::new(system);
                            layout::compute(rows, &system, &cancel).await
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.tables.complete(request, result) {
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }

    /// Freezes every table's inputs and queues the tables whose rows no
    /// longer match their geometry. A changed row is laid out at once to its
    /// table's previous column widths, within a budget, so its text stays
    /// visible; otherwise it takes a placeholder until its table is laid out.
    fn plan_tables(&mut self, bounds: Bounds<Pixels>, window: &Window, cx: &mut Context<Self>) {
        self.tables.stale = false;
        self.tables.restart();
        let style = window.text_style();
        let padding = window.pixel_snap(style.font_size.to_pixels(window.rem_size()) * 0.6);
        // Adjacent rows of one table, in order.
        let mut tables: Vec<(Key<I>, Vec<Row>)> = Vec::new();
        for (ix, item) in self.layout_items.iter().enumerate() {
            let Some(TableItem {
                key,
                content,
                geometry,
            }) = table_item(item)
            else {
                continue;
            };
            let DocumentLayoutItem::Text {
                line,
                presentation: Some(presentation),
                ..
            } = item
            else {
                continue;
            };
            let frame = self.resolve_line_frame(item, item, &style, window, cx);
            let row = Row {
                item: ix,
                content,
                input: Arc::new(Input::new(
                    self.model.display_text()[line.clone()].into(),
                    presentation,
                    &style,
                    window.rem_size(),
                    frame.line_height(),
                    padding,
                    frame.widths(bounds.size.width).0,
                )),
                previous: geometry
                    .or_else(|| self.tables.cache.get(&(key.clone(), content)))
                    .cloned(),
            };
            match tables.last_mut() {
                Some((last, rows)) if *last == key => rows.push(row),
                _ => tables.push((key, vec![row])),
            }
        }
        let counts = tables
            .iter()
            .map(|(key, rows)| (key.clone(), rows.len()))
            .collect();
        let counts = std::mem::replace(&mut self.tables.counts, counts);
        let live = &self.tables.counts;
        self.tables.offsets.retain(|key, _| live.contains_key(key));
        if self
            .tables
            .drag
            .as_ref()
            .is_some_and(|drag| !live.contains_key(&drag.key))
        {
            self.tables.drag = None;
        }
        let focus = self
            .reveal_target()
            .or_else(|| self.viewport_top_offset())
            .and_then(|source| self.model.source_to_display(source, Affinity::After))
            .map(|at| self.items_around_display(at).start);
        let mut cache = HashMap::new();
        let mut provisional = PROVISIONAL_BYTES;
        let mut work = Vec::new();
        for (key, mut rows) in tables {
            let last = rows.len() - 1;
            let unchanged = counts.get(&key) == Some(&rows.len())
                && rows.iter().enumerate().all(|(ix, row)| {
                    row.previous.as_ref().is_some_and(|old| {
                        !old.provisional
                            && old.input == row.input
                            && old.geometry.fits(ix == 0, ix == last)
                    })
                });
            let widths = rows.iter().find_map(|row| {
                let old = row.previous.as_ref().filter(|old| old.is_measured())?;
                Some(
                    old.geometry
                        .cells
                        .iter()
                        .map(|cell| cell.width)
                        .collect::<Vec<_>>(),
                )
            });
            for (ix, row) in rows.iter_mut().enumerate() {
                let kept = row
                    .previous
                    .clone()
                    .filter(|old| unchanged || old.reusable(&row.input));
                let shown = kept.unwrap_or_else(|| {
                    let laid = widths
                        .as_ref()
                        .filter(|_| row.input.text.len() <= provisional)
                        .and_then(|widths| {
                            provisional -= row.input.text.len();
                            layout::provisional(
                                row.input.clone(),
                                widths,
                                ix == 0,
                                ix == last,
                                window.text_system(),
                            )
                        })
                        .map(Arc::new);
                    if laid.is_some() {
                        row.previous = laid.clone();
                    }
                    laid.unwrap_or_else(|| Arc::new(Resolved::pending(row.input.clone(), ix == 0)))
                });
                cache.insert((key.clone(), row.content), shown.clone());
                self.set_table_geometry(row.item, shown);
            }
            if !unchanged {
                work.push(rows);
            }
        }
        self.tables.cache = cache;
        // Popped from the end: the table being read or revealed first, then
        // in document order.
        work.sort_by_key(|rows| {
            let (first, last) = (rows[0].item, rows[rows.len() - 1].item);
            (!focus.is_some_and(|at| first <= at && at <= last), first)
        });
        work.reverse();
        self.tables.work = work;
    }

    /// Installs a laid-out table. A row whose item has since changed text or
    /// moved is left for the next plan.
    fn install_table_rows(&mut self, rows: Vec<(usize, Arc<Resolved>)>, window: &Window) {
        for (ix, row) in rows {
            let Some(item) = self.layout_items.get(ix) else {
                self.tables.stale = true;
                continue;
            };
            let current = match (table_item(item), item) {
                (Some(table), DocumentLayoutItem::Text { line, .. })
                    if self.model.display_text()[line.clone()] == *row.input.text =>
                {
                    Some((table.key, table.content))
                }
                _ => None,
            };
            let Some((key, content)) = current else {
                self.tables.stale = true;
                continue;
            };
            self.tables
                .cache
                .insert((key.clone(), content), row.clone());
            let geometry = &row.geometry;
            let (_, maximum, ratio) = scrollbar(geometry.viewport, geometry.width);
            let offset = self.tables.offsets.entry(key.clone()).or_default();
            *offset = (*offset).clamp(px(0.), maximum);
            if let Some(drag) = self.tables.drag.as_mut().filter(|drag| drag.key == key)
                && (maximum != drag.maximum || ratio != drag.ratio)
            {
                drag.offset = *offset;
                drag.start = window.mouse_position().x;
                drag.maximum = maximum;
                drag.ratio = ratio;
            }
            self.set_table_geometry(ix, row);
        }
    }

    /// Gives a row new geometry; a different geometry reframes the row.
    fn set_table_geometry(&mut self, ix: usize, resolved: Arc<Resolved>) {
        if self
            .layout_items
            .get(ix)
            .and_then(table_item)
            .and_then(|row| row.geometry)
            .is_some_and(|old| Arc::ptr_eq(old, &resolved))
        {
            return;
        }
        let mut reframe = false;
        self.layout_items.update(ix, |item| {
            if let DocumentLayoutItem::Text { table, frame, .. } = item {
                reframe = table
                    .0
                    .as_ref()
                    .is_none_or(|old| old.geometry != resolved.geometry);
                if reframe {
                    frame.0 = None;
                }
                table.0 = Some(resolved);
            }
        });
        if reframe {
            self.resolve_dropped_frames();
        }
    }

    pub(in crate::document) fn scroll_table(
        &mut self,
        key: &Key<I>,
        delta: Pixels,
        max: Pixels,
        cx: &mut Context<Self>,
    ) {
        let offset = self.tables.offsets.entry(key.clone()).or_default();
        let next = (*offset + delta).clamp(px(0.), max.max(px(0.)));
        // Columns move under a fixed vertical viewport: following and pending
        // reveals are unaffected.
        if next != *offset {
            *offset = next;
            self.hovered_link = None;
            cx.notify();
        }
    }

    pub(in crate::document) fn begin_table_drag(
        &mut self,
        key: Key<I>,
        point: gpui::Point<Pixels>,
        bounds: Bounds<Pixels>,
        width: Pixels,
        cx: &mut Context<Self>,
    ) {
        let viewport = bounds.size.width;
        let (thumb, maximum, ratio) = scrollbar(viewport, width);
        let offset = self.tables.offset(&key);
        let left = bounds.left() + offset / ratio.max(f32::EPSILON);
        if point.x < left || point.x > left + thumb {
            let next = (point.x - bounds.left() - thumb / 2.) * ratio;
            self.scroll_table(&key, next - offset, maximum, cx);
        }
        self.auto_scroll.stop();
        self.pressed_link = None;
        self.tables.drag = Some(Drag {
            offset: self.tables.offset(&key),
            key,
            start: point.x,
            maximum,
            ratio,
        });
        cx.notify();
    }

    pub(super) fn drag_table(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = &self.tables.drag else {
            return false;
        };
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.tables.drag = None;
            return false;
        }
        let key = drag.key.clone();
        let current = self.tables.offset(&key);
        let next = drag.offset + (event.position.x - drag.start) * drag.ratio;
        self.scroll_table(&key, next - current, drag.maximum, cx);
        true
    }
}

#[cfg(test)]
mod tests;
