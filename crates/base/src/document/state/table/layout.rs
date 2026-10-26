use std::{
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    Font, Pixels, Refineable, SharedString, TextAlign, TextRun, TextStyle, WindowTextSystem, px,
};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    super::{DocumentTextPresentation, text_rows::font_runs::font_runs},
    SCROLLBAR,
};

/// Frozen font and text inputs; neither a Window nor a document entity crosses
/// the worker boundary. Paint-only changes leave these inputs unchanged.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Input {
    pub text: SharedString,
    fonts: Vec<(Range<usize>, Font, Pixels)>,
    pub cells: Vec<(Range<usize>, TextAlign)>,
    pub line_height: Pixels,
    pub padding: Pixels,
    pub viewport: Pixels,
    pub key: u64,
}

impl Input {
    pub fn new(
        text: SharedString,
        presentation: &DocumentTextPresentation,
        base: &TextStyle,
        rem: Pixels,
        line_height: Pixels,
        padding: Pixels,
        viewport: Pixels,
    ) -> Self {
        let mut style = base.clone();
        if let Some(refinement) = &presentation.text_style {
            style.refine(refinement);
        }
        let size = style.font_size.to_pixels(rem);
        let fonts: Vec<_> = font_runs(presentation, text.len())
            .map(|(range, font)| {
                let mut face = style.font();
                font.apply(&mut face);
                (
                    range,
                    face,
                    size * (f32::from(font.scale.unwrap_or(100)) / 100.),
                )
            })
            .collect();
        let cells = presentation.table_row.as_ref().unwrap().cells().to_vec();
        let mut hash = DefaultHasher::new();
        text.hash(&mut hash);
        for (range, font, size) in &fonts {
            range.hash(&mut hash);
            font.hash(&mut hash);
            f32::from(*size).to_bits().hash(&mut hash);
        }
        for (range, _) in &cells {
            range.hash(&mut hash);
        }
        Self {
            text,
            fonts,
            cells,
            line_height,
            padding,
            viewport,
            key: hash.finish(),
        }
    }

    fn same_measurement(&self, other: &Self) -> bool {
        self.key == other.key
            && self.text == other.text
            && self.fonts == other.fonts
            && self
                .cells
                .iter()
                .map(|(range, _)| range)
                .eq(other.cells.iter().map(|(range, _)| range))
    }
}

type Advances = Vec<(Range<usize>, Pixels)>;

/// A row to lay out: its layout item, frozen inputs and earlier geometry.
pub(super) struct Row {
    pub item: usize,
    /// The item's line key, under which the table caches the row.
    pub content: u64,
    pub input: Arc<Input>,
    pub previous: Option<Arc<Resolved>>,
}

#[derive(Debug, PartialEq)]
pub(in crate::document) struct Resolved {
    pub(super) input: Arc<Input>,
    measured: Option<Arc<Vec<Advances>>>,
    pub geometry: Arc<Geometry>,
    /// Laid out to an earlier layout's column widths; the table still needs
    /// a complete layout before it counts as unchanged.
    pub(super) provisional: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::document) struct Geometry {
    pub cells: Vec<Cell>,
    pub width: Pixels,
    pub viewport: Pixels,
    pub height: Pixels,
    pub padding: Pixels,
    pub line_height: Pixels,
    /// The table's first row draws the top border.
    pub first: bool,
    pub scrollbar: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::document) struct Cell {
    pub x: Pixels,
    pub width: Pixels,
    pub lines: Vec<Line>,
    pub alignment: TextAlign,
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::document) struct Line {
    pub range: Range<usize>,
    pub width: Pixels,
    positions: Box<[(usize, Pixels)]>,
}

impl Line {
    fn new(range: Range<usize>, advances: &[(Range<usize>, Pixels)]) -> Self {
        let mut x = px(0.);
        let positions = std::iter::once((0, x))
            .chain(advances.iter().map(|(span, width)| {
                x += *width;
                (span.end, x)
            }))
            .collect();
        Self {
            range,
            width: x,
            positions,
        }
    }

    pub fn x_for_index(&self, ix: usize) -> Pixels {
        let local = ix.saturating_sub(self.range.start);
        self.positions[self
            .positions
            .partition_point(|(at, _)| *at <= local)
            .saturating_sub(1)]
        .1
    }

    pub fn closest_index(&self, x: Pixels) -> usize {
        self.range.start
            + self
                .positions
                .iter()
                .min_by(|(_, a), (_, b)| {
                    f32::from((*a - x).abs()).total_cmp(&f32::from((*b - x).abs()))
                })
                .map_or(0, |(at, _)| *at)
    }

    pub fn index_at(&self, x: Pixels) -> Option<usize> {
        self.positions
            .windows(2)
            .find(|pair| pair[0].1 <= x && x < pair[1].1)
            .map(|pair| self.range.start + pair[0].0)
    }
}

impl Cell {
    pub fn text_x(&self, line: &Line, padding: Pixels) -> Pixels {
        let gap = (self.width - padding * 2. - line.width).max(px(0.));
        self.x
            + padding
            + match self.alignment {
                TextAlign::Center => gap / 2.,
                TextAlign::Right => gap,
                _ => px(0.),
            }
    }
}

impl Geometry {
    /// Whether the geometry was placed as row `first`/`last` of its table:
    /// the first row draws the top border, the last the scrollbar.
    pub fn fits(&self, first: bool, last: bool) -> bool {
        self.first == first && self.scrollbar == (last && self.width > self.viewport)
    }

    pub fn position_for_index(&self, ix: usize) -> Option<gpui::Point<Pixels>> {
        for cell in &self.cells {
            let row = cell
                .lines
                .partition_point(|line| line.range.end <= ix && !line.range.is_empty());
            let row = row.min(cell.lines.len().saturating_sub(1));
            let line = cell.lines.get(row)?;
            if line.range.start <= ix && ix <= line.range.end {
                return Some(gpui::point(
                    cell.text_x(line, self.padding) + line.x_for_index(ix),
                    self.padding + self.line_height * row as f32,
                ));
            }
        }
        None
    }

    pub fn cell_at(&self, x: Pixels) -> Option<&Cell> {
        self.cells
            .iter()
            .find(|cell| cell.x <= x && x < cell.x + cell.width)
            .or_else(|| {
                self.cells.iter().min_by(|a, b| {
                    let distance = |cell: &Cell| {
                        f32::from((cell.x - x).max(x - cell.x - cell.width).max(px(0.)))
                    };
                    distance(a).total_cmp(&distance(b))
                })
            })
    }

    pub fn line_at<'a>(&self, cell: &'a Cell, y: Pixels) -> Option<&'a Line> {
        let ix = (f32::from((y - self.padding).max(px(0.))) / f32::from(self.line_height)).floor()
            as usize;
        cell.lines.get(ix.min(cell.lines.len().saturating_sub(1)))
    }
}

impl Resolved {
    pub(super) fn pending(input: Arc<Input>, first: bool) -> Self {
        let width = (input.viewport / input.cells.len().max(1) as f32).max(input.padding * 7.);
        let cells = input
            .cells
            .iter()
            .enumerate()
            .map(|(ix, (range, alignment))| Cell {
                x: width * ix as f32,
                width,
                lines: vec![Line::new(range.start..range.start, &[])],
                alignment: *alignment,
            })
            .collect();
        let geometry = Arc::new(Geometry {
            cells,
            width: width * input.cells.len() as f32,
            viewport: input.viewport,
            height: input.line_height + input.padding * 2.,
            padding: input.padding,
            line_height: input.line_height,
            first,
            scrollbar: false,
        });
        Self {
            input,
            measured: None,
            geometry,
            provisional: false,
        }
    }

    pub(super) fn is_measured(&self) -> bool {
        self.measured.is_some()
    }

    /// The same layout, now confirmed by a complete table layout.
    fn settled(self: Arc<Self>) -> Arc<Self> {
        if !self.provisional {
            return self;
        }
        Arc::new(Self {
            input: self.input.clone(),
            measured: self.measured.clone(),
            geometry: self.geometry.clone(),
            provisional: false,
        })
    }

    pub(super) fn reusable(&self, input: &Input) -> bool {
        self.measured.is_some()
            && self.input.same_measurement(input)
            && self.input.cells == input.cells
            && self.input.line_height == input.line_height
            && self.input.padding == input.padding
    }
}

/// Every shaping request and cancellation interval is at most 4 KiB, except
/// an indivisible cluster; the text system bounds that cluster's displayed form.
fn measure(
    input: &Input,
    range: Range<usize>,
    system: &WindowTextSystem,
    cancel: &AtomicBool,
) -> Option<Advances> {
    let mut result = Vec::new();
    for (run, font, size) in input.fonts[input
        .fonts
        .partition_point(|(run, _, _)| run.end <= range.start)..]
        .iter()
        .take_while(|(run, _, _)| run.start < range.end)
    {
        let mut start = run.start.max(range.start);
        let end = run.end.min(range.end);
        while start < end {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let mut next = start;
            for (at, cluster) in input.text[start..end].grapheme_indices(true) {
                if at > 0 && at + cluster.len() > 4096 {
                    break;
                }
                next = start + at + cluster.len();
            }
            let text = &input.text[start..next];
            let line = system.shape_line(
                text.to_owned().into(),
                *size,
                &[TextRun {
                    len: text.len(),
                    font: font.clone(),
                    ..Default::default()
                }],
                None,
            );
            for (at, cluster) in text.grapheme_indices(true) {
                let end = at + cluster.len();
                result.push((
                    start + at - range.start..start + end - range.start,
                    line.x_for_index(end) - line.x_for_index(at),
                ));
            }
            start = next;
        }
    }
    Some(result)
}

fn wrapped(
    input: &Input,
    range: Range<usize>,
    advances: &[(Range<usize>, Pixels)],
    width: Pixels,
    system: &WindowTextSystem,
    cancel: &AtomicBool,
) -> Option<Vec<Line>> {
    let limit = width + px(0.01);
    let mut lines = Vec::new();
    let mut start = 0;
    while start < advances.len() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut end = start;
        let mut used = px(0.);
        let mut word = None;
        while let Some((span, advance)) = advances.get(end) {
            if end > start && (used + *advance > limit || span.end - advances[start].0.start > 4096)
            {
                break;
            }
            used += *advance;
            end += 1;
            if input.text[range.start + span.start..range.start + span.end]
                .chars()
                .all(char::is_whitespace)
            {
                word = Some(end);
            }
        }
        if end < advances.len()
            && let Some(boundary) = word
        {
            end = boundary;
        }
        let line = loop {
            let span = range.start + advances[start].0.start..range.start + advances[end - 1].0.end;
            let measured = measure(input, span.clone(), system, cancel)?;
            let line = Line::new(span, &measured);
            if line.width <= limit || end == start + 1 {
                break line;
            }
            end -= 1;
        };
        lines.push(line);
        start = end;
    }
    if lines.is_empty() {
        lines.push(Line::new(range.start..range.start, &[]));
    }
    Some(lines)
}

pub(super) async fn compute(
    rows: Vec<Row>,
    system: &WindowTextSystem,
    cancel: &AtomicBool,
) -> Option<Vec<(usize, Arc<Resolved>)>> {
    let mut result = Vec::new();
    let mut widths: Vec<Pixels> = Vec::new();
    let mut minimums: Vec<Pixels> = Vec::new();
    let mut measured = Vec::new();
    let mut available = px(f32::MAX);
    for row in &rows {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let input = &row.input;
        available = available.min(input.viewport);
        let cached = row
            .previous
            .as_ref()
            .filter(|previous| previous.input.same_measurement(input))
            .and_then(|previous| previous.measured.clone());
        let cells = match cached {
            Some(cells) => cells,
            None => Arc::new(
                input
                    .cells
                    .iter()
                    .map(|(range, _)| measure(input, range.clone(), system, cancel))
                    .collect::<Option<Vec<_>>>()?,
            ),
        };
        widths.resize(widths.len().max(cells.len()), px(0.));
        minimums.resize(widths.len(), px(0.));
        for (column, advances) in cells.iter().enumerate() {
            let natural: Pixels = advances.iter().map(|(_, width)| *width).sum();
            let widest = advances
                .iter()
                .map(|(_, width)| *width)
                .fold(px(0.), Pixels::max);
            minimums[column] = minimums[column]
                .max(widest + input.padding * 2.)
                .max(input.padding * 7.);
            widths[column] = widths[column]
                .max(natural + input.padding * 2.)
                .max(minimums[column]);
        }
        measured.push(cells);
        smol::future::yield_now().await;
    }
    let natural: Pixels = widths.iter().copied().sum();
    let minimum: Pixels = minimums.iter().copied().sum();
    if natural > available && natural > minimum {
        let proportion =
            (f32::from(available - minimum) / f32::from(natural - minimum)).clamp(0., 1.);
        for (width, min) in widths.iter_mut().zip(&minimums) {
            *width = *min + (*width - *min) * proportion;
        }
    }
    let total: Pixels = widths.iter().copied().sum();
    let last = rows.len().saturating_sub(1);
    for (ix, (row, measured)) in rows.into_iter().zip(measured).enumerate() {
        let input = row.input;
        let scrollbar = ix == last && total > available;
        if let Some(previous) = row.previous.filter(|previous| {
            previous.reusable(&input)
                && previous.geometry.viewport == available
                && previous
                    .geometry
                    .cells
                    .iter()
                    .map(|cell| cell.width)
                    .eq(widths.iter().copied())
                && previous.geometry.fits(ix == 0, ix == last)
        }) {
            result.push((row.item, previous.settled()));
            continue;
        }
        let placement = Placement {
            widths: &widths,
            viewport: available,
            first: ix == 0,
            scrollbar,
        };
        let resolved = place(input, measured, placement, system, cancel)?;
        result.push((row.item, Arc::new(resolved)));
        smol::future::yield_now().await;
    }
    Some(result)
}

/// Lays out a changed row synchronously to an earlier layout's column widths,
/// so its text stays visible until its table is laid out again.
pub(super) fn provisional(
    input: Arc<Input>,
    widths: &[Pixels],
    first: bool,
    last: bool,
    system: &WindowTextSystem,
) -> Option<Resolved> {
    if widths.len() != input.cells.len() {
        return None;
    }
    let cancel = AtomicBool::new(false);
    let measured = input
        .cells
        .iter()
        .map(|(range, _)| measure(&input, range.clone(), system, &cancel))
        .collect::<Option<Vec<_>>>()?;
    let placement = Placement {
        widths,
        viewport: input.viewport,
        first,
        scrollbar: last && widths.iter().copied().sum::<Pixels>() > input.viewport,
    };
    let mut resolved = place(input, Arc::new(measured), placement, system, &cancel)?;
    resolved.provisional = true;
    Some(resolved)
}

/// Where a row sits among its table's columns and rows.
struct Placement<'a> {
    widths: &'a [Pixels],
    viewport: Pixels,
    first: bool,
    scrollbar: bool,
}

/// Wraps one measured row to the table's column widths.
fn place(
    input: Arc<Input>,
    measured: Arc<Vec<Advances>>,
    placement: Placement,
    system: &WindowTextSystem,
    cancel: &AtomicBool,
) -> Option<Resolved> {
    let Placement {
        widths,
        viewport,
        first,
        scrollbar,
    } = placement;
    let mut x = px(0.);
    let mut height = 1;
    let mut cells = Vec::new();
    for ((range, alignment), (width, advances)) in
        input.cells.iter().zip(widths.iter().zip(measured.iter()))
    {
        let lines = wrapped(
            &input,
            range.clone(),
            advances,
            (*width - input.padding * 2.).max(px(1.)),
            system,
            cancel,
        )?;
        height = height.max(lines.len());
        cells.push(Cell {
            x,
            width: *width,
            lines,
            alignment: *alignment,
        });
        x += *width;
    }
    let geometry = Arc::new(Geometry {
        cells,
        width: widths.iter().copied().sum(),
        viewport,
        height: input.line_height * height as f32
            + input.padding * 2.
            + if scrollbar { SCROLLBAR } else { px(0.) },
        padding: input.padding,
        line_height: input.line_height,
        first,
        scrollbar,
    });
    Some(Resolved {
        input,
        measured: Some(measured),
        geometry,
        provisional: false,
    })
}
