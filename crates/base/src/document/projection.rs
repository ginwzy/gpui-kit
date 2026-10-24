use std::{
    error::Error,
    fmt,
    ops::Range,
    sync::{Arc, OnceLock},
};

use ropey::Rope;

use super::Affinity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionSpan {
    source: Range<usize>,
    replacement: String,
    mapping: Option<Vec<ProjectionMapping>>,
}

impl ProjectionSpan {
    pub(crate) fn shift_source(&mut self, delta: isize) {
        self.source = shifted(self.source.clone(), delta);
        if let Some(mapping) = &mut self.mapping {
            for entry in mapping {
                entry.source = shifted(entry.source.clone(), delta);
            }
        }
    }

    pub fn replace(source: Range<usize>, replacement: impl Into<String>) -> Self {
        Self {
            source,
            replacement: replacement.into(),
            mapping: None,
        }
    }

    pub fn mapped(
        source: Range<usize>,
        replacement: impl Into<String>,
        mapping: Vec<ProjectionMapping>,
    ) -> Self {
        Self {
            source,
            replacement: replacement.into(),
            mapping: Some(mapping),
        }
    }

    pub fn hide(source: Range<usize>) -> Self {
        Self::replace(source, "")
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn mapping(&self) -> Option<&[ProjectionMapping]> {
        self.mapping.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionMapping {
    display: Range<usize>,
    source: Range<usize>,
}

impl ProjectionMapping {
    pub fn new(display: Range<usize>, source: Range<usize>) -> Self {
        Self { display, source }
    }

    pub fn display(&self) -> Range<usize> {
        self.display.clone()
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentProjection {
    source_len: usize,
    spans: Vec<ProjectionSpan>,
}

impl DocumentProjection {
    pub(super) fn splice(&mut self, range: Range<usize>, mut replacement: Self) {
        let delta = replacement.source_len as isize - range.len() as isize;
        let from = self
            .spans
            .partition_point(|span| span.source.end <= range.start);
        let to = self
            .spans
            .partition_point(|span| span.source.start < range.end);
        for span in &mut replacement.spans {
            span.shift_source(range.start as isize);
        }
        for span in &mut self.spans[to..] {
            span.shift_source(delta);
        }
        self.spans.splice(from..to, replacement.spans);
        self.source_len = self.source_len.checked_add_signed(delta).unwrap();
    }

    pub fn identity(source_len: usize) -> Self {
        Self {
            source_len,
            spans: Vec::new(),
        }
    }

    pub fn new(source_len: usize, mut spans: Vec<ProjectionSpan>) -> Result<Self, ProjectionError> {
        spans.sort_by_key(|span| (span.source.start, span.source.end));
        let mut previous_end = 0;
        for span in &spans {
            if span.source.start >= span.source.end {
                return Err(ProjectionError::InvalidRange(span.source.clone()));
            }
            if span.source.end > source_len {
                return Err(ProjectionError::OutOfBounds {
                    range: span.source.clone(),
                    source_len,
                });
            }
            if span.source.start < previous_end {
                return Err(ProjectionError::Overlap(span.source.clone()));
            }
            if let Some(mapping) = &span.mapping {
                let mut display_end = 0;
                let mut source_start = span.source.start;
                for entry in mapping {
                    if entry.display.start != display_end
                        || entry.display.end <= entry.display.start
                        || entry.display.end > span.replacement.len()
                        || entry.source.start < span.source.start
                        || entry.source.end > span.source.end
                        || entry.source.end <= entry.source.start
                        || entry.source.start < source_start
                        || !span.replacement.is_char_boundary(entry.display.start)
                        || !span.replacement.is_char_boundary(entry.display.end)
                    {
                        return Err(ProjectionError::InvalidMapping(span.source.clone()));
                    }
                    display_end = entry.display.end;
                    source_start = entry.source.start;
                }
                if display_end != span.replacement.len() {
                    return Err(ProjectionError::InvalidMapping(span.source.clone()));
                }
            }
            previous_end = span.source.end;
        }
        Ok(Self { source_len, spans })
    }

    pub fn source_len(&self) -> usize {
        self.source_len
    }

    pub fn spans(&self) -> &[ProjectionSpan] {
        &self.spans
    }

    pub fn is_identity(&self) -> bool {
        self.spans.is_empty()
    }

    #[cfg(any(test, debug_assertions))]
    pub(crate) fn transformed(&self, edits: &[super::TextEdit], new_source_len: usize) -> Self {
        Self {
            source_len: new_source_len,
            spans: self
                .spans
                .iter()
                .map(|span| span.transformed(edits))
                .collect(),
        }
    }

    /// Follows `edits` inside editable source `region`, which no span
    /// overlaps: spans at its edges transform, later spans move by `delta`.
    pub(crate) fn edit_region(
        &mut self,
        region: Range<usize>,
        edits: &[super::TextEdit],
        delta: isize,
    ) {
        let from = self
            .spans
            .partition_point(|span| span.source.end < region.start);
        let to = self
            .spans
            .partition_point(|span| span.source.start <= region.end);
        for span in &mut self.spans[from..to] {
            *span = span.transformed(edits);
        }
        for span in &mut self.spans[to..] {
            span.shift_source(delta);
        }
        self.source_len = self.source_len.checked_add_signed(delta).unwrap();
    }
}

impl ProjectionSpan {
    // User edits cannot overlap projected source. Insertions at either edge
    // belong to adjacent editable text, so spans and their mappings must not
    // grow to consume them.
    fn transformed(&self, edits: &[super::TextEdit]) -> Self {
        let source = super::position::transform_source_range(self.source.clone(), edits);
        let mapping = self.mapping.as_ref().map(|mapping| {
            mapping
                .iter()
                .map(|entry| {
                    let source =
                        super::position::transform_source_range(entry.source.clone(), edits);
                    ProjectionMapping::new(entry.display.clone(), source)
                })
                .collect()
        });
        ProjectionSpan {
            source,
            replacement: self.replacement.clone(),
            mapping,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidBoundary(Range<usize>),
    SourceLength {
        expected: usize,
        actual: usize,
    },
    InvalidRange(Range<usize>),
    OutOfBounds {
        range: Range<usize>,
        source_len: usize,
    },
    Overlap(Range<usize>),
    EditableOverlap(Range<usize>),
    BlocksRequirePresentation,
    InvalidMapping(Range<usize>),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundary(range) => write!(
                formatter,
                "projection range {range:?} is not on UTF-8 boundaries"
            ),
            Self::SourceLength { expected, actual } => write!(
                formatter,
                "projection source length {actual} does not match document length {expected}"
            ),
            Self::InvalidRange(range) => write!(formatter, "invalid projection range {range:?}"),
            Self::OutOfBounds { range, source_len } => {
                write!(
                    formatter,
                    "projection range {range:?} exceeds source length {source_len}"
                )
            }
            Self::Overlap(range) => {
                write!(formatter, "projection range {range:?} overlaps another")
            }
            Self::EditableOverlap(range) => {
                write!(
                    formatter,
                    "projection range {range:?} overlaps editable text"
                )
            }
            Self::BlocksRequirePresentation => formatter.write_str(
                "documents with inline blocks must update projection and blocks together",
            ),
            Self::InvalidMapping(range) => {
                write!(
                    formatter,
                    "projection span {range:?} has an invalid mapping"
                )
            }
        }
    }
}

impl Error for ProjectionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProjectionRunKind {
    Identity,
    Replacement(Option<MappedRun>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MappedRun {
    entries: Vec<ProjectionMapping>,
    ordered_ends: bool,
}

impl MappedRun {
    fn new(entries: Vec<ProjectionMapping>) -> Self {
        let ordered_ends = entries
            .windows(2)
            .all(|pair| pair[0].source.end <= pair[1].source.end);
        Self {
            entries,
            ordered_ends,
        }
    }

    fn intersecting(&self, source: &Range<usize>) -> impl Iterator<Item = &ProjectionMapping> {
        // Starts are always ordered. Markdown also has monotonic ends, including
        // repeated source events; arbitrary legal overlaps retain a full scan.
        let from = if self.ordered_ends {
            self.entries
                .partition_point(|entry| entry.source.end <= source.start)
        } else {
            0
        };
        self.entries[from..]
            .iter()
            .take_while(|entry| entry.source.start < source.end)
            .filter(|entry| source.start < entry.source.end)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ProjectionRun {
    source: Range<usize>,
    display: Range<usize>,
    kind: ProjectionRunKind,
}

fn shifted(range: Range<usize>, delta: isize) -> Range<usize> {
    range.start.checked_add_signed(delta).unwrap()..range.end.checked_add_signed(delta).unwrap()
}

impl ProjectionRun {
    fn shift(&mut self, source_delta: isize, display_delta: isize) {
        self.source = shifted(self.source.clone(), source_delta);
        self.display = shifted(self.display.clone(), display_delta);
        if let ProjectionRunKind::Replacement(Some(mapping)) = &mut self.kind {
            for entry in &mut mapping.entries {
                entry.source = shifted(entry.source.clone(), source_delta);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectionMap {
    display: String,
    shared_display: SharedDisplay,
    source_len: usize,
    runs: Vec<ProjectionRun>,
}

/// The display text in one shared allocation, made when first asked for
/// after each change, so background wraps scheduled frame after frame share
/// it rather than copy the text each time. Not part of the map's identity.
#[derive(Clone, Debug, Default)]
struct SharedDisplay(OnceLock<Arc<str>>);

impl PartialEq for SharedDisplay {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ProjectionMap {
    /// The caller has checked that no replacement run straddles either source boundary.
    pub(super) fn splice(
        &mut self,
        source: Range<usize>,
        display: Range<usize>,
        mut replacement: Self,
    ) {
        let source_delta = replacement.source_len as isize - source.len() as isize;
        let display_delta = replacement.display.len() as isize - display.len() as isize;
        let from = self
            .runs
            .partition_point(|run| run.source.end <= source.start && !run.source.is_empty());
        let to = self
            .runs
            .partition_point(|run| run.source.start < source.end);
        // Identity runs can include both a readonly region and the adjacent draft.
        let mut inserted = Vec::new();
        if let Some(run) = self
            .runs
            .get(from)
            .filter(|run| run.source.start < source.start)
        {
            inserted.push(ProjectionRun {
                source: run.source.start..source.start,
                display: run.display.start..display.start,
                kind: ProjectionRunKind::Identity,
            });
        }
        for run in &mut replacement.runs {
            run.shift(source.start as isize, display.start as isize);
        }
        inserted.extend(
            replacement
                .runs
                .into_iter()
                .filter(|run| !run.source.is_empty()),
        );
        if let Some(run) = self.runs[..to]
            .last()
            .filter(|run| run.source.end > source.end)
        {
            inserted.push(ProjectionRun {
                source: shifted(source.end..run.source.end, source_delta),
                display: shifted(display.end..run.display.end, display_delta),
                kind: ProjectionRunKind::Identity,
            });
        }
        for run in &mut self.runs[to..] {
            run.shift(source_delta, display_delta);
        }
        let inserted_len = inserted.len();
        self.runs.splice(from..to, inserted);
        self.display.replace_range(display, &replacement.display);
        self.shared_display = SharedDisplay::default();
        self.source_len = self.source_len.checked_add_signed(source_delta).unwrap();
        // Identity runs cut at the seams join again, as `new` builds them.
        self.merge_identity_runs(from.saturating_sub(1)..from + inserted_len + 1);
        // An empty document keeps one empty run.
        if self.runs.is_empty() {
            self.runs.push(ProjectionRun {
                source: 0..0,
                display: 0..0,
                kind: ProjectionRunKind::Identity,
            });
        }
    }

    /// Joins adjacent identity runs among `runs`.
    fn merge_identity_runs(&mut self, runs: Range<usize>) {
        let mut ix = runs.start;
        let mut end = runs.end;
        while ix + 1 < self.runs.len() && ix < end {
            let (left, right) = (&self.runs[ix], &self.runs[ix + 1]);
            if left.kind == ProjectionRunKind::Identity
                && right.kind == ProjectionRunKind::Identity
                && left.source.end == right.source.start
            {
                let right = self.runs.remove(ix + 1);
                self.runs[ix].source.end = right.source.end;
                self.runs[ix].display.end = right.display.end;
                end = end.saturating_sub(1);
            } else {
                ix += 1;
            }
        }
    }

    /// Replaces the text of `source`, which only identity runs cover, with
    /// `text`.
    pub(super) fn replace_identity(&mut self, source: Range<usize>, text: &str) {
        let display = self
            .source_to_display(source.start, Affinity::Before)
            .expect("an edited range maps to display")
            ..self
                .source_to_display(source.end, Affinity::Before)
                .expect("an edited range maps to display");
        self.splice(
            source,
            display,
            Self::new(&Rope::from(text), &DocumentProjection::identity(text.len())),
        );
    }

    pub(crate) fn new(source: &Rope, projection: &DocumentProjection) -> Self {
        debug_assert_eq!(source.len(), projection.source_len);
        let mut display = String::new();
        let mut runs = Vec::new();
        let mut source_cursor = 0;

        for span in &projection.spans {
            if source_cursor < span.source.start {
                let text = source.slice(source_cursor..span.source.start).to_string();
                let display_start = display.len();
                display.push_str(&text);
                runs.push(ProjectionRun {
                    source: source_cursor..span.source.start,
                    display: display_start..display.len(),
                    kind: ProjectionRunKind::Identity,
                });
            }

            let display_start = display.len();
            display.push_str(&span.replacement);
            runs.push(ProjectionRun {
                source: span.source.clone(),
                display: display_start..display.len(),
                kind: ProjectionRunKind::Replacement(span.mapping.clone().map(MappedRun::new)),
            });
            source_cursor = span.source.end;
        }

        if source_cursor < source.len() || runs.is_empty() {
            let text = source.slice(source_cursor..source.len()).to_string();
            let display_start = display.len();
            display.push_str(&text);
            runs.push(ProjectionRun {
                source: source_cursor..source.len(),
                display: display_start..display.len(),
                kind: ProjectionRunKind::Identity,
            });
        }

        Self {
            display,
            shared_display: SharedDisplay::default(),
            source_len: source.len(),
            runs,
        }
    }

    pub(crate) fn display_text(&self) -> &str {
        &self.display
    }

    /// [`Self::display_text`], shared.
    pub(crate) fn shared_display_text(&self) -> Arc<str> {
        self.shared_display
            .0
            .get_or_init(|| self.display.as_str().into())
            .clone()
    }

    pub(crate) fn source_to_display(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        if offset > self.source_len {
            return None;
        }
        if offset == self.source_len {
            return Some(self.display.len());
        }
        let start = self
            .runs
            .partition_point(|run| run.source.end <= offset && !run.source.is_empty());
        let run = self.runs[start..].iter().find(|run| {
            run.source.start <= offset
                && (offset < run.source.end || (offset == run.source.end && run.source.is_empty()))
        })?;
        match &run.kind {
            ProjectionRunKind::Identity => Some(run.display.start + offset - run.source.start),
            ProjectionRunKind::Replacement(None) => {
                if offset == run.source.start || affinity == Affinity::Before {
                    Some(run.display.start)
                } else {
                    Some(run.display.end)
                }
            }
            ProjectionRunKind::Replacement(Some(mapping)) => {
                mapped_source_to_display(&mapping.entries, run.display.start, offset, affinity)
            }
        }
    }

    /// Project source coverage, rather than a caret's affinity at either edge.
    /// Shared endpoints do not intersect. A replacement can have several
    /// display scalars for the same source, all of which retain its style.
    pub(crate) fn project_source_range(
        &self,
        source: Range<usize>,
        mut emit: impl FnMut(Range<usize>),
    ) {
        if source.is_empty() {
            return;
        }
        let mut pending: Option<Range<usize>> = None;
        let mut append = |range: Range<usize>| {
            if range.is_empty() {
                return;
            }
            if let Some(previous) = &mut pending
                && previous.end == range.start
            {
                previous.end = range.end;
            } else if let Some(previous) = pending.replace(range) {
                emit(previous);
            }
        };
        let from = self
            .runs
            .partition_point(|run| run.source.end <= source.start);
        for run in self.runs[from..]
            .iter()
            .take_while(|run| run.source.start < source.end)
        {
            match &run.kind {
                ProjectionRunKind::Identity => append(
                    run.display.start + source.start.max(run.source.start) - run.source.start
                        ..run.display.start + source.end.min(run.source.end) - run.source.start,
                ),
                ProjectionRunKind::Replacement(None) => append(run.display.clone()),
                ProjectionRunKind::Replacement(Some(mapping)) => {
                    for entry in mapping.intersecting(&source) {
                        let local = if entry.source.len() == entry.display.len() {
                            entry.display.start + source.start.max(entry.source.start)
                                - entry.source.start
                                ..entry.display.start + source.end.min(entry.source.end)
                                    - entry.source.start
                        } else {
                            entry.display.clone()
                        };
                        // Equal byte lengths can still replace different scalars.
                        // Styles cover whole displayed characters in that case.
                        append(
                            self.display
                                .floor_char_boundary(run.display.start + local.start)
                                ..self
                                    .display
                                    .ceil_char_boundary(run.display.start + local.end),
                        );
                    }
                }
            }
        }
        if let Some(previous) = pending {
            emit(previous);
        }
    }

    pub(crate) fn display_to_source(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        self.display_to_source_from(offset, affinity, &mut 0)
    }

    /// `display_to_source`, searching the runs forward from `hint` and leaving
    /// there the run it starts from. Ascending offsets each cost O(log distance).
    pub(crate) fn display_to_source_from(
        &self,
        offset: usize,
        affinity: Affinity,
        hint: &mut usize,
    ) -> Option<usize> {
        if offset > self.display.len() {
            return None;
        }
        if offset == self.display.len() {
            return Some(self.source_len);
        }
        // Inside the hinted identity run, no other run holds the offset and
        // the search would end at the hint. Lines in order mostly land here.
        if let Some(run) = self.runs.get(*hint)
            && run.display.start < offset
            && offset < run.display.end
            && matches!(run.kind, ProjectionRunKind::Identity)
        {
            return Some(run.source.start + offset - run.display.start);
        }
        let start = partition_from(&self.runs, *hint, |run| {
            run.display.end < offset || (run.display.end == offset && !run.display.is_empty())
        });
        *hint = start;
        // Runs before `start` end at or before `offset`, so they also start there.
        let end = partition_from(&self.runs, start, |run| run.display.start <= offset);
        let mut runs = self.runs[start..end].iter().filter(|run| {
            run.display.start <= offset
                && (offset < run.display.end
                    || (run.display.is_empty() && offset == run.display.start))
        });
        let run = if affinity == Affinity::Before {
            runs.next()
        } else {
            runs.next_back()
        }?;
        match &run.kind {
            ProjectionRunKind::Identity => Some(run.source.start + offset - run.display.start),
            ProjectionRunKind::Replacement(None) => {
                if run.display.is_empty() {
                    Some(if affinity == Affinity::Before {
                        run.source.start
                    } else {
                        run.source.end
                    })
                } else if offset == run.display.start || affinity == Affinity::Before {
                    Some(run.source.start)
                } else {
                    Some(run.source.end)
                }
            }
            ProjectionRunKind::Replacement(Some(mapping)) => {
                mapped_display_to_source(&mapping.entries, run.display.start, offset, affinity)
            }
        }
    }
}

/// `items.partition_point(pred)`, galloping forward from `hint` unless the
/// answer lies before it. Ascending queries each cost O(log distance).
pub(super) fn partition_from<T>(items: &[T], hint: usize, pred: impl Fn(&T) -> bool) -> usize {
    let mut low = hint.min(items.len());
    if low > 0 && !pred(&items[low - 1]) {
        return items.partition_point(pred);
    }
    let mut step = 1;
    while low + step <= items.len() && pred(&items[low + step - 1]) {
        low += step;
        step *= 2;
    }
    let high = (low + step).min(items.len());
    low + items[low..high].partition_point(pred)
}

fn mapped_source_to_display(
    mapping: &[ProjectionMapping],
    display_start: usize,
    offset: usize,
    affinity: Affinity,
) -> Option<usize> {
    if let Some(entry) = mapping.iter().find(|entry| {
        entry.source.start <= offset
            && (offset < entry.source.end
                || offset == entry.source.end && affinity == Affinity::Before)
    }) {
        let display = if entry.source.len() == entry.display.len() {
            entry.display.start + (offset - entry.source.start).min(entry.display.len())
        } else if offset == entry.source.start || affinity == Affinity::Before {
            entry.display.start
        } else {
            entry.display.end
        };
        return Some(display_start + display);
    }
    match affinity {
        Affinity::Before => mapping
            .iter()
            .find(|entry| entry.source.start >= offset)
            .map(|entry| display_start + entry.display.start)
            .or_else(|| {
                mapping
                    .last()
                    .map(|entry| display_start + entry.display.end)
            }),
        Affinity::After => mapping
            .iter()
            .rev()
            .find(|entry| entry.source.end <= offset)
            .map(|entry| display_start + entry.display.end)
            .or_else(|| {
                mapping
                    .first()
                    .map(|entry| display_start + entry.display.start)
            }),
    }
}

fn mapped_display_to_source(
    mapping: &[ProjectionMapping],
    display_start: usize,
    offset: usize,
    affinity: Affinity,
) -> Option<usize> {
    let local = offset.saturating_sub(display_start);
    // Entries tile the replacement in order, so the first one that reaches
    // `local` also starts at or before it.
    let entry = mapping.get(mapping.partition_point(|entry| {
        entry.display.end < local || (entry.display.end == local && affinity == Affinity::After)
    }))?;
    if entry.source.len() == entry.display.len() {
        Some(entry.source.start + (local - entry.display.start).min(entry.source.len()))
    } else if local == entry.display.start || affinity == Affinity::Before {
        Some(entry.source.start)
    } else {
        Some(entry.source.end)
    }
}

#[cfg(test)]
mod tests;
