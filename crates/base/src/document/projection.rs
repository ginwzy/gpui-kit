use std::{error::Error, fmt, ops::Range};

use ropey::Rope;

use super::Affinity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionSpan {
    source: Range<usize>,
    replacement: String,
    mapping: Option<Vec<ProjectionMapping>>,
}

impl ProjectionSpan {
    fn shift_source(&mut self, delta: isize) {
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

    pub(crate) fn transformed(&self, edits: &[super::TextEdit], new_source_len: usize) -> Self {
        // User edits cannot overlap projected source. Insertions at either edge
        // belong to adjacent editable text, so spans and their mappings must not
        // grow to consume them.
        let spans = self
            .spans
            .iter()
            .map(|span| {
                let source = super::position::transform_source_range(span.source.clone(), edits);
                let mapping = span.mapping.as_ref().map(|mapping| {
                    mapping
                        .iter()
                        .map(|entry| {
                            let source = super::position::transform_source_range(
                                entry.source.clone(),
                                edits,
                            );
                            ProjectionMapping::new(entry.display.clone(), source)
                        })
                        .collect()
                });
                ProjectionSpan {
                    source,
                    replacement: span.replacement.clone(),
                    mapping,
                }
            })
            .collect();
        Self {
            source_len: new_source_len,
            spans,
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
    Replacement(Option<Vec<ProjectionMapping>>),
}

#[derive(Clone, Debug)]
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
            for entry in mapping {
                entry.source = shifted(entry.source.clone(), source_delta);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectionMap {
    display: String,
    source_len: usize,
    runs: Vec<ProjectionRun>,
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
        self.runs.splice(from..to, inserted);
        self.display.replace_range(display, &replacement.display);
        self.source_len = self.source_len.checked_add_signed(source_delta).unwrap();
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
                kind: ProjectionRunKind::Replacement(span.mapping.clone()),
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
            source_len: source.len(),
            runs,
        }
    }

    pub(crate) fn display_text(&self) -> &str {
        &self.display
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
                mapped_source_to_display(mapping, run.display.start, offset, affinity)
            }
        }
    }

    pub(crate) fn display_to_source(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        if offset > self.display.len() {
            return None;
        }
        if offset == self.display.len() {
            return Some(self.source_len);
        }
        let start = self.runs.partition_point(|run| {
            run.display.end < offset || (run.display.end == offset && !run.display.is_empty())
        });
        let end = self.runs.partition_point(|run| run.display.start <= offset);
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
                mapped_display_to_source(mapping, run.display.start, offset, affinity)
            }
        }
    }
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
    let entry = mapping.iter().find(|entry| {
        entry.display.start <= local
            && (local < entry.display.end
                || local == entry.display.end && affinity == Affinity::Before)
    })?;
    if entry.source.len() == entry.display.len() {
        Some(entry.source.start + (local - entry.display.start).min(entry.source.len()))
    } else if local == entry.display.start || affinity == Affinity::Before {
        Some(entry.source.start)
    } else {
        Some(entry.source.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::TextEdit;

    #[test]
    fn after_affinity_skips_all_hidden_objects_at_a_text_boundary() {
        let source = Rope::from("\u{fffc}\u{fffc}text");
        let projection = DocumentProjection::new(
            source.len(),
            vec![ProjectionSpan::hide(0..3), ProjectionSpan::hide(3..6)],
        )
        .unwrap();
        let map = ProjectionMap::new(&source, &projection);
        assert_eq!(map.display_to_source(0, Affinity::Before), Some(0));
        assert_eq!(map.display_to_source(0, Affinity::After), Some(6));
    }

    #[test]
    fn replacement_maps_source_and_display_edges_with_affinity() {
        let source = Rope::from("a**bold**z");
        let projection = DocumentProjection::new(
            source.len(),
            vec![ProjectionSpan::hide(1..3), ProjectionSpan::hide(7..9)],
        )
        .unwrap();
        let map = ProjectionMap::new(&source, &projection);

        assert_eq!(map.display_text(), "aboldz");
        assert_eq!(map.source_to_display(1, Affinity::Before), Some(1));
        assert_eq!(map.source_to_display(2, Affinity::After), Some(1));
        assert_eq!(map.source_to_display(3, Affinity::After), Some(1));
        assert_eq!(map.display_to_source(1, Affinity::Before), Some(1));
        assert_eq!(map.display_to_source(1, Affinity::After), Some(3));
        assert_eq!(map.display_to_source(5, Affinity::After), Some(9));
    }

    #[test]
    fn replacement_text_maps_interior_offsets_to_source_boundaries() {
        let source = Rope::from("aXYZz");
        let projection =
            DocumentProjection::new(source.len(), vec![ProjectionSpan::replace(1..4, "block")])
                .unwrap();
        let map = ProjectionMap::new(&source, &projection);

        assert_eq!(map.display_text(), "ablockz");
        assert_eq!(map.display_to_source(3, Affinity::Before), Some(1));
        assert_eq!(map.display_to_source(3, Affinity::After), Some(4));
        assert_eq!(map.source_to_display(2, Affinity::Before), Some(1));
        assert_eq!(map.source_to_display(2, Affinity::After), Some(6));
    }

    #[test]
    fn spans_follow_an_edit_before_them() {
        let projection =
            DocumentProjection::new(6, vec![ProjectionSpan::replace(4..5, "X")]).unwrap();
        let transformed = projection.transformed(&[TextEdit::new(1..1, "++")], 8);
        assert_eq!(transformed.spans()[0].source(), 6..7);
    }

    #[test]
    fn projection_boundaries_exclude_adjacent_edits() {
        for span in [
            ProjectionSpan::hide(2..4),
            ProjectionSpan::replace(2..4, "Q"),
            ProjectionSpan::mapped(2..4, "Q", vec![ProjectionMapping::new(0..1, 2..4)]),
        ] {
            let projection = DocumentProjection::new(6, vec![span]).unwrap();
            for (edit, source, expected) in [
                (TextEdit::new(2..2, "x"), "abxCDyz", 3..5),
                (TextEdit::new(4..4, "x"), "abCDxyz", 2..4),
                (TextEdit::new(0..2, "x"), "xCDyz", 1..3),
                (TextEdit::new(4..6, "x"), "abCDx", 2..4),
            ] {
                let transformed = projection.transformed(&[edit], source.len());
                let span = &transformed.spans()[0];
                assert_eq!(span.source(), expected);
                if let Some(mapping) = &span.mapping {
                    assert_eq!(mapping[0].source(), expected);
                }
                assert_eq!(
                    ProjectionMap::new(&Rope::from(source), &transformed).display_text(),
                    format!(
                        "{}{}{}",
                        &source[..expected.start],
                        span.replacement(),
                        &source[expected.end..]
                    ),
                );
            }
        }
    }

    #[test]
    fn mapped_replacement_preserves_scalar_source_ranges() {
        let source = Rope::from("**重复 &amp;**");
        let projection = DocumentProjection::new(
            source.len(),
            vec![ProjectionSpan::mapped(
                0..source.len(),
                "重复 &",
                vec![
                    ProjectionMapping::new(0..3, 2..5),
                    ProjectionMapping::new(3..6, 5..8),
                    ProjectionMapping::new(6..7, 8..9),
                    ProjectionMapping::new(7..8, 9..14),
                ],
            )],
        )
        .unwrap();
        let map = ProjectionMap::new(&source, &projection);

        assert_eq!(map.display_text(), "重复 &");
        assert_eq!(map.source_to_display(2, Affinity::After), Some(0));
        assert_eq!(map.source_to_display(5, Affinity::After), Some(3));
        assert_eq!(map.display_to_source(3, Affinity::After), Some(5));
        assert_eq!(map.display_to_source(7, Affinity::Before), Some(9));
        assert_eq!(
            map.display_to_source(8, Affinity::After),
            Some(source.len())
        );
    }

    #[test]
    fn invalid_spans_are_rejected() {
        assert_eq!(
            DocumentProjection::new(4, vec![ProjectionSpan::hide(2..2)]),
            Err(ProjectionError::InvalidRange(2..2))
        );
        assert_eq!(
            DocumentProjection::new(
                4,
                vec![ProjectionSpan::hide(1..3), ProjectionSpan::hide(2..4)],
            ),
            Err(ProjectionError::Overlap(2..4))
        );
    }
}
