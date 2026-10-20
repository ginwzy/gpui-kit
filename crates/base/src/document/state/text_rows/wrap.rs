//! Resumable greedy cluster wrapper. Cluster advances propose a row; shaping
//! validates its end before publication. Checkpoints retain unfinished scans.
//! Cluster advances stay in GPUI's font/size keyed LineWrapper pool; immutable
//! shaped measurements use TextSystem's bounded cache across width changes.
use gpui::{LineWrapper, LineWrapperHandle, Pixels, px};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{MAX_ROW_BYTES, advance};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Checkpoint {
    next: usize,
    row: usize,
    x: Pixels,
    previous: char,
    indented: bool,
}

impl Checkpoint {
    fn shifted(mut self, delta: isize) -> Self {
        self.next = self
            .next
            .checked_add_signed(delta)
            .expect("retained suffix offset");
        self.row = self
            .row
            .checked_add_signed(delta)
            .expect("retained suffix row");
        self
    }
}

#[derive(Clone, Debug)]
struct Part {
    points: Arc<[Checkpoint]>,
    range: Range<usize>,
    delta: isize,
}

/// Shared ranges of checkpoint arrays. Editing copies only the recomputed
/// middle; the converged suffix retains its original allocation.
#[derive(Clone, Default, Debug)]
pub(in crate::document) struct Breaks {
    parts: Vec<Part>,
    len: usize,
}

impl Breaks {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn starts(&self) -> impl DoubleEndedIterator<Item = u32> + Clone + '_ {
        self.parts.iter().flat_map(|part| {
            part.points[part.range.clone()]
                .iter()
                .map(move |point| point.row.checked_add_signed(part.delta).unwrap() as u32)
        })
    }
    pub fn start(&self, ix: usize) -> usize {
        self.point(ix).row
    }
    fn point(&self, mut ix: usize) -> Checkpoint {
        for part in &self.parts {
            if ix < part.range.len() {
                return part.points[part.range.start + ix].shifted(part.delta);
            }
            ix -= part.range.len();
        }
        panic!("wrap checkpoint index");
    }
    fn partition_point(&self, predicate: impl Fn(Checkpoint) -> bool) -> usize {
        let (mut lo, mut hi) = (0, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if predicate(self.point(mid)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
    fn append_range(&mut self, other: &Self, range: Range<usize>, delta: isize) {
        let mut from = 0;
        for part in &other.parts {
            let start = range.start.max(from);
            let end = range.end.min(from + part.range.len());
            if start < end {
                self.parts.push(Part {
                    points: part.points.clone(),
                    range: part.range.start + start - from..part.range.start + end - from,
                    delta: part.delta + delta,
                });
                self.len += end - start;
            }
            from += part.range.len();
        }
    }
    fn append_points(&mut self, points: Vec<Checkpoint>) {
        if points.is_empty() {
            return;
        }
        let len = points.len();
        self.parts.push(Part {
            points: points.into(),
            range: 0..len,
            delta: 0,
        });
        self.len += len;
    }
}

#[derive(Clone)]
pub(super) struct Previous {
    pub text: Arc<str>,
    pub range: Range<usize>,
    pub breaks: Breaks,
    pub runs: Arc<[(usize, usize)]>,
    pub widths: (Pixels, Pixels),
    pub fonts_key: u64,
    pub change: Option<(usize, usize)>,
}

pub(super) struct Wrap {
    point: Checkpoint,
    candidate: Option<usize>,
    widths: (Pixels, Pixels),
    prefix: Breaks,
    points: Vec<Checkpoint>,
    reuse: Option<(Previous, usize, isize)>,
    complete: bool,
    pub processed: usize,
    pub reused: usize,
    objects: Vec<(Range<usize>, Pixels)>,
}

impl Wrap {
    pub fn new(
        line: &str,
        runs: &[(usize, usize)],
        widths: (Pixels, Pixels),
        previous: Option<Previous>,
    ) -> Self {
        let mut this = Self {
            point: Checkpoint {
                next: 0,
                row: 0,
                x: px(0.),
                previous: '\0',
                indented: false,
            },
            candidate: None,
            widths,
            prefix: Breaks::default(),
            points: Vec::new(),
            reuse: None,
            complete: false,
            processed: 0,
            reused: 0,
            objects: Vec::new(),
        };
        if let Some(old) = previous.filter(|old| old.widths == widths) {
            let text = &old.text[old.range.clone()];
            let (prefix, suffix) = old.change.unwrap_or_else(|| {
                let prefix = text
                    .bytes()
                    .zip(line.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                let suffix = text.as_bytes()[prefix..]
                    .iter()
                    .rev()
                    .zip(line.as_bytes()[prefix..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                (prefix, suffix)
            });
            let delta = line.len() as isize - text.len() as isize;
            // Font-run endpoints follow the existing edit projection. Reuse
            // only when the unchanged prefix and shifted suffix retain every
            // font boundary. Runs inside the changed interval fall back to a
            // complete wrap. The caller also validates actual font identities.
            if old.runs.len() != runs.len()
                || !old
                    .runs
                    .iter()
                    .zip(runs)
                    .all(|((end, font), (new_end, new_font))| {
                        font == new_font
                            && ((*end <= prefix && end == new_end)
                                || (*end >= text.len() - suffix
                                    && end.checked_add_signed(delta) == Some(*new_end)))
                    })
            {
                return this;
            }
            // Restart before the cluster adjoining the edit, since insertion
            // can join it (combining marks, ZWJ, regional indicators).
            let restart = text[..text.floor_char_boundary(prefix)]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(ix, _)| ix);
            let keep = old.breaks.partition_point(|point| point.next < restart);
            if keep > 0 {
                this.point = old.breaks.point(keep - 1);
                this.prefix.append_range(&old.breaks, 0..keep, 0);
            }
            this.reuse = Some((old, line.len() - suffix, delta));
        }
        this
    }

    pub fn with_objects(mut self, objects: Vec<(Range<usize>, Pixels)>) -> Self {
        self.objects = objects;
        self
    }

    /// Consume a bounded number of input bytes. A cluster is indivisible;
    /// callers route larger inputs to a cancellable background task.
    pub fn step(
        &mut self,
        line: &str,
        runs: &[(usize, usize)],
        wrappers: &mut [LineWrapperHandle],
        budget: usize,
        cancelled: &AtomicBool,
    ) -> bool {
        if self.complete {
            return true;
        }
        let font_at = |ix| {
            runs.get(runs.partition_point(|(end, _)| *end <= ix))
                .or(runs.last())
                .map_or(0, |(_, font)| *font)
        };
        let stop = self.point.next.saturating_add(budget);
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return false;
            }
            let ix = self.point.next;
            if ix == line.len() {
                let Some(end) = self.fitting_end(line, runs, wrappers, ix, cancelled) else {
                    return false;
                };
                if end == ix {
                    self.complete = true;
                    return true;
                }
                if self.start_row(line, end) {
                    return true;
                }
                continue;
            }
            if ix >= stop {
                return false;
            }
            let (_, cluster) = Clusters::new(line, &self.objects, ix..line.len())
                .next()
                .expect("remaining cluster");
            let first = cluster.chars().next().unwrap_or_default();
            if can_break(
                self.point.previous,
                first,
                self.point.indented,
                !self.objects.is_empty(),
            ) {
                self.candidate = Some(ix);
            }
            self.point.indented |= first != ' ';
            let object = self
                .objects
                .get(self.objects.partition_point(|(range, _)| range.end <= ix));
            let amount = object.filter(|(range, _)| range.start == ix).map_or_else(
                || advance(&mut wrappers[font_at(ix)], cluster, self.point.x),
                |(_, width)| *width,
            );
            let width = if self.prefix.is_empty() && self.points.is_empty() {
                self.widths.0
            } else {
                self.widths.1
            };
            let end = ix + cluster.len();
            let broke = (self.point.x + amount > width || end - self.point.row > MAX_ROW_BYTES)
                && ix > self.point.row;
            if broke {
                let row = self.point.row;
                let proposed = self
                    .candidate
                    .filter(|start| *start > row && end - start <= MAX_ROW_BYTES)
                    .unwrap_or(ix);
                let Some(start) = self.fitting_end(line, runs, wrappers, proposed, cancelled)
                else {
                    return false;
                };
                if self.start_row(line, start) {
                    return true;
                }
                continue;
            }
            self.point.x += amount;
            self.point.previous = cluster.chars().last().unwrap_or(first);
            self.point.next = ix + cluster.len();
            self.processed += cluster.len();
        }
    }

    fn start_row(&mut self, line: &str, start: usize) -> bool {
        self.point.next = start;
        self.point.row = start;
        self.point.x = px(0.);
        self.point.previous = line[..start].chars().next_back().unwrap_or_default();
        self.candidate = None;
        self.points.push(self.point);
        if let Some((old, suffix, delta)) = &self.reuse
            && start >= *suffix
        {
            let old_next = start.checked_add_signed(-*delta).unwrap();
            let at = old.breaks.partition_point(|point| point.next < old_next);
            if at < old.breaks.len() && old.breaks.point(at).shifted(*delta) == self.point {
                self.prefix.append_points(std::mem::take(&mut self.points));
                self.prefix
                    .append_range(&old.breaks, at + 1..old.breaks.len(), *delta);
                self.reused = old.breaks.len() - at - 1;
                self.complete = true;
                return true;
            }
        }
        false
    }

    /// Only shape one bounded row, including its actual font runs. On overflow
    /// find a fitting cluster boundary, then prefer the preceding word boundary.
    /// The first cluster/object remains indivisible even in a zero-width view.
    fn fitting_end(
        &self,
        line: &str,
        runs: &[(usize, usize)],
        wrappers: &[LineWrapperHandle],
        proposed: usize,
        cancelled: &AtomicBool,
    ) -> Option<usize> {
        let start = self.point.row;
        let width = if self.prefix.is_empty() && self.points.is_empty() {
            self.widths.0
        } else {
            self.widths.1
        };
        // The checkpoint at this row's start records whether the physical line
        // has passed its indentation. The current scan may already have done so.
        let row_indented = self
            .points
            .last()
            .copied()
            .or_else(|| (!self.prefix.is_empty()).then(|| self.prefix.point(self.prefix.len() - 1)))
            .is_some_and(|point| point.indented);
        let mut end = proposed;
        let mut clusters = None;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let (drawn, fit) = measure_row(
                line,
                runs,
                wrappers,
                &self.objects,
                start..end,
                width + px(0.01),
            );
            if drawn <= width + px(0.01) {
                return Some(end);
            }
            let clusters = clusters.get_or_insert_with(|| {
                Clusters::new(line, &self.objects, start..proposed).collect::<Vec<_>>()
            });
            let first_end = clusters[0].0 + clusters[0].1.len();
            if end == first_end {
                return Some(end);
            }
            // Glyph positions give a close proposal without shaping log(n)
            // different prefixes. Still revalidate the final context, and always
            // retreat at least one cluster if a shaper returns a non-progressing hint.
            let count = clusters
                .partition_point(|(ix, cluster)| {
                    *ix + cluster.len() <= fit && *ix + cluster.len() < end
                })
                .max(1);
            end = clusters[count - 1].0 + clusters[count - 1].1.len();
            let mut indented = row_indented;
            let mut previous = line[..start].chars().next_back().unwrap_or_default();
            let mut preferred = None;
            for &(ix, cluster) in clusters.iter().take_while(|(ix, _)| *ix <= end) {
                let first = cluster.chars().next().unwrap_or_default();
                if ix > start && can_break(previous, first, indented, !self.objects.is_empty()) {
                    preferred = Some(ix);
                }
                indented |= first != ' ';
                previous = cluster.chars().next_back().unwrap_or(first);
            }
            end = preferred.unwrap_or(end);
        }
    }

    pub fn finish(mut self) -> Breaks {
        assert!(self.complete);
        self.prefix.append_points(self.points);
        // Keep checkpoint lookup bounded after a long sequence of edits.
        // This occasional compaction runs with the wrap, in the background
        // for a long line; ordinary edits share the unchanged arrays.
        if self.prefix.parts.len() > 64 {
            let points = (0..self.prefix.len())
                .map(|ix| self.prefix.point(ix))
                .collect();
            self.prefix = Breaks::default();
            self.prefix.append_points(points);
        }
        self.prefix
    }
}

fn can_break(previous: char, first: char, indented: bool, objects: bool) -> bool {
    indented
        && (LineWrapper::can_break_before(previous, first)
            || (objects && (previous == '\u{fffc}' || first == '\u{fffc}')))
}

fn measure_row(
    line: &str,
    runs: &[(usize, usize)],
    wrappers: &[LineWrapperHandle],
    objects: &[(Range<usize>, Pixels)],
    range: Range<usize>,
    max_width: Pixels,
) -> (Pixels, usize) {
    let text_width = |range: Range<usize>, available| {
        let mut start = range.start;
        let mut at = runs.partition_point(|(end, _)| *end <= start);
        LineWrapper::measure_runs(
            &line[range.clone()],
            std::iter::from_fn(|| {
                if start == range.end {
                    return None;
                }
                let (end, font) = runs.get(at).copied().unwrap_or((range.end, 0));
                let end = end.min(range.end);
                let len = end - start;
                start = end;
                at += 1;
                Some((len, &*wrappers[font]))
            }),
            available,
        )
    };
    let mut start = range.start;
    let mut width = px(0.);
    let mut fit = range.end;
    for (object, size) in
        objects[objects.partition_point(|(object, _)| object.end <= start)..].iter()
    {
        if object.start >= range.end {
            break;
        }
        let (drawn, boundary) = text_width(start..object.start, max_width - width);
        if fit == range.end && width + drawn > max_width {
            fit = start + boundary;
        }
        width += drawn + *size;
        if fit == range.end && width > max_width {
            fit = object.start;
        }
        start = object.end;
    }
    let (drawn, boundary) = text_width(start..range.end, max_width - width);
    if fit == range.end && width + drawn > max_width {
        fit = start + boundary;
    }
    (width + drawn, fit)
}

/// Object boundaries also separate grapheme segmentation. A combining mark
/// following an object belongs to the next text run, never to the object.
struct Clusters<'a> {
    text: &'a str,
    objects: &'a [(Range<usize>, Pixels)],
    end: usize,
    offset: usize,
    text_end: usize,
    current: unicode_segmentation::GraphemeIndices<'a>,
}

impl<'a> Clusters<'a> {
    fn new(text: &'a str, objects: &'a [(Range<usize>, Pixels)], range: Range<usize>) -> Self {
        let objects = &objects[objects.partition_point(|(object, _)| object.end <= range.start)..];
        let end = objects
            .first()
            .map_or(range.end, |(object, _)| object.start.min(range.end));
        Self {
            text,
            objects,
            end: range.end,
            offset: range.start,
            text_end: end,
            current: text[range.start..end].grapheme_indices(true),
        }
    }
}

impl<'a> Iterator for Clusters<'a> {
    type Item = (usize, &'a str);
    fn next(&mut self) -> Option<Self::Item> {
        if let Some((ix, cluster)) = self.current.next() {
            return Some((self.offset + ix, cluster));
        }
        let (object, _) = self.objects.first()?;
        if object.start >= self.end {
            return None;
        }
        let result = (object.start, &self.text[object.clone()]);
        self.offset = object.end;
        self.objects = &self.objects[1..];
        self.text_end = self
            .objects
            .first()
            .map_or(self.end, |(object, _)| object.start.min(self.end));
        self.current = self.text[self.offset..self.text_end].grapheme_indices(true);
        Some(result)
    }
}
