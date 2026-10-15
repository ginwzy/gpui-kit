//! Resumable form of the document's greedy cluster wrapper. Checkpoints
//! include the carried row width and all break state, not just an offset.
//! Measurements stay in GPUI's font/size keyed LineWrapper pool; changing
//! width invalidates breakpoints without invalidating cluster measurements.
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

    /// Consume a bounded number of input bytes. A cluster is indivisible;
    /// callers route non-ASCII input to a cancellable background task.
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
        let from = self.point.next;
        for (relative, cluster) in line[from..].grapheme_indices(true) {
            if cancelled.load(Ordering::Relaxed) {
                return false;
            }
            let ix = from + relative;
            if ix >= stop {
                return false;
            }
            let first = cluster.chars().next().unwrap_or_default();
            if self.point.indented && LineWrapper::can_break_before(self.point.previous, first) {
                self.candidate = Some(ix);
            }
            self.point.indented |= first != ' ';
            let mut amount = advance(&mut wrappers[font_at(ix)], cluster, self.point.x);
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
                let start = self
                    .candidate
                    .filter(|start| *start > row && end - start <= MAX_ROW_BYTES)
                    .unwrap_or(ix);
                self.point.row = start;
                self.candidate = None;
                self.point.x =
                    line[start..ix]
                        .grapheme_indices(true)
                        .fold(px(0.), |x, (offset, cluster)| {
                            x + advance(&mut wrappers[font_at(start + offset)], cluster, x)
                        });
                amount = advance(&mut wrappers[font_at(ix)], cluster, self.point.x);
            }
            self.point.x += amount;
            self.point.previous = cluster.chars().last().unwrap_or(first);
            self.point.next = ix + cluster.len();
            self.processed += cluster.len();
            if broke {
                self.points.push(self.point);
                if let Some((old, suffix, delta)) = &self.reuse {
                    if ix >= *suffix && self.point.row >= *suffix {
                        let old_next = self.point.next.checked_add_signed(-*delta).unwrap();
                        let at = old.breaks.partition_point(|point| point.next < old_next);
                        if at < old.breaks.len()
                            && old.breaks.point(at).shifted(*delta) == self.point
                        {
                            self.prefix.append_points(std::mem::take(&mut self.points));
                            self.prefix
                                .append_range(&old.breaks, at + 1..old.breaks.len(), *delta);
                            self.reused = old.breaks.len() - at - 1;
                            self.complete = true;
                            return true;
                        }
                    }
                }
            }
        }
        self.complete = true;
        true
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
