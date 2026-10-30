use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
    hash::Hash,
    ops::Range,
    rc::Rc,
};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, HighlightStyle, InteractiveElement as _,
    IntoElement as _, ListAlignment, ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, TextStyleRefinement,
    UTF16Selection, Window, div, list, point, px,
};
use ropey::{LineType, Rope};
use sum_tree::Bias;

use crate::{
    AutoScroll, BlinkCursor, Scrollbar, ScrollbarHandle,
    actions::{SelectDown, SelectLeft, SelectRight, SelectUp},
    input::{
        Backspace, Copy, Cut, Delete, DeleteToBeginningOfLine, DeleteToEndOfLine,
        DeleteToNextWordEnd, DeleteToPreviousWordStart, Enter, MoveDown, MoveEnd, MoveHome,
        MoveLeft, MovePageDown, MovePageUp, MoveRight, MoveToEnd, MoveToEndOfLine, MoveToNextWord,
        MoveToPreviousWord, MoveToStart, MoveToStartOfLine, MoveUp, Paste, Redo, RopeExt as _,
        SelectAll, SelectToEnd, SelectToEndOfLine, SelectToNextWordEnd, SelectToPreviousWordStart,
        SelectToStart, SelectToStartOfLine, Undo,
    },
};

use super::{
    Affinity, AnchorBias, AnchoredBlock, AnchoredBlockError, BlockError, DocumentAnchor,
    DocumentBlock, DocumentElement, DocumentInlineStyle, DocumentParagraphStyle, DocumentPosition,
    DocumentProjection, DocumentRegion, DocumentRegions, DocumentRevision, DocumentSelection,
    DocumentStyleError, DocumentStyles, EditDecision, EditOrigin, EditPolicy, EditTransaction,
    PositionError, ProjectionError, RegionError, TextEdit, TransactionError,
    anchored_block::display_offset as anchored_display_offset,
    block::validate_blocks,
    element::DocumentChild,
    position::{shift_offset, transform_offset, transform_source_range},
    projection::{ProjectionMap, partition_from},
};

const DOCUMENT_INPUT_CONTEXT: &str = "Input";

mod host;
pub(super) mod inline_objects;
mod inline_styles;
mod layout_items;
mod layout_window;
mod presentation;
pub(in crate::document) mod table;
mod text_rows;
mod viewport;
use layout_items::LayoutItems;

#[cfg(test)]
mod style_tests;

pub use inline_objects::StructureReplacement;
pub(super) use text_rows::RowFrame;
pub use text_rows::{TextFrame, TextRows};
pub use viewport::ScrollHold;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentEditRejection {
    InvalidOrigin,
    StaleRevision,
    OutsideRegion,
    CrossesRegions,
    Readonly,
    Atomic,
    InvalidBoundary,
    InvalidRegions,
    InvalidPresentation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostTransactionError {
    FragmentBoundary,
    InvalidFragment(PresentationError),
    InvalidOrigin,
    InvalidEditRange(Range<usize>),
    StaleRevision,
    TouchesEditableRegion,
    InvalidRegions(RegionError),
    EditableRegionsChanged,
    ProjectionRequired,
    InvalidProjection(ProjectionError),
    BlocksRequired,
    InvalidBlocks(BlockError),
    AnchoredBlocksRequired,
    InvalidAnchoredBlocks(AnchoredBlockError),
    StylesRequired,
    InvalidStyles(DocumentStyleError),
}

impl fmt::Display for HostTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FragmentBoundary => formatter.write_str(
                "host fragment must replace complete readonly nodes at display line boundaries",
            ),
            Self::InvalidFragment(error) => write!(formatter, "invalid host fragment: {error}"),
            Self::InvalidEditRange(range) => write!(
                formatter,
                "host edit range {range:?} is outside the source or not on UTF-8 boundaries"
            ),
            Self::InvalidOrigin => {
                formatter.write_str("host transaction must use EditOrigin::Host")
            }
            Self::StaleRevision => formatter.write_str("host transaction revision is stale"),
            Self::TouchesEditableRegion => {
                formatter.write_str("host transaction touches an editable region")
            }
            Self::InvalidRegions(error) => write!(formatter, "invalid host regions: {error}"),
            Self::EditableRegionsChanged => formatter.write_str(
                "host transaction must preserve editable region identities and contents",
            ),
            Self::ProjectionRequired => formatter
                .write_str("host transaction must provide the next non-identity projection"),
            Self::InvalidProjection(error) => write!(formatter, "invalid projection: {error}"),
            Self::BlocksRequired => formatter
                .write_str("host transaction must provide the next inline block descriptors"),
            Self::InvalidBlocks(error) => write!(formatter, "invalid inline blocks: {error}"),
            Self::AnchoredBlocksRequired => {
                formatter.write_str("host transaction must provide the next anchored blocks")
            }
            Self::InvalidAnchoredBlocks(error) => {
                write!(formatter, "invalid anchored blocks: {error}")
            }
            Self::StylesRequired => formatter
                .write_str("host transaction must provide the next document style descriptors"),
            Self::InvalidStyles(error) => write!(formatter, "invalid document styles: {error}"),
        }
    }
}

impl Error for HostTransactionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentationError {
    Regions(RegionError),
    Projection(ProjectionError),
    Blocks(BlockError),
    AnchoredBlocks(AnchoredBlockError),
    Styles(DocumentStyleError),
    Position(PositionError),
}

impl fmt::Display for PresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Regions(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
            Self::Blocks(error) => error.fmt(formatter),
            Self::AnchoredBlocks(error) => error.fmt(formatter),
            Self::Styles(error) => error.fmt(formatter),
            Self::Position(error) => error.fmt(formatter),
        }
    }
}

impl Error for PresentationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructureError {
    InvalidBoundary,
    PrefixChanged,
    StaleReplay,
    MarkedText,
    Presentation(PresentationError),
}

impl fmt::Display for StructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundary => {
                formatter.write_str("structure edit must start at a region boundary")
            }
            Self::PrefixChanged => {
                formatter.write_str("structure edit must preserve the preceding document")
            }
            Self::StaleReplay => {
                formatter.write_str("structure replay no longer matches the document")
            }
            Self::MarkedText => formatter.write_str("structure edit cannot interrupt marked text"),
            Self::Presentation(error) => error.fmt(formatter),
        }
    }
}

impl Error for StructureError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrollPinError {
    InvalidViewportFraction,
    InvalidPosition(PositionError),
}

impl fmt::Display for ScrollPinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidViewportFraction => {
                formatter.write_str("scroll pin viewport fraction must be between 0 and 1")
            }
            Self::InvalidPosition(error) => write!(formatter, "invalid scroll pin: {error}"),
        }
    }
}

impl Error for ScrollPinError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutedEdit<I> {
    region_id: I,
    transaction: EditTransaction,
}

impl<I> RoutedEdit<I> {
    pub fn region_id(&self) -> &I {
        &self.region_id
    }

    pub fn transaction(&self) -> &EditTransaction {
        &self.transaction
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentEvent<I> {
    Changed {
        revision: DocumentRevision,
        origin: EditOrigin,
    },
    Routed(RoutedEdit<I>),
    StructureReplayRequested(StructureReplay<I>),
    ReplaceRequested(StructureReplacement<I>),
    Rejected(DocumentEditRejection),
    SelectionChanged,
    ProjectionChanged,
    PresentationChanged,
    StopFollowingRequested {
        pin_id: I,
    },
    /// The user moved the viewport with the wheel or scrollbar.
    ViewportScrolled,
    /// A layout placed another row at the viewport top, whether the user
    /// scrolled, a reveal or pin moved it, or the content above it changed.
    /// Read it with [`DocumentState::viewport_top`].
    ViewportTopChanged,
    /// A click that selected nothing landed on text styled
    /// [`with_link`](super::DocumentInlineStyle::with_link); carries its target.
    LinkClicked(SharedString),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTextContext<I> {
    node_id: I,
    source: Range<usize>,
    node_start: usize,
    first_line: bool,
    first_row: bool,
    last_row: bool,
}

impl<I> DocumentTextContext<I> {
    pub fn node_id(&self) -> &I {
        &self.node_id
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    /// Source coordinates relative to the owning node, independent of its document position.
    pub fn source_in_node(&self) -> Range<usize> {
        self.source.start.saturating_sub(self.node_start)
            ..self.source.end.saturating_sub(self.node_start)
    }

    pub fn is_first_line(&self) -> bool {
        self.first_line
    }

    /// Whether this is the first display row of its line. Without text rows,
    /// every line is one row.
    pub fn is_first_row(&self) -> bool {
        self.first_row
    }

    /// Whether this is the last display row of its line.
    pub fn is_last_row(&self) -> bool {
        self.last_row
    }
}

#[derive(Clone)]
pub struct DocumentSnapshot<I> {
    text: String,
    regions: Vec<DocumentRegion<I>>,
    projection: DocumentProjection,
    blocks: Vec<DocumentBlock<I>>,
    anchored_blocks: Vec<AnchoredBlock<I>>,
    styles: DocumentStyles,
    selection: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
}

impl<I: Clone> DocumentSnapshot<I> {
    pub fn new(
        text: impl Into<String>,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        styles: DocumentStyles,
    ) -> Self {
        Self {
            text: text.into(),
            regions,
            projection,
            blocks,
            anchored_blocks: Vec::new(),
            styles,
            selection: None,
        }
    }

    pub fn selection(mut self, selection: DocumentPosition<I>) -> Self {
        self.selection = Some((selection.clone(), selection));
        self
    }

    pub fn selection_range(
        mut self,
        anchor: DocumentPosition<I>,
        head: DocumentPosition<I>,
    ) -> Self {
        self.selection = Some((anchor, head));
        self
    }

    pub fn anchored_blocks(mut self, blocks: Vec<AnchoredBlock<I>>) -> Self {
        self.anchored_blocks = blocks;
        self
    }
}

/// The document suffix owned by an application editor, relative to its start.
/// Host-owned history is never stored in an undo record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentStructure<I> {
    text: String,
    regions: Vec<DocumentRegion<I>>,
}

impl<I> DocumentStructure<I> {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn regions(&self) -> &[DocumentRegion<I>] {
        &self.regions
    }

    fn allocated_bytes(&self) -> usize {
        self.text.capacity() + self.regions.capacity() * std::mem::size_of::<DocumentRegion<I>>()
    }
}

impl<I: Clone + Eq + Hash> DocumentStructure<I> {
    fn suffix(model: &DocumentModel<I>, start: usize) -> Option<Self> {
        if start > model.text.len() || model.text.clip_offset(start, Bias::Left) != start {
            return None;
        }
        if model.regions.as_slice().iter().any(|region| {
            let range = region.range();
            range.start < start && range.end > start
        }) {
            return None;
        }
        let regions = model
            .regions
            .as_slice()
            .iter()
            .filter(|region| {
                region.range().start >= start
                    // Empty readonly nodes at the boundary belong to the host
                    // prefix. Retaining them would make user undo depend on a
                    // submitted node that acceptance is allowed to remove.
                    && !(region.range().start == start
                        && region.range().is_empty()
                        && region.policy() == EditPolicy::Readonly)
                    && matches!(
                        region.policy(),
                        EditPolicy::Editable | EditPolicy::Atomic | EditPolicy::Readonly
                    )
            })
            .map(|region| {
                let range = region.range();
                DocumentRegion::new(
                    region.id().clone(),
                    range.start - start..range.end - start,
                    region.policy(),
                )
            })
            .collect::<Vec<_>>();
        let mut end = 0;
        for region in &regions {
            let range = region.range();
            if range.start != end {
                return None;
            }
            end = range.end;
        }
        (!regions.is_empty() && end == model.text.len() - start).then(|| Self {
            text: model.text.slice(start..model.text.len()).to_string(),
            regions,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureReplay<I> {
    target: DocumentStructure<I>,
    selection: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
    undo: bool,
    start_node: I,
}

impl<I> StructureReplay<I> {
    pub fn target(&self) -> &DocumentStructure<I> {
        &self.target
    }

    pub fn selection(&self) -> Option<&(DocumentPosition<I>, DocumentPosition<I>)> {
        self.selection.as_ref()
    }

    pub fn is_undo(&self) -> bool {
        self.undo
    }

    pub fn start_node(&self) -> &I {
        &self.start_node
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AnchorRecord<I> {
    offset: usize,
    bias: AnchorBias,
    node_id: Option<I>,
    affinity: Affinity,
    /// Text replaced around the anchor moves it to the replacement's start,
    /// while an insertion at it moves it past the inserted text.
    content: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct AnchorStore<I> {
    next_id: u64,
    anchors: BTreeMap<DocumentAnchor, AnchorRecord<I>>,
}

impl<I> Default for AnchorStore<I> {
    fn default() -> Self {
        Self {
            next_id: 0,
            anchors: BTreeMap::new(),
        }
    }
}

impl<I> AnchorStore<I> {
    fn create_for_node(
        &mut self,
        offset: usize,
        bias: AnchorBias,
        node_id: Option<I>,
        affinity: Affinity,
    ) -> DocumentAnchor {
        let anchor = DocumentAnchor::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.anchors.insert(
            anchor,
            AnchorRecord {
                offset,
                bias,
                node_id,
                affinity,
                content: false,
            },
        );
        anchor
    }

    /// An anchor on the content at `offset`, whichever node owns it.
    fn create_for_content(&mut self, offset: usize) -> DocumentAnchor {
        let anchor = self.create_for_node(offset, AnchorBias::Right, None, Affinity::After);
        self.anchors
            .get_mut(&anchor)
            .expect("anchor was just created")
            .content = true;
        anchor
    }

    fn resolve(&self, anchor: DocumentAnchor) -> Option<usize> {
        self.anchors.get(&anchor).map(|record| record.offset)
    }

    fn remove(&mut self, anchor: DocumentAnchor) {
        self.anchors.remove(&anchor);
    }

    fn apply_edit(&mut self, range: &Range<usize>, replacement_len: usize) {
        for record in self.anchors.values_mut() {
            record.offset = if record.content && range.contains(&record.offset) {
                range.start
            } else {
                transform_offset(record.offset, record.bias, range, replacement_len)
            };
        }
    }
}

enum RoutingOutcome<I> {
    Applied,
    Routed(I),
    Rejected(DocumentEditRejection),
}

#[derive(Clone, Copy, Debug)]
struct RelativeSelection {
    anchor: usize,
    head: usize,
}

#[derive(Clone, Debug)]
struct RegionSelection<I> {
    region_id: I,
    selection: RelativeSelection,
    affinities: (Affinity, Affinity),
}

// A record owns only replaced text, in region-relative coordinates. Multiple
// changes are in application order; undo replays them in reverse.
#[derive(Clone, Debug)]
struct UndoChange {
    range: Range<usize>,
    removed: String,
    inserted: String,
}

#[derive(Clone, Debug)]
enum UndoKind<I> {
    Text {
        region_id: I,
        changes: Vec<UndoChange>,
    },
    Structure {
        before: DocumentStructure<I>,
        after: DocumentStructure<I>,
    },
}

#[derive(Clone, Debug)]
struct UndoRecord<I> {
    sequence: u64,
    kind: UndoKind<I>,
    selection_before: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
    selection_after: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
}

impl<I> UndoRecord<I> {
    #[cfg(test)]
    fn bytes(&self) -> usize {
        match &self.kind {
            UndoKind::Text { changes, .. } => changes
                .iter()
                .map(|change| change.removed.len() + change.inserted.len())
                .sum(),
            UndoKind::Structure { before, after } => before.text.len() + after.text.len(),
        }
    }

    fn allocated_bytes(&self) -> usize {
        match &self.kind {
            UndoKind::Text { changes, .. } => changes
                .iter()
                .map(|change| change.removed.capacity() + change.inserted.capacity())
                .sum(),
            UndoKind::Structure { before, after } => {
                before.allocated_bytes() + after.allocated_bytes()
            }
        }
    }
}

const MAX_UNDO_RECORDS: usize = 1000;
const MAX_UNDO_CHANGES: usize = 1000;
const MAX_UNDO_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
struct DocumentUndoManager<I> {
    undo: Vec<UndoRecord<I>>,
    redo: Vec<UndoRecord<I>>,
    coalescing: Option<EditOrigin>,
    next_sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UndoCheckpoint(u64);

impl<I> Default for DocumentUndoManager<I> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing: None,
            next_sequence: 0,
        }
    }
}

impl<I: Clone + Eq + Hash> DocumentUndoManager<I> {
    fn checkpoint(&mut self) -> UndoCheckpoint {
        self.break_coalescing();
        UndoCheckpoint(self.next_sequence)
    }

    fn retire_before(&mut self, checkpoint: UndoCheckpoint) {
        self.break_coalescing();
        self.undo.retain(|record| record.sequence >= checkpoint.0);
        self.redo.retain(|record| record.sequence >= checkpoint.0);
    }

    fn record(&mut self, mut record: UndoRecord<I>, origin: EditOrigin) {
        self.redo.clear();
        record.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let previous = self.undo.last_mut().filter(|previous| {
            self.coalescing == Some(origin)
                && matches!(
                    (&previous.kind, &record.kind),
                    (UndoKind::Text { region_id: left, .. }, UndoKind::Text { region_id: right, .. })
                        if left == right
                )
                && previous.selection_after == record.selection_before
        });
        let merged = previous.is_some_and(|previous| {
            let (UndoKind::Text { changes: left, .. }, UndoKind::Text { changes: right, .. }) =
                (&mut previous.kind, &mut record.kind)
            else {
                unreachable!("only text records coalesce")
            };
            if origin == EditOrigin::Composition {
                if let ([change], [next]) = (left.as_mut_slice(), right.as_slice())
                    && next.range
                        == (change.range.start..change.range.start + change.inserted.len())
                    && next.removed == change.inserted
                {
                    if change.removed.len() + next.inserted.len() > MAX_UNDO_BYTES {
                        return false;
                    }
                    change.inserted = next.inserted.clone();
                } else if left.len() + right.len() <= MAX_UNDO_CHANGES {
                    if left
                        .iter()
                        .chain(right.iter())
                        .map(|change| change.removed.len() + change.inserted.len())
                        .sum::<usize>()
                        > MAX_UNDO_BYTES
                    {
                        return false;
                    }
                    left.append(right);
                } else {
                    return false;
                }
            } else if origin == EditOrigin::User {
                let ([left], [right]) = (left.as_mut_slice(), right.as_slice()) else {
                    return false;
                };
                if left.removed.len()
                    + left.inserted.len()
                    + right.removed.len()
                    + right.inserted.len()
                    > MAX_UNDO_BYTES
                {
                    return false;
                }
                if left.removed.is_empty()
                    && right.removed.is_empty()
                    && right.range.start == left.range.start + left.inserted.len()
                    && !left.inserted.contains(['\n', '\r'])
                    && !right.inserted.contains(['\n', '\r'])
                {
                    left.inserted.push_str(&right.inserted);
                } else if left.inserted.is_empty()
                    && right.inserted.is_empty()
                    && right.range.end == left.range.start
                {
                    left.range.start = right.range.start;
                    left.removed.insert_str(0, &right.removed);
                } else if left.inserted.is_empty()
                    && right.inserted.is_empty()
                    && right.range.start == left.range.start
                {
                    left.range.end += right.removed.len();
                    left.removed.push_str(&right.removed);
                } else {
                    return false;
                }
            } else {
                return false;
            }
            previous.selection_after.clone_from(&record.selection_after);
            true
        });
        if !merged {
            self.undo.push(record);
        }
        // A canceled composition contributes no text change and must not hide
        // the previous undo entry.
        if self.undo.last().is_some_and(|record| matches!(&record.kind,
            UndoKind::Text { changes, .. } if changes.iter().all(|change| change.removed == change.inserted))) {
            self.undo.pop();
            self.break_coalescing();
        } else {
            self.coalescing =
                matches!(origin, EditOrigin::User | EditOrigin::Composition).then_some(origin);
        }
        self.trim(MAX_UNDO_RECORDS, MAX_UNDO_BYTES);
    }

    fn trim(&mut self, records: usize, bytes: usize) {
        if self.undo.last().is_some_and(|record| {
            matches!(&record.kind,
            UndoKind::Text { changes, .. } if changes.len() > MAX_UNDO_CHANGES)
        }) {
            self.undo.clear();
        }
        let mut retained: usize = self.undo.iter().map(UndoRecord::allocated_bytes).sum();
        while self.undo.len() > records || retained > bytes {
            retained -= self.undo.remove(0).allocated_bytes();
        }
        // An individual edit larger than the budget is deliberately not retained.
        if self.undo.is_empty() {
            self.break_coalescing();
        }
    }

    fn finish_composition(&mut self) {
        if self.coalescing == Some(EditOrigin::Composition) {
            self.break_coalescing();
        }
    }

    fn break_coalescing(&mut self) {
        self.coalescing = None;
    }
}

/// Where generated layout items go: a vector, or a tree built chunk by chunk.
trait ItemSink<I> {
    fn push_item(&mut self, item: DocumentLayoutItem<I>);
}

impl<I> ItemSink<I> for Vec<DocumentLayoutItem<I>> {
    fn push_item(&mut self, item: DocumentLayoutItem<I>) {
        self.push(item);
    }
}

impl<I: Clone> ItemSink<I> for layout_items::LayoutItemsBuilder<I> {
    fn push_item(&mut self, item: DocumentLayoutItem<I>) {
        self.push(item);
    }
}

/// Where the lookups for the previous layout item ended. Items built in
/// document order resume there instead of searching from the start.
#[derive(Clone, Copy, Default)]
struct LayoutCursor {
    run: usize,
    region: usize,
    paragraph: usize,
    inline: usize,
}

#[derive(Clone)]
struct DocumentModel<I> {
    text: Rope,
    revision: DocumentRevision,
    regions: DocumentRegions<I>,
    anchors: AnchorStore<I>,
    selection: DocumentSelection,
    marked: Option<DocumentSelection>,
    projection: DocumentProjection,
    projection_map: ProjectionMap,
    blocks: Vec<DocumentBlock<I>>,
    anchored_blocks: Vec<AnchoredBlock<I>>,
    styles: DocumentStyles,
    resolved_styles: ResolvedDocumentStyles,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ResolvedDocumentStyles {
    paragraphs: Vec<ResolvedParagraphStyle>,
    inline: Vec<ResolvedInlineStyle>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ResolvedParagraphStyle {
    source: Range<usize>,
    display: Range<usize>,
    text_style: TextStyleRefinement,
    hanging_marker: Option<SharedString>,
    table_row: Option<super::TableRow>,
}

impl ResolvedParagraphStyle {
    fn shift(&mut self, source_delta: isize, display_delta: isize) {
        self.source = shift_offset(self.source.start, source_delta)
            ..shift_offset(self.source.end, source_delta);
        self.display = shift_offset(self.display.start, display_delta)
            ..shift_offset(self.display.end, display_delta);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedInlineStyle {
    source: Range<usize>,
    display: Range<usize>,
    highlight: HighlightStyle,
    font_family: Option<SharedString>,
    font_scale: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DocumentTextPresentation {
    pub(super) table_row: Option<super::TableRow>,
    pub(super) text_style: Option<TextStyleRefinement>,
    pub(super) indent: Option<DocumentTextIndent>,
    pub(super) highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub(super) font_family_overrides: Vec<(Range<usize>, SharedString)>,
    pub(super) font_scales: Vec<(Range<usize>, u8)>,
    pub(super) objects: Vec<(Range<usize>, super::InlineSize)>,
}

/// Aligns a line of a paragraph with the text after its hanging marker.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DocumentTextIndent {
    pub(super) marker: SharedString,
    /// The paragraph's first line shows the marker and indents only the rows
    /// it wraps onto; later lines indent every row.
    pub(super) hanging: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum DocumentLayoutItem<I> {
    Text {
        display: Range<usize>,
        source: Range<usize>,
        node_id: Option<I>,
        /// Identifies the row's text without holding it; the text is sliced
        /// from the display text when the row is drawn.
        fingerprint: u64,
        /// `None` for unstyled text, which then costs no allocation. Shared,
        /// so attaching rows or updating their geometry copies no styles.
        presentation: Option<std::sync::Arc<DocumentTextPresentation>>,
        /// The display line this row belongs to. Without text rows, every line
        /// is one row.
        line: Range<usize>,
        /// Identifies the line's text and fonts, keying its wrap in text rows.
        line_key: u64,
        /// The line's resolved frame in text rows, once layout resolved it.
        frame: Derived<Option<text_rows::RowFrame>>,
        /// What the line's rows were wrapped for, once text rows wrapped it.
        wrapped: Derived<Option<text_rows::WrapKey>>,
        /// A table row's cells, once text rows laid out its table. It moves
        /// with the row; layout replaces it when the row's inputs change.
        table: Derived<Option<std::sync::Arc<table::Resolved>>>,
    },
    Block {
        id: I,
        source: Range<usize>,
        height: Derived<text_rows::BlockGeometry>,
    },
    AnchoredBlock {
        id: I,
        source: usize,
        height: Derived<text_rows::BlockGeometry>,
    },
    Trailer,
}

/// Geometry a layout pass derives for an item. It is not part of the item's
/// identity: equal items compare equal whatever geometry they hold.
#[derive(Clone, Copy, Debug, Default)]
struct Derived<T>(T);

impl<T> PartialEq for Derived<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<I: Eq> DocumentLayoutItem<I> {
    fn presentation(&self) -> Option<&DocumentTextPresentation> {
        match self {
            Self::Text { presentation, .. } => presentation.as_deref(),
            _ => None,
        }
    }
    fn same_layout_identity(&self, other: &Self) -> bool {
        self.same_row(other) && self.node() == other.node()
    }

    /// The same row, whichever node owns it: a list slot belongs to the row's
    /// content and geometry, so a node renamed around unchanged text keeps it.
    fn same_row(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Text {
                    display: left_display,
                    fingerprint: left,
                    presentation: left_presentation,
                    line: left_line,
                    line_key: left_key,
                    ..
                },
                Self::Text {
                    display: right_display,
                    fingerprint: right,
                    presentation: right_presentation,
                    line: right_line,
                    line_key: right_key,
                    ..
                },
            ) => {
                // A row keeps its place in its line, wherever the line moved.
                left == right
                    && left_presentation == right_presentation
                    && left_key == right_key
                    && left_line.len() == right_line.len()
                    && left_display.start - left_line.start
                        == right_display.start - right_line.start
            }
            (Self::Block { id: left, .. }, Self::Block { id: right, .. }) => left == right,
            (
                Self::AnchoredBlock {
                    id: left,
                    source: left_source,
                    ..
                },
                Self::AnchoredBlock {
                    id: right,
                    source: right_source,
                    ..
                },
            ) => left == right && left_source == right_source,
            (Self::Trailer, Self::Trailer) => true,
            _ => false,
        }
    }
}

impl<I: Clone + Eq + Hash> DocumentModel<I> {
    fn new(text: impl Into<String>, regions: Vec<DocumentRegion<I>>) -> Result<Self, RegionError> {
        let text = Rope::from(text.into());
        let regions = DocumentRegions::new(regions, text.len())?;
        validate_region_boundaries(&regions, &text)?;
        let initial_region = regions
            .as_slice()
            .iter()
            .rev()
            .find(|region| region.policy() == EditPolicy::Editable);
        let initial_offset = initial_region.map_or(0, |region| region.range().start);
        let mut anchors = AnchorStore::default();
        let cursor = anchors.create_for_node(
            initial_offset,
            AnchorBias::Right,
            initial_region.map(|region| region.id().clone()),
            Affinity::After,
        );
        let projection = DocumentProjection::identity(text.len());
        let projection_map = ProjectionMap::new(&text, &projection);

        Ok(Self {
            text,
            revision: DocumentRevision::INITIAL,
            regions,
            anchors,
            selection: DocumentSelection::new(cursor, cursor),
            marked: None,
            projection,
            projection_map,
            blocks: Vec::new(),
            anchored_blocks: Vec::new(),
            styles: DocumentStyles::default(),
            resolved_styles: ResolvedDocumentStyles::default(),
        })
    }

    fn text(&self) -> String {
        self.text.to_string()
    }

    fn display_text(&self) -> &str {
        self.projection_map.display_text()
    }

    fn set_projection(&mut self, projection: DocumentProjection) -> Result<(), ProjectionError> {
        if !self.blocks.is_empty() || !self.anchored_blocks.is_empty() {
            return Err(ProjectionError::BlocksRequirePresentation);
        }
        validate_projection(&projection, &self.regions, &self.text)?;
        self.projection_map = ProjectionMap::new(&self.text, &projection);
        self.projection = projection;
        self.styles = DocumentStyles::default();
        self.resolved_styles = ResolvedDocumentStyles::default();
        Ok(())
    }

    fn set_presentation(
        &mut self,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
    ) -> Result<(), PresentationError> {
        self.set_rich_presentation(projection, blocks, DocumentStyles::default())
    }

    fn set_rich_presentation(
        &mut self,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        styles: DocumentStyles,
    ) -> Result<(), PresentationError> {
        self.set_rich_presentation_with_anchors(projection, blocks, Vec::new(), styles)
    }

    fn set_rich_presentation_with_anchors(
        &mut self,
        projection: DocumentProjection,
        mut blocks: Vec<DocumentBlock<I>>,
        anchored_blocks: Vec<AnchoredBlock<I>>,
        styles: DocumentStyles,
    ) -> Result<(), PresentationError> {
        blocks.sort_by_key(|block| {
            let source = block.source();
            (source.start, source.end)
        });
        validate_projection(&projection, &self.regions, &self.text)
            .map_err(PresentationError::Projection)?;
        validate_blocks(&blocks, &self.regions, &projection, self.text.len())
            .map_err(PresentationError::Blocks)?;
        let projection_map = ProjectionMap::new(&self.text, &projection);
        Self::validate_anchored_blocks(
            &anchored_blocks,
            &blocks,
            &projection_map,
            &self.regions,
            &self.text,
        )
        .map_err(PresentationError::AnchoredBlocks)?;
        let resolved_styles =
            resolve_document_styles(&self.text, &self.regions, &projection_map, &styles, &blocks)
                .map_err(PresentationError::Styles)?;
        self.projection_map = projection_map;
        self.projection = projection;
        self.blocks = blocks;
        self.anchored_blocks = anchored_blocks;
        self.styles = styles;
        self.resolved_styles = resolved_styles;
        Ok(())
    }

    fn validate_anchored_blocks(
        anchored: &[AnchoredBlock<I>],
        blocks: &[DocumentBlock<I>],
        projection: &ProjectionMap,
        regions: &DocumentRegions<I>,
        text: &Rope,
    ) -> Result<(), AnchoredBlockError> {
        if anchored.is_empty() {
            return Ok(());
        }
        let regions_by_id: HashMap<&I, &DocumentRegion<I>> = regions
            .as_slice()
            .iter()
            .map(|region| (region.id(), region))
            .collect();
        let mut ids: HashSet<&I> = blocks.iter().map(|block| block.id()).collect();
        for block in anchored {
            if regions_by_id.contains_key(block.id()) || !ids.insert(block.id()) {
                return Err(AnchoredBlockError::DuplicateId);
            }
            let region = regions_by_id
                .get(block.position().node_id())
                .copied()
                .ok_or(AnchoredBlockError::InvalidPosition(
                    PositionError::NodeNotFound,
                ))?;
            if matches!(region.policy(), EditPolicy::Editable | EditPolicy::Atomic) {
                return Err(AnchoredBlockError::EditableRegion);
            }
            let (source, _) = anchored_display_offset(
                block,
                |position| {
                    let range = region.range();
                    if position.offset() > range.len() {
                        return Err(PositionError::NodeOffsetOutOfBounds {
                            offset: position.offset(),
                            node_len: range.len(),
                        });
                    }
                    Ok(range.start + position.offset())
                },
                |offset, affinity| projection.source_to_display(offset, affinity),
                projection.display_text(),
            )?;
            if text.clip_offset(source, Bias::Left) != source {
                return Err(AnchoredBlockError::NotLineBoundary);
            }
        }
        Ok(())
    }

    fn source_to_display(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        self.projection_map.source_to_display(offset, affinity)
    }

    fn display_to_source(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        self.projection_map.display_to_source(offset, affinity)
    }

    /// Items for the document, with text lines split at the display offsets in
    /// `row_starts` (ascending) when it lays out display rows.
    /// Layout items in document order. With `rows`, each line splits into the
    /// display rows last wrapped for its text and fonts; otherwise a line is one item.
    fn layout_items(&self, rows: Option<&text_rows::RowCache>) -> Vec<DocumentLayoutItem<I>> {
        self.layout_items_of_nodes(0..self.regions.as_slice().len(), rows)
    }

    /// Whether the layout splits cleanly before node `ix`: its items start a
    /// display line, after every item of earlier nodes. The document ends are
    /// seams. Nodes between two seams lay out without the rest of the document.
    fn is_layout_seam(&self, ix: usize) -> bool {
        let regions = self.regions.as_slice();
        if ix == 0 || ix >= regions.len() {
            return true;
        }
        self.source_to_display(regions[ix].range().start, Affinity::Before)
            .is_some_and(|display| {
                display == 0 || self.display_text().as_bytes().get(display - 1) == Some(&b'\n')
            })
    }

    /// The layout items of the nodes in `nodes`, which start and end at
    /// layout seams ([`Self::is_layout_seam`]), as [`Self::layout_items`]
    /// places them.
    fn layout_items_of_nodes(
        &self,
        nodes: Range<usize>,
        rows: Option<&text_rows::RowCache>,
    ) -> Vec<DocumentLayoutItem<I>> {
        let mut items = Vec::new();
        self.fill_layout_items_of_nodes(nodes.clone(), rows, &mut items);
        if self.anchored_blocks.is_empty() {
            return items;
        }
        let regions = self.regions.as_slice();
        let all = nodes.start == 0 && nodes.end == regions.len();
        let display_text = self.display_text();
        // Each anchored block goes before the first text or block item at or after
        // its offset, after anchored blocks at earlier offsets. Text and blocks
        // are in document order, so one merge places them all. A block anchored
        // at the end of the last node goes after every item here, which is
        // before the first item of the next node.
        let region_ix: HashMap<&I, usize> = regions[nodes.clone()]
            .iter()
            .zip(nodes.clone())
            .map(|(region, ix)| (region.id(), ix))
            .collect();
        let mut anchored: Vec<_> = self
            .anchored_blocks
            .iter()
            .filter(|block| all || region_ix.contains_key(block.position().node_id()))
            .map(|block| {
                let (source, display) = anchored_display_offset(
                    block,
                    |position| {
                        self.resolve_position_in(
                            position,
                            region_ix.get(position.node_id()).copied(),
                        )
                    },
                    |offset, affinity| self.source_to_display(offset, affinity),
                    display_text,
                )
                .expect("validated anchored block must map to a line boundary");
                let index = items.partition_point(|item| match item {
                    DocumentLayoutItem::Text {
                        display: range,
                        source: text,
                        ..
                    } => !(range.start >= display && text.start >= source),
                    DocumentLayoutItem::Block { source: range, .. } => range.start < source,
                    DocumentLayoutItem::AnchoredBlock { .. } => true,
                    DocumentLayoutItem::Trailer => false,
                });
                (index, source, block.id().clone())
            })
            .collect();
        anchored.sort_by_key(|(index, source, _)| (*index, *source));
        let mut merged = Vec::with_capacity(items.len() + anchored.len());
        let mut anchored = anchored.into_iter().peekable();
        for (ix, item) in items.into_iter().enumerate() {
            while let Some((_, source, id)) = anchored.next_if(|(index, _, _)| *index == ix) {
                merged.push(DocumentLayoutItem::AnchoredBlock {
                    id,
                    source,
                    height: Derived::default(),
                });
            }
            merged.push(item);
        }
        merged.extend(
            anchored.map(|(_, source, id)| DocumentLayoutItem::AnchoredBlock {
                id,
                source,
                height: Derived::default(),
            }),
        );
        merged
    }

    /// The whole document's layout items as a tree, built without first
    /// collecting them.
    fn layout_items_tree(&self, rows: Option<&text_rows::RowCache>) -> LayoutItems<I> {
        // Anchored blocks are placed among the generated items afterwards.
        if !self.anchored_blocks.is_empty() {
            return self.layout_items(rows).into();
        }
        let mut items = layout_items::LayoutItemsBuilder::new();
        self.fill_layout_items_of_nodes(0..self.regions.as_slice().len(), rows, &mut items);
        items.finish()
    }

    fn fill_layout_items_of_nodes(
        &self,
        nodes: Range<usize>,
        rows: Option<&text_rows::RowCache>,
        items: &mut impl ItemSink<I>,
    ) {
        let regions = self.regions.as_slice();
        let display_text = self.display_text();
        let (mut source_cursor, mut display_cursor) = match regions.get(nodes.start) {
            Some(region) if nodes.start > 0 => {
                let start = region.range().start;
                let display = self
                    .source_to_display(start, Affinity::Before)
                    .expect("a node start maps to display");
                (start, display)
            }
            _ => (0, 0),
        };
        let (source_end, display_end) = match regions.get(nodes.end) {
            Some(region) => {
                let end = region.range().start;
                let display = self
                    .source_to_display(end, Affinity::Before)
                    .expect("a node start maps to display");
                (end, display)
            }
            None => (self.text.len(), display_text.len()),
        };
        let mut cursor = LayoutCursor::default();
        // Blocks of earlier nodes start before the first node. Only empty nodes
        // share a start, and they have no blocks.
        let first_block = if nodes.start == 0 {
            0
        } else {
            self.blocks
                .partition_point(|block| block.source().start < source_cursor)
        };
        let mut blocks = self.blocks[first_block..].iter().peekable();
        for (region_ix, region) in regions.iter().enumerate().take(nodes.end).skip(nodes.start) {
            let block = blocks
                .peek()
                .filter(|block| block.id() == region.id())
                .copied();
            let empty_text = region.policy() == EditPolicy::Editable
                && region.range().is_empty()
                && region_ix + 1 != self.regions.as_slice().len()
                && !self.blocks.iter().any(|block| {
                    block.inline_size().is_some()
                        && (block.source().start == region.range().start
                            || block.source().end == region.range().start)
                });
            if block.is_some_and(|block| block.inline_size().is_some()) {
                blocks.next();
                continue;
            }
            if block.is_none() && !empty_text {
                continue;
            }
            let source = region.range();
            let display_start = self
                .source_to_display(source.start, Affinity::Before)
                .expect("validated block source must map to display");
            let display_end = self
                .source_to_display(source.end, Affinity::After)
                .expect("validated block source must map to display");
            self.push_text_layout_items(
                items,
                display_cursor..display_start,
                source_cursor..source.start,
                false,
                rows,
                &mut cursor,
            );
            if let Some(block) = block {
                items.push_item(DocumentLayoutItem::Block {
                    id: block.id().clone(),
                    source: source.clone(),
                    height: Derived::default(),
                });
                blocks.next();
            } else {
                let empty = display_start..display_start;
                let mut item = self.text_layout_item_from(
                    display_text,
                    empty.clone(),
                    source.clone(),
                    empty,
                    0,
                    &mut cursor,
                );
                if let DocumentLayoutItem::Text { node_id, .. } = &mut item {
                    *node_id = Some(region.id().clone());
                }
                items.push_item(item);
            }
            display_cursor = display_end;
            source_cursor = source.end;
        }
        // Only the document end has a line after its last line break; a seam
        // starts the next node's first line.
        self.push_text_layout_items(
            items,
            display_cursor..display_end,
            source_cursor..source_end,
            nodes.end == regions.len(),
            rows,
            &mut cursor,
        );
    }

    fn push_text_layout_items(
        &self,
        items: &mut impl ItemSink<I>,
        range: Range<usize>,
        source: Range<usize>,
        include_empty_tail: bool,
        rows: Option<&text_rows::RowCache>,
        cursor: &mut LayoutCursor,
    ) {
        let display_text = self.display_text();
        let mut push_line = |line: Range<usize>| {
            // Every line is keyed, so turning text rows on keeps these items.
            let line_cursor = *cursor;
            let mut whole = self.text_layout_item_from(
                display_text,
                line.clone(),
                source.clone(),
                line.clone(),
                0,
                cursor,
            );
            let line_key = text_rows::line_key(&whole);
            let starts = rows.and_then(|rows| rows.row_starts(line_key));
            if starts.is_none_or(|starts| starts.is_empty()) {
                if let DocumentLayoutItem::Text { line_key: key, .. } = &mut whole {
                    *key = line_key;
                }
                items.push_item(whole);
                return;
            }
            // The rows start where the line did.
            *cursor = line_cursor;
            for row in text_rows::rows_of_line(line.clone(), starts.unwrap().starts()) {
                items.push_item(self.text_layout_item_from(
                    display_text,
                    row,
                    source.clone(),
                    line.clone(),
                    line_key,
                    cursor,
                ));
            }
        };
        let mut start = range.start;
        for newline in display_text[range.clone()].match_indices('\n') {
            // The line terminator separates layout items. Passing it to StyledText
            // creates another visual row inside each item and doubles line spacing.
            let end = range.start + newline.0;
            push_line(start..end);
            start = end + 1;
        }
        if start < range.end || (include_empty_tail && start == range.end) {
            push_line(start..range.end);
        }
    }

    fn text_layout_item(
        &self,
        display_text: &str,
        display: Range<usize>,
        segment: Range<usize>,
        line: Range<usize>,
        line_key: u64,
    ) -> DocumentLayoutItem<I> {
        self.text_layout_item_from(
            display_text,
            display,
            segment,
            line,
            line_key,
            &mut LayoutCursor::default(),
        )
    }

    /// `text_layout_item`, with its lookups resuming at `cursor`. A pass over
    /// lines in document order then avoids a full search per line.
    fn text_layout_item_from(
        &self,
        display_text: &str,
        display: Range<usize>,
        segment: Range<usize>,
        line: Range<usize>,
        line_key: u64,
        cursor: &mut LayoutCursor,
    ) -> DocumentLayoutItem<I> {
        // A hidden object shares display offsets with both adjacent text runs.
        // Keep the source extent so caret ownership does not cross that object.
        // A line's end owns the markup its text hides there, such as a closing
        // delimiter; a soft-wrapped row's end leaves the rest to the next row.
        let source_start = self
            .projection_map
            .display_to_source_from(display.start, Affinity::After, &mut cursor.run)
            .unwrap_or(segment.end)
            .clamp(segment.start, segment.end);
        let mut source_end = self
            .projection_map
            .display_to_source_from(display.end, Affinity::Before, &mut cursor.run)
            .unwrap_or(segment.end);
        if display.end == line.end {
            // A generated line break can map back over the text before it;
            // the end only ever moves forward over hidden markup.
            source_end = self
                .projection_map
                .display_to_source_from(display.end, Affinity::After, &mut cursor.run)
                .map_or(source_end, |after| after.max(source_end));
        }
        let source_end = source_end.clamp(source_start, segment.end);
        let source = source_start..source_end;
        let mut node_id = self.node_after_from(source_start, &mut cursor.region);
        let objects = self.inline_objects_in(display.clone());
        // A mixed paragraph can span several domain nodes. All its display
        // rows share the paragraph identity; source positions keep their own IDs.
        if !self.inline_objects_in(line.clone()).is_empty() {
            let start = self
                .display_to_source(line.start, Affinity::After)
                .unwrap_or(source_start);
            node_id = self
                .position_for_offset(start, Affinity::After)
                .ok()
                .map(DocumentPosition::into_node_id);
        }
        // Source styles are ordered and nonoverlapping; projection preserves
        // endpoint order (but can collapse or overlap display ranges). Find the
        // first end that can contain this line, then check its start. At a shared
        // display end, skip a paragraph whose source ends before this row.
        let paragraphs = &self.resolved_styles.paragraphs;
        let from = partition_from(paragraphs, cursor.paragraph, |style| {
            style.display.end < display.end
                || (style.display.end == display.start
                    && style.source.start < source.start
                    && style.source.end <= source.start)
        });
        cursor.paragraph = from;
        let paragraph = paragraphs
            .get(from)
            .filter(|style| style.display.start <= display.start);
        let table = paragraph
            .filter(|style| style.display.start == line.start && style.table_row.is_some());
        let text_style = paragraph.map(|style| style.text_style.clone());
        let indent = paragraph.and_then(|style| {
            Some(DocumentTextIndent {
                marker: style.hanging_marker.clone()?,
                hanging: style.display.start == display.start,
            })
        });
        let mut highlights = Vec::new();
        let mut font_family_overrides = Vec::new();
        let mut font_scales = Vec::new();
        // Resolved inline endpoints are monotonic. A line only
        // needs its intersecting styles, even in a document with many matches.
        let inline = &self.resolved_styles.inline;
        let from = partition_from(inline, cursor.inline, |style| {
            style.display.end <= display.start
        });
        cursor.inline = from;
        for inline in inline[from..]
            .iter()
            .take_while(|style| style.display.start < display.end)
        {
            let start = display.start.max(inline.display.start);
            let end = display.end.min(inline.display.end);
            if start >= end {
                continue;
            }
            let local = start - display.start..end - display.start;
            highlights.push((local.clone(), inline.highlight));
            if let Some(font_family) = &inline.font_family {
                font_family_overrides.push((local.clone(), font_family.clone()));
            }
            if let Some(scale) = inline.font_scale {
                font_scales.push((local, scale));
            }
        }
        // Most lines carry no styles; they skip building and comparing one.
        let styled = text_style.is_some()
            || indent.is_some()
            || !highlights.is_empty()
            || !font_family_overrides.is_empty()
            || !objects.is_empty();
        let presentation = styled
            .then(|| {
                let mut presentation = DocumentTextPresentation {
                    table_row: table.and_then(|style| style.table_row.clone()),
                    text_style,
                    indent,
                    highlights,
                    font_family_overrides,
                    font_scales,
                    objects,
                };
                presentation.resolve_inline(display.len());
                debug_assert!(presentation.highlights.iter().all(|(range, _)| {
                    display_text.is_char_boundary(display.start + range.start)
                        && display_text.is_char_boundary(display.start + range.end)
                }));
                presentation
            })
            .filter(|presentation| *presentation != DocumentTextPresentation::default())
            .map(std::sync::Arc::new);
        DocumentLayoutItem::Text {
            fingerprint: text_fingerprint(&display_text[display.clone()]),
            display,
            source,
            node_id,
            presentation,
            line,
            line_key,
            frame: Derived::default(),
            wrapped: Derived::default(),
            table: Derived::default(),
        }
    }

    fn selection_offsets(&self) -> (usize, usize) {
        (
            self.anchors
                .resolve(self.selection.anchor())
                .expect("document selection anchor must be live"),
            self.anchors
                .resolve(self.selection.head())
                .expect("document selection head must be live"),
        )
    }

    fn selected_range(&self) -> Range<usize> {
        let (anchor, head) = self.selection_offsets();
        anchor.min(head)..anchor.max(head)
    }

    fn selection_reversed(&self) -> bool {
        let (anchor, head) = self.selection_offsets();
        head < anchor
    }

    fn cursor(&self) -> usize {
        self.selection_offsets().1
    }

    /// The node a layout item starting at `offset` belongs to: the last region
    /// containing it, as [`Self::position_for_offset`] with `After` finds. Item
    /// starts are character boundaries, and regions are in order.
    /// The node at `offset`, searching the regions forward from `hint`.
    fn node_after_from(&self, offset: usize, hint: &mut usize) -> Option<I> {
        let regions = self.regions.as_slice();
        *hint = partition_from(regions, *hint, |region| region.range().start <= offset);
        let found = hint
            .checked_sub(1)
            .map(|ix| &regions[ix])
            .filter(|region| region.range().contains_inclusive(offset))
            .map(|region| region.id().clone());
        #[cfg(test)]
        assert!(
            found
                == self
                    .position_for_offset(offset, Affinity::After)
                    .ok()
                    .map(DocumentPosition::into_node_id)
        );
        found
    }

    fn position_for_offset(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> Result<DocumentPosition<I>, PositionError> {
        if offset > self.text.len() {
            return Err(PositionError::SourceOffsetOutOfBounds {
                offset,
                source_len: self.text.len(),
            });
        }
        if self.text.clip_offset(offset, Bias::Left) != offset {
            return Err(PositionError::InvalidBoundary { offset });
        }
        let mut candidates = self
            .regions
            .as_slice()
            .iter()
            .filter(|region| region.range().contains_inclusive(offset));
        let region = if affinity == Affinity::Before {
            candidates.next()
        } else {
            candidates.next_back()
        }
        .ok_or(PositionError::NodeNotFound)?;
        Ok(DocumentPosition::new(
            region.id().clone(),
            offset - region.range().start,
            affinity,
        ))
    }

    fn resolve_position(&self, position: &DocumentPosition<I>) -> Result<usize, PositionError> {
        self.resolve_position_in(position, self.region_index(position.node_id()))
    }

    /// Resolves `position` in the region at `index`, the index of its node.
    fn resolve_position_in(
        &self,
        position: &DocumentPosition<I>,
        index: Option<usize>,
    ) -> Result<usize, PositionError> {
        let index = index.ok_or(PositionError::NodeNotFound)?;
        let range = self.regions.as_slice()[index].range();
        if position.offset() > range.len() {
            return Err(PositionError::NodeOffsetOutOfBounds {
                offset: position.offset(),
                node_len: range.len(),
            });
        }
        let offset = range.start + position.offset();
        if self.text.clip_offset(offset, Bias::Left) != offset {
            return Err(PositionError::InvalidBoundary { offset });
        }
        Ok(offset)
    }

    fn position_for_anchor(
        &self,
        anchor: DocumentAnchor,
    ) -> Result<DocumentPosition<I>, PositionError> {
        let record = self
            .anchors
            .anchors
            .get(&anchor)
            .expect("document anchor must be live");
        if let Some(node_id) = &record.node_id
            && let Some(index) = self.region_index(node_id)
        {
            let range = self.regions.as_slice()[index].range();
            if range.contains_inclusive(record.offset) {
                return Ok(DocumentPosition::new(
                    node_id.clone(),
                    record.offset - range.start,
                    record.affinity,
                ));
            }
        }
        self.position_for_offset(record.offset, record.affinity)
    }

    fn selection_anchor(
        &mut self,
        offset: usize,
        bias: AnchorBias,
        affinity: Affinity,
    ) -> DocumentAnchor {
        let node_id = self
            .position_for_offset(offset, affinity)
            .ok()
            .map(DocumentPosition::into_node_id);
        self.anchors
            .create_for_node(offset, bias, node_id, affinity)
    }

    fn replace_selection(&mut self, anchor_offset: usize, head_offset: usize) {
        if anchor_offset == head_offset {
            self.collapse_selection(head_offset);
            return;
        }
        self.release_selection(self.selection);
        let reversed = head_offset < anchor_offset;
        let anchor = self.selection_anchor(
            anchor_offset,
            AnchorBias::Left,
            if reversed {
                Affinity::Before
            } else {
                Affinity::After
            },
        );
        let head = self.selection_anchor(
            head_offset,
            AnchorBias::Right,
            if reversed {
                Affinity::After
            } else {
                Affinity::Before
            },
        );
        self.selection = DocumentSelection::new(anchor, head);
    }

    fn collapse_selection(&mut self, offset: usize) {
        self.release_selection(self.selection);
        let cursor = self.selection_anchor(offset, AnchorBias::Right, Affinity::After);
        self.selection = DocumentSelection::new(cursor, cursor);
    }

    fn replace_selection_positions(
        &mut self,
        anchor: DocumentPosition<I>,
        head: DocumentPosition<I>,
    ) -> Result<(), PositionError> {
        let anchor_offset = self.resolve_position(&anchor)?;
        let head_offset = self.resolve_position(&head)?;
        self.release_selection(self.selection);
        if anchor == head {
            let cursor = self.anchors.create_for_node(
                head_offset,
                AnchorBias::Right,
                Some(head.node_id().clone()),
                head.affinity(),
            );
            self.selection = DocumentSelection::new(cursor, cursor);
        } else {
            let anchor = self.anchors.create_for_node(
                anchor_offset,
                AnchorBias::Left,
                Some(anchor.node_id().clone()),
                anchor.affinity(),
            );
            let head = self.anchors.create_for_node(
                head_offset,
                AnchorBias::Right,
                Some(head.node_id().clone()),
                head.affinity(),
            );
            self.selection = DocumentSelection::new(anchor, head);
        }
        Ok(())
    }

    fn select_in_region(&mut self, index: usize, anchor: usize, head: usize) {
        let region = &self.regions.as_slice()[index];
        self.replace_selection_positions(
            DocumentPosition::new(region.id().clone(), anchor, Affinity::After),
            DocumentPosition::new(region.id().clone(), head, Affinity::After),
        )
        .expect("region-relative selection must resolve");
    }

    fn release_selection(&mut self, selection: DocumentSelection) {
        self.anchors.remove(selection.anchor());
        if selection.head() != selection.anchor() {
            self.anchors.remove(selection.head());
        }
    }

    fn set_marked_range(&mut self, range: Option<Range<usize>>) {
        if let Some(marked) = self.marked.take() {
            self.release_selection(marked);
        }
        self.marked = range.map(|range| {
            let node_id = self
                .position_for_anchor(self.selection.head())
                .ok()
                .map(DocumentPosition::into_node_id);
            let start = self.anchors.create_for_node(
                range.start,
                AnchorBias::Left,
                node_id.clone(),
                Affinity::After,
            );
            let end = self.anchors.create_for_node(
                range.end,
                AnchorBias::Right,
                node_id,
                Affinity::Before,
            );
            DocumentSelection::new(start, end)
        });
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.map(|marked| {
            let start = self
                .anchors
                .resolve(marked.anchor())
                .expect("marked range anchor must be live");
            let end = self
                .anchors
                .resolve(marked.head())
                .expect("marked range head must be live");
            start.min(end)..start.max(end)
        })
    }

    fn route_and_apply(
        &mut self,
        transaction: &EditTransaction,
        node_id: Option<&I>,
    ) -> RoutingOutcome<I> {
        if let Err(reason) = self.validate_transaction(transaction) {
            return RoutingOutcome::Rejected(reason);
        }

        let target = match self.target_region_for_transaction(transaction, node_id) {
            Ok(target) => target,
            Err(reason) => return RoutingOutcome::Rejected(reason),
        };
        let region = &self.regions.as_slice()[target];
        match region.policy() {
            EditPolicy::Readonly => RoutingOutcome::Rejected(DocumentEditRejection::Readonly),
            EditPolicy::Atomic => RoutingOutcome::Rejected(DocumentEditRejection::Atomic),
            EditPolicy::Routed => RoutingOutcome::Routed(region.id().clone()),
            EditPolicy::Editable => match self.apply_to_editable_region(target, transaction) {
                Ok(()) => RoutingOutcome::Applied,
                Err(reason) => RoutingOutcome::Rejected(reason),
            },
        }
    }

    fn validate_transaction(
        &self,
        transaction: &EditTransaction,
    ) -> Result<(), DocumentEditRejection> {
        if transaction.revision() != self.revision {
            return Err(DocumentEditRejection::StaleRevision);
        }
        if transaction
            .edits()
            .iter()
            .any(|edit| !valid_source_range(&self.text, &edit.range()))
        {
            return Err(DocumentEditRejection::InvalidBoundary);
        }
        Ok(())
    }

    fn target_region_for_transaction(
        &self,
        transaction: &EditTransaction,
        node_id: Option<&I>,
    ) -> Result<usize, DocumentEditRejection> {
        let mut target = None;
        for edit in transaction.edits() {
            let range = edit.range();
            let index = node_id
                .and_then(|id| self.region_index(id))
                .filter(|index| {
                    let region = self.regions.as_slice()[*index].range();
                    region.start <= range.start && range.end <= region.end
                });
            let Some(index) = index.or_else(|| {
                if node_id.is_some() {
                    None
                } else {
                    self.region_for_range(&range)
                }
            }) else {
                return Err(DocumentEditRejection::OutsideRegion);
            };
            if target.replace(index).is_some_and(|target| target != index) {
                return Err(DocumentEditRejection::CrossesRegions);
            }
        }
        Ok(target.expect("validated transactions contain at least one edit"))
    }

    fn region_for_range(&self, range: &Range<usize>) -> Option<usize> {
        if range.is_empty() {
            if range.start == self.cursor()
                && let Ok(position) = self.position_for_anchor(self.selection.head())
                && let Some(index) = self.region_index(position.node_id())
            {
                return Some(index);
            }
            return self
                .regions
                .as_slice()
                .iter()
                .enumerate()
                .rev()
                .find(|(_, region)| {
                    let region_range = region.range();
                    if region_range.is_empty() {
                        region_range.start == range.start
                    } else {
                        region_range.start <= range.start && range.start <= region_range.end
                    }
                })
                .map(|(index, _)| index);
        }

        self.regions
            .as_slice()
            .iter()
            .enumerate()
            .find(|(_, region)| {
                let region_range = region.range();
                region_range.start <= range.start && range.end <= region_range.end
            })
            .map(|(index, _)| index)
    }

    fn apply_to_editable_region(
        &mut self,
        target: usize,
        transaction: &EditTransaction,
    ) -> Result<(), DocumentEditRejection> {
        let edits = transaction.edits();
        let region = self.regions.as_slice()[target].range();
        let delta = edits
            .iter()
            .map(|edit| edit.replacement().len() as isize - edit.range().len() as isize)
            .sum::<isize>();
        let end = shift_offset(region.end, delta);
        let mut text = self.text.clone();
        for edit in edits {
            text.replace(edit.range(), edit.replacement());
        }
        #[cfg(debug_assertions)]
        let expected = self.edited_from_scratch(target, edits, &text);

        // No projection span, block or paragraph style overlaps an editable
        // region, and its text maps to display unchanged. Only its own text
        // and inline styles change; everything after it moves by `delta`.
        let old_text = self.text.slice(region.clone()).to_string();
        self.projection_map
            .replace_identity(region.clone(), &text.slice(region.start..end).to_string());
        // A paragraph next to the region must still cover whole display lines.
        let paragraphs = self.styles.paragraphs();
        let touched = paragraphs.partition_point(|style| style.source().end < region.start)
            ..paragraphs.partition_point(|style| style.source().start <= region.end);
        let resolved = paragraphs[touched.clone()]
            .iter()
            .map(|style| {
                let source = transform_source_range(style.source(), edits);
                resolve_paragraph_style(&self.projection_map, source, style)
            })
            .collect::<Result<Vec<_>, _>>();
        let Ok(resolved) = resolved else {
            self.projection_map
                .replace_identity(region.start..end, &old_text);
            #[cfg(debug_assertions)]
            assert!(
                expected.is_err(),
                "a local edit rejects what a full one accepts"
            );
            return Err(DocumentEditRejection::InvalidPresentation);
        };

        for edit in edits {
            self.anchors
                .apply_edit(&edit.range(), edit.replacement().len());
        }
        self.text = text;
        self.regions.resize(target, delta);
        self.projection.edit_region(region.clone(), edits, delta);
        let blocks = self
            .blocks
            .partition_point(|block| block.source().start < region.end);
        for block in &mut self.blocks[blocks..] {
            block.shift(delta);
        }
        let (_, inline) = self.styles.edit_region(region.clone(), edits, delta);
        let shift = |range: &mut Range<usize>| {
            *range = shift_offset(range.start, delta)..shift_offset(range.end, delta);
        };
        let paragraphs = &mut self.resolved_styles.paragraphs;
        for style in &mut paragraphs[touched.end..] {
            style.shift(delta, delta);
        }
        paragraphs.splice(touched, resolved);
        let resolved_inline = &mut self.resolved_styles.inline;
        let from = resolved_inline.partition_point(|style| style.source.end < region.start);
        let to = resolved_inline.partition_point(|style| style.source.start <= region.end);
        for style in &mut resolved_inline[to..] {
            shift(&mut style.source);
            shift(&mut style.display);
        }
        let mut replacement = Vec::new();
        for style in &self.styles.inline[inline] {
            resolve_inline_style(&self.projection_map, style, &mut replacement);
        }
        self.resolved_styles.inline.splice(from..to, replacement);
        self.reconcile_anchor_nodes_in(region.start..end);
        self.revision = self.revision.next();

        #[cfg(debug_assertions)]
        self.assert_edited_like(expected);
        Ok(())
    }

    /// The model after an edit inside editable region `target`, derived
    /// from the whole document, to check the local update against.
    #[cfg(debug_assertions)]
    fn edited_from_scratch(
        &self,
        target: usize,
        edits: &[TextEdit],
        text: &Rope,
    ) -> Result<Self, DocumentEditRejection> {
        let delta = text.len() as isize - self.text.len() as isize;
        let regions = self
            .regions
            .as_slice()
            .iter()
            .enumerate()
            .map(|(index, region)| {
                let mut range = region.range();
                if index == target {
                    range.end = shift_offset(range.end, delta);
                } else if index > target {
                    range.start = shift_offset(range.start, delta);
                    range.end = shift_offset(range.end, delta);
                }
                DocumentRegion::new(region.id().clone(), range, region.policy())
            })
            .collect();
        let regions = DocumentRegions::new(regions, text.len())
            .map_err(|_| DocumentEditRejection::InvalidRegions)?;
        let projection = self.projection.transformed(edits, text.len());
        let blocks = super::block::transform_blocks(&self.blocks, edits);
        validate_projection(&projection, &regions, text)
            .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        validate_blocks(&blocks, &regions, &projection, text.len())
            .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        let projection_map = ProjectionMap::new(text, &projection);
        let styles = self.styles.transformed(edits);
        let resolved_styles =
            resolve_document_styles(text, &regions, &projection_map, &styles, &blocks)
                .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        let mut model = self.clone();
        for edit in edits {
            model
                .anchors
                .apply_edit(&edit.range(), edit.replacement().len());
        }
        model.text = text.clone();
        model.projection = projection;
        model.projection_map = projection_map;
        model.blocks = blocks;
        model.styles = styles;
        model.resolved_styles = resolved_styles;
        model.regions = regions;
        model.reconcile_anchor_nodes();
        model.revision = model.revision.next();
        Ok(model)
    }

    #[cfg(debug_assertions)]
    fn assert_edited_like(&self, expected: Result<Self, DocumentEditRejection>) {
        let expected = expected.expect("a local edit accepts what a full one rejects");
        for (same, part) in [
            (self.text == expected.text, "text"),
            (self.regions == expected.regions, "regions"),
            (self.projection == expected.projection, "projection"),
            (
                self.projection_map == expected.projection_map,
                "projection map",
            ),
            (self.blocks == expected.blocks, "blocks"),
            (self.styles == expected.styles, "styles"),
            (
                self.resolved_styles == expected.resolved_styles,
                "resolved styles",
            ),
            (self.anchors == expected.anchors, "anchors"),
        ] {
            assert!(same, "a local edit differs from a full one in its {part}");
        }
    }

    fn reconcile_anchor_nodes(&mut self) {
        for record in self.anchors.anchors.values_mut() {
            if let Some(id) = &record.node_id {
                if let Some(region) = self
                    .regions
                    .as_slice()
                    .iter()
                    .find(|region| region.id() == id)
                {
                    let range = region.range();
                    record.offset = record.offset.clamp(range.start, range.end);
                } else {
                    record.node_id = None;
                }
            }
        }
    }

    /// `reconcile_anchor_nodes` after an edit inside `edited`, the edited
    /// region's new range: only anchors the edit moved onto it can have left
    /// their node.
    fn reconcile_anchor_nodes_in(&mut self, edited: Range<usize>) {
        let regions = &self.regions;
        for record in self.anchors.anchors.values_mut() {
            if !edited.contains_inclusive(record.offset) {
                continue;
            }
            let Some(id) = &record.node_id else {
                continue;
            };
            let start = regions
                .as_slice()
                .partition_point(|region| region.range().end < record.offset);
            let region = regions.as_slice()[start..]
                .iter()
                .take_while(|region| region.range().start <= record.offset)
                .find(|region| region.id() == id)
                .or_else(|| regions.as_slice().iter().find(|region| region.id() == id));
            match region {
                Some(region) => {
                    let range = region.range();
                    record.offset = record.offset.clamp(range.start, range.end);
                }
                None => record.node_id = None,
            }
        }
    }

    fn editable_range_for_cursor(&self) -> Option<Range<usize>> {
        let cursor = self.cursor();
        let index = self.region_for_range(&(cursor..cursor))?;
        let region = &self.regions.as_slice()[index];
        (region.policy() == EditPolicy::Editable).then(|| region.range())
    }

    fn region_index(&self, id: &I) -> Option<usize> {
        self.regions
            .as_slice()
            .iter()
            .position(|region| region.id() == id)
    }

    fn capture_selection_in_region(
        &self,
        selection: DocumentSelection,
        index: usize,
    ) -> Option<RelativeSelection> {
        let region = &self.regions.as_slice()[index];
        let range = region.range();
        let anchor = self.anchors.resolve(selection.anchor())?;
        let head = self.anchors.resolve(selection.head())?;
        if !range.contains_inclusive(anchor) || !range.contains_inclusive(head) {
            return None;
        }
        if self.position_for_anchor(selection.anchor()).ok()?.node_id() != region.id()
            || self.position_for_anchor(selection.head()).ok()?.node_id() != region.id()
        {
            return None;
        }
        Some(RelativeSelection {
            anchor: anchor - range.start,
            head: head - range.start,
        })
    }

    fn capture_active_selection_in_editable_region(&self) -> Option<RegionSelection<I>> {
        self.regions
            .as_slice()
            .iter()
            .enumerate()
            .filter(|(_, region)| region.policy() == EditPolicy::Editable)
            .find_map(|(index, region)| {
                self.capture_selection_in_region(self.selection, index)
                    .map(|selection| RegionSelection {
                        region_id: region.id().clone(),
                        selection,
                        affinities: (
                            self.position_for_anchor(self.selection.anchor())
                                .unwrap()
                                .affinity(),
                            self.position_for_anchor(self.selection.head())
                                .unwrap()
                                .affinity(),
                        ),
                    })
            })
    }

    fn capture_marked_in_editable_region(&self) -> Option<RegionSelection<I>> {
        let marked = self.marked?;
        self.regions
            .as_slice()
            .iter()
            .enumerate()
            .filter(|(_, region)| region.policy() == EditPolicy::Editable)
            .find_map(|(index, region)| {
                self.capture_selection_in_region(marked, index)
                    .map(|selection| RegionSelection {
                        region_id: region.id().clone(),
                        selection,
                        affinities: (
                            self.position_for_anchor(marked.anchor())
                                .unwrap()
                                .affinity(),
                            self.position_for_anchor(marked.head()).unwrap().affinity(),
                        ),
                    })
            })
    }

    fn restore_active_selection(&mut self, capture: &RegionSelection<I>) {
        if let Some(index) = self.region_index(&capture.region_id) {
            let range = self.regions.as_slice()[index].range();
            self.replace_selection_positions(
                DocumentPosition::new(
                    capture.region_id.clone(),
                    capture.selection.anchor.min(range.len()),
                    capture.affinities.0,
                ),
                DocumentPosition::new(
                    capture.region_id.clone(),
                    capture.selection.head.min(range.len()),
                    capture.affinities.1,
                ),
            )
            .expect("retained editable selection resolves after host update");
        }
    }

    fn restore_marked(&mut self, capture: Option<&RegionSelection<I>>) {
        let range = capture.and_then(|capture| {
            let index = self.region_index(&capture.region_id)?;
            let region = self.regions.as_slice()[index].range();
            Some(
                region.start + capture.selection.anchor.min(region.len())
                    ..region.start + capture.selection.head.min(region.len()),
            )
        });
        self.set_marked_range(range);
    }
}

/// Identifies a row's text for layout reuse. Deterministic within a process;
/// a collision could only keep a stale measurement, never wrong text. It is
/// computed for every line a document lays out, so it uses fixed-seed
/// foldhash, several times faster than SipHash on short lines.
fn text_fingerprint(text: &str) -> u64 {
    use std::hash::BuildHasher as _;
    foldhash::quality::FixedState::with_seed(0).hash_one(text)
}

fn resolve_document_styles<I: Eq>(
    text: &Rope,
    regions: &DocumentRegions<I>,
    projection: &ProjectionMap,
    styles: &DocumentStyles,
    blocks: &[DocumentBlock<I>],
) -> Result<ResolvedDocumentStyles, DocumentStyleError> {
    validate_style_ranges_with_objects(
        text,
        regions,
        styles.paragraphs().iter().map(|style| style.source()),
        false,
        blocks,
    )?;
    validate_style_ranges(
        text,
        regions,
        styles.inline().iter().map(|style| style.source()),
        true,
    )?;

    let paragraphs = styles
        .paragraphs()
        .iter()
        .map(|style| {
            let source = style.source();
            if let Some(row) = style.table_row() {
                let cells = row
                    .cells()
                    .iter()
                    .map(|(cell, _)| {
                        if cell.start > cell.end || cell.end > source.len() {
                            return Err(DocumentStyleError::ParagraphBoundary(source.clone()));
                        }
                        Ok(source.start + cell.start..source.start + cell.end)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                validate_style_ranges(text, regions, cells, false)?;
            }
            resolve_paragraph_style(projection, source, style)
        })
        .collect::<Result<_, _>>()?;
    let mut inline = Vec::new();
    for style in styles.inline() {
        resolve_inline_style(projection, style, &mut inline);
    }
    Ok(ResolvedDocumentStyles { paragraphs, inline })
}

/// `style` placed at `source`, which must cover whole display lines.
fn resolve_paragraph_style(
    projection: &ProjectionMap,
    source: Range<usize>,
    style: &DocumentParagraphStyle,
) -> Result<ResolvedParagraphStyle, DocumentStyleError> {
    let display_text = projection.display_text();
    let display = projection
        .source_to_display(source.start, Affinity::Before)
        .expect("validated style start must map")
        ..projection
            .source_to_display(source.end, Affinity::After)
            .expect("validated style end must map");
    let starts_line =
        display.start == 0 || display_text.as_bytes().get(display.start - 1) == Some(&b'\n');
    let ends_line = display.end == display_text.len()
        || display.end > 0 && display_text.as_bytes().get(display.end - 1) == Some(&b'\n');
    if !starts_line || !ends_line {
        return Err(DocumentStyleError::ParagraphBoundary(source));
    }
    let table_row = style
        .table_row()
        .map(|row| {
            // Source rows may include hidden Markdown delimiters. The projected
            // row, excluding its terminating newline, must remain one line.
            let content = display_text[display.clone()]
                .strip_suffix('\n')
                .unwrap_or(&display_text[display.clone()]);
            if content.contains('\n') {
                return Err(DocumentStyleError::ParagraphBoundary(source.clone()));
            }
            let content_end = display.start + content.len();
            let cells = row
                .cells()
                .iter()
                .map(|(cell, align)| {
                    if cell.start > cell.end || cell.end > source.len() {
                        return Err(DocumentStyleError::ParagraphBoundary(source.clone()));
                    }
                    let mut projected: Option<Range<usize>> = None;
                    projection.project_source_range(
                        source.start + cell.start..source.start + cell.end,
                        |range| {
                            projected = Some(match projected.take() {
                                Some(previous) => {
                                    previous.start.min(range.start)..previous.end.max(range.end)
                                }
                                None => range,
                            });
                        },
                    );
                    let range = projected.unwrap_or_else(|| {
                        let at = projection
                            .source_to_display(source.start + cell.start, Affinity::After)
                            .unwrap_or(display.start)
                            .clamp(display.start, content_end);
                        at..at
                    });
                    // Normalized cell whitespace can share the source span
                    // of the generated row newline; that newline is not cell text.
                    Ok((
                        range.start.clamp(display.start, content_end) - display.start
                            ..range.end.clamp(display.start, content_end) - display.start,
                        *align,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(super::TableRow::new(row.table()).with_cells(cells))
        })
        .transpose()?;
    Ok(ResolvedParagraphStyle {
        source,
        display,
        text_style: style.text_style().clone(),
        hanging_marker: style.hanging_marker().cloned(),
        table_row,
    })
}

/// `style` in display text, unless its text is hidden.
fn resolve_inline_style(
    projection: &ProjectionMap,
    style: &DocumentInlineStyle,
    resolved: &mut Vec<ResolvedInlineStyle>,
) {
    let source = style.source();
    projection.project_source_range(source.clone(), |display| {
        resolved.push(ResolvedInlineStyle {
            source: source.clone(),
            display,
            highlight: style.highlight(),
            font_family: style.font_family_override().cloned(),
            font_scale: style.font_scale_override(),
        });
    });
}

fn validate_style_ranges<I: Eq>(
    text: &Rope,
    regions: &DocumentRegions<I>,
    ranges: impl IntoIterator<Item = Range<usize>>,
    allow_editable: bool,
) -> Result<(), DocumentStyleError> {
    validate_style_ranges_with_objects(text, regions, ranges, allow_editable, &[])
}

fn validate_style_ranges_with_objects<I: Eq>(
    text: &Rope,
    regions: &DocumentRegions<I>,
    ranges: impl IntoIterator<Item = Range<usize>>,
    allow_editable: bool,
    inline_blocks: &[DocumentBlock<I>],
) -> Result<(), DocumentStyleError> {
    let mut ranges = ranges.into_iter().collect::<Vec<_>>();
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut previous_end = 0;
    for range in ranges {
        if range.end > text.len() || range.start > range.end {
            return Err(DocumentStyleError::OutOfBounds {
                range,
                source_len: text.len(),
            });
        }
        if text.clip_offset(range.start, Bias::Left) != range.start
            || text.clip_offset(range.end, Bias::Right) != range.end
        {
            return Err(DocumentStyleError::InvalidBoundary(range));
        }
        if range.start < previous_end {
            return Err(DocumentStyleError::Overlap(range));
        }
        // Canonical regions have monotonic starts and ends. These strict bounds
        // preserve the existing intersection predicate even for an empty style
        // inside a region, or an empty region inside a style.
        let regions = regions.as_slice();
        let from = regions.partition_point(|region| region.range().end <= range.start);
        for region in regions[from..]
            .iter()
            .take_while(|region| region.range().start < range.end)
        {
            let region_range = region.range();
            if range.start < region_range.end && region_range.start < range.end {
                match region.policy() {
                    EditPolicy::Editable if !allow_editable => {
                        return Err(DocumentStyleError::EditableRange(range));
                    }
                    EditPolicy::Atomic => {
                        let ix = inline_blocks
                            .partition_point(|block| block.source().end <= region_range.start);
                        if !inline_blocks.get(ix).is_some_and(|block| {
                            block.id() == region.id() && block.inline_size().is_some()
                        }) {
                            return Err(DocumentStyleError::AtomicRange(range));
                        }
                    }
                    EditPolicy::Editable | EditPolicy::Readonly | EditPolicy::Routed => {}
                }
            }
        }
        previous_end = range.end;
    }
    Ok(())
}

// Include the final caret as a real candidate. GPUI 0.3.5's closest-index
// helper returns EOF for every x after the last glyph start; rounding an
// absolute caret back into local coordinates can otherwise advance a column.
/// The caret boundary nearest `position` in `line`, which lays out `text`.
/// Glyphs inside a grapheme cluster (combining marks, ZWJ parts) are not
/// caret boundaries, so a click never splits a cluster.
fn closest_caret_index(
    line: &gpui::WrappedLineLayout,
    position: Point<Pixels>,
    line_height: Pixels,
    text: &str,
) -> usize {
    let is_boundary = |index: usize| {
        index >= text.len()
            || unicode_segmentation::GraphemeCursor::new(index, text.len(), true)
                .is_boundary(text, 0)
                .unwrap_or(true)
    };
    let row_ix =
        ((position.y / line_height).max(0.) as usize).min(line.rows().len().saturating_sub(1));
    let Some(row) = line.rows().get(row_ix) else {
        return 0;
    };
    let position = point(position.x - line.indent.for_row(row_ix), position.y);
    let range = row.range();
    let (start, end) = (range.start, range.end);
    let mut closest = start;
    let mut distance = position.x.abs();
    for glyph in row.layout().runs.iter().flat_map(|run| &run.glyphs) {
        let index = start + glyph.index;
        let delta = (glyph.position.x - position.x).abs();
        if delta < distance && is_boundary(index) {
            closest = index;
            distance = delta;
        }
    }
    if (row.layout().width - position.x).abs() < distance {
        end
    } else {
        closest
    }
}

fn valid_source_range(source: &Rope, range: &Range<usize>) -> bool {
    range.start <= range.end
        && range.end <= source.len()
        && source.clip_offset(range.start, Bias::Left) == range.start
        && source.clip_offset(range.end, Bias::Right) == range.end
}

fn validate_region_boundaries<I: Eq>(
    regions: &DocumentRegions<I>,
    source: &Rope,
) -> Result<(), RegionError> {
    for (index, region) in regions.as_slice().iter().enumerate() {
        if !valid_source_range(source, &region.range()) {
            return Err(RegionError::InvalidBoundary { index });
        }
    }
    Ok(())
}

fn validate_projection<I: Eq>(
    projection: &DocumentProjection,
    regions: &DocumentRegions<I>,
    source: &Rope,
) -> Result<(), ProjectionError> {
    if projection.source_len() != source.len() {
        return Err(ProjectionError::SourceLength {
            expected: source.len(),
            actual: projection.source_len(),
        });
    }
    for span in projection.spans() {
        let span_source = span.source();
        if !valid_source_range(source, &span_source) {
            return Err(ProjectionError::InvalidBoundary(span_source));
        }
        if let Some(mapping) = span.mapping()
            && mapping.iter().any(|entry| {
                let range = entry.source();
                source.clip_offset(range.start, Bias::Left) != range.start
                    || source.clip_offset(range.end, Bias::Right) != range.end
            })
        {
            return Err(ProjectionError::InvalidMapping(span_source));
        }
        // Regions are in source order with monotonic ends, so only those from
        // the first ending after the span's start can intersect it.
        let regions = regions.as_slice();
        let from = regions.partition_point(|region| region.range().end <= span_source.start);
        if regions[from..]
            .iter()
            .take_while(|region| region.range().start < span_source.end)
            .any(|region| {
                region.policy() == EditPolicy::Editable
                    && span_source.start < region.range().end
                    && region.range().start < span_source.end
            })
        {
            return Err(ProjectionError::EditableOverlap(span_source));
        }
    }
    Ok(())
}

trait RangeExt {
    fn contains_inclusive(&self, value: usize) -> bool;
}

impl RangeExt for Range<usize> {
    fn contains_inclusive(&self, value: usize) -> bool {
        self.start <= value && value <= self.end
    }
}

pub struct DocumentState<I> {
    model: DocumentModel<I>,
    layout_items: LayoutItems<I>,
    list_state: ListState,
    focus_handle: FocusHandle,
    blink_cursor: gpui::Entity<BlinkCursor>,
    // Independent of editability so returning from a selection can restart blinking.
    cursor_active: bool,
    _cursor_subscriptions: [gpui::Subscription; 2],
    cursor_focus_subscriptions: Option<[gpui::Subscription; 3]>,
    text_layouts: Vec<TextLayoutRecord>,
    tables: table::Tables<I>,
    block_layouts: Vec<BlockLayoutRecord>,
    trailer_layout: Option<(usize, Bounds<Pixels>)>,
    last_bounds: Option<Bounds<Pixels>>,
    layout_style: Option<(gpui::TextStyle, Pixels)>,
    auto_scroll: AutoScroll,
    /// Source range of the link under the pointer, and of the one pressed.
    hovered_link: Option<Range<usize>>,
    pressed_link: Option<Range<usize>>,
    undo: DocumentUndoManager<I>,
    preferred_x: Option<Pixels>,
    dynamic_trailer: bool,
    trailer_height: Option<Pixels>,
    viewport: viewport::Viewport<I>,
    scroll_handler_installed: bool,
    scrollbar_input_pending: Rc<Cell<bool>>,
    block_renderer: Option<BlockRenderer<I>>,
    text_renderer: Option<TextRenderer<I>>,
    text_frame_resolver: Option<TextFrameResolver<I>>,
    block_height_resolver: Option<BlockHeightResolver<I>>,
    /// Display rows, when the host asked for them.
    text_rows: Option<text_rows::TextRowsState<I>>,
}

struct TextLayoutRecord {
    item_ix: usize,
    display: Range<usize>,
    layout: inline_objects::LineLayout,
    bounds: Bounds<Pixels>,
}

struct BlockLayoutRecord {
    item_ix: usize,
    source: Range<usize>,
    bounds: Bounds<Pixels>,
}

type BlockRenderer<I> = Rc<dyn Fn(&I, &mut Window, &mut App) -> AnyElement>;
type TextFrameResolver<I> = Rc<dyn Fn(&DocumentTextContext<I>, &Window, &App) -> TextFrame>;
type BlockHeightResolver<I> = Rc<dyn Fn(&I, &Window, &App) -> Option<gpui::AbsoluteLength>>;
type TextRenderer<I> =
    Rc<dyn Fn(DocumentTextContext<I>, AnyElement, &mut Window, &mut App) -> AnyElement>;

// ListState's wheel callback does not run for scrollbar offset writes. Keep
// that input intent alongside the existing viewport, without a second offset.
#[derive(Clone)]
struct DocumentScrollbarHandle {
    list: ListState,
    input_pending: Rc<Cell<bool>>,
}

impl ScrollbarHandle for DocumentScrollbarHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.list.viewport_bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        ScrollbarHandle::offset(&self.list)
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.input_pending.set(true);
        ScrollbarHandle::set_offset(&self.list, offset);
    }

    fn content_size(&self) -> gpui::Size<Pixels> {
        ScrollbarHandle::content_size(&self.list)
    }

    fn start_drag(&self) {
        self.input_pending.set(true);
        self.list.scrollbar_drag_started();
    }

    fn end_drag(&self) {
        self.list.scrollbar_drag_ended();
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    pub fn new(
        text: impl Into<String>,
        regions: Vec<DocumentRegion<I>>,
        cx: &mut Context<Self>,
    ) -> Result<Self, RegionError> {
        let model = DocumentModel::new(text, regions)?;
        let layout_items = model.layout_items_tree(None);
        let list_state =
            ListState::new(layout_items.len(), ListAlignment::Top, px(400.)).measure_all();
        let focus_handle = cx.focus_handle();
        let blink_cursor = cx.new(|_| BlinkCursor::new());
        let weak = cx.weak_entity();
        let input_focus = focus_handle.clone();
        let cursor_subscriptions = [
            cx.observe(&blink_cursor, |_, _, cx| cx.notify()),
            cx.intercept_keystrokes(move |_, window, cx| {
                if input_focus.is_focused(window) && window.is_window_active() {
                    _ = weak.update(cx, |this, cx| this.pause_blink_cursor(cx));
                }
            }),
        ];
        Ok(Self {
            model,
            layout_items,
            list_state,
            focus_handle,
            blink_cursor,
            cursor_active: false,
            _cursor_subscriptions: cursor_subscriptions,
            cursor_focus_subscriptions: None,
            text_layouts: Vec::new(),
            tables: table::Tables::default(),
            block_layouts: Vec::new(),
            trailer_layout: None,
            last_bounds: None,
            layout_style: None,
            auto_scroll: AutoScroll::default(),
            hovered_link: None,
            pressed_link: None,
            undo: DocumentUndoManager::default(),
            preferred_x: None,
            dynamic_trailer: false,
            trailer_height: None,
            viewport: viewport::Viewport::default(),
            scroll_handler_installed: false,
            scrollbar_input_pending: Rc::default(),
            block_renderer: None,
            text_renderer: None,
            text_frame_resolver: None,
            block_height_resolver: None,
            text_rows: None,
        })
    }

    pub fn text(&self) -> String {
        self.model.text()
    }

    pub fn display_text(&self) -> &str {
        self.model.display_text()
    }

    pub fn set_projection(
        &mut self,
        projection: DocumentProjection,
        cx: &mut Context<Self>,
    ) -> Result<(), ProjectionError> {
        self.model.set_projection(projection)?;
        self.reconcile_presented_layout_items();
        cx.emit(DocumentEvent::ProjectionChanged);
        cx.notify();
        Ok(())
    }

    pub fn set_block_renderer(
        &mut self,
        renderer: impl Fn(&I, &mut Window, &mut App) -> AnyElement + 'static,
        cx: &mut Context<Self>,
    ) {
        self.block_renderer = Some(Rc::new(renderer));
        self.list_state.remeasure_items(0..self.layout_items.len());
        cx.notify();
    }

    pub fn set_text_renderer(
        &mut self,
        renderer: impl Fn(DocumentTextContext<I>, AnyElement, &mut Window, &mut App) -> AnyElement
        + 'static,
        cx: &mut Context<Self>,
    ) {
        self.text_renderer = Some(Rc::new(renderer));
        self.list_state.remeasure_items(0..self.layout_items.len());
        cx.notify();
    }

    pub fn reset(
        &mut self,
        snapshot: DocumentSnapshot<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), PresentationError> {
        self.validate_inline_layout(&snapshot.blocks)?;
        if self.block_renderer.is_none() {
            if !snapshot.blocks.is_empty() {
                return Err(PresentationError::Blocks(BlockError::MissingRenderer));
            }
            if !snapshot.anchored_blocks.is_empty() {
                return Err(PresentationError::AnchoredBlocks(
                    AnchoredBlockError::MissingRenderer,
                ));
            }
        }
        let mut model = DocumentModel::new(snapshot.text, snapshot.regions)
            .map_err(PresentationError::Regions)?;
        model.set_rich_presentation_with_anchors(
            snapshot.projection,
            snapshot.blocks,
            snapshot.anchored_blocks,
            snapshot.styles,
        )?;
        if let Some((anchor, head)) = snapshot.selection {
            model
                .replace_selection_positions(anchor, head)
                .map_err(PresentationError::Position)?;
        }
        model.revision = self.model.revision.next();
        self.model = model;
        self.auto_scroll.stop();
        self.undo = DocumentUndoManager::default();
        self.preferred_x = None;
        self.reset_viewport();
        self.scrollbar_input_pending.set(false);
        self.text_layouts.clear();
        self.block_layouts.clear();
        self.trailer_layout = None;
        let mut items = self
            .model
            .layout_items_tree(self.text_rows.as_ref().map(|rows| &rows.cache));
        if self.dynamic_trailer {
            items.extend([DocumentLayoutItem::Trailer]);
        }
        let retired = std::mem::replace(&mut self.layout_items, items);
        self.retire_layout_items(retired);
        self.list_state.reset(self.layout_items.len());
        self.layout_items_changed();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
        Ok(())
    }

    pub fn set_presentation(
        &mut self,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        cx: &mut Context<Self>,
    ) -> Result<(), PresentationError> {
        self.validate_inline_layout(&blocks)?;
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(PresentationError::Blocks(BlockError::MissingRenderer));
        }
        self.model.set_presentation(projection, blocks)?;
        self.reconcile_presented_layout_items();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }

    pub fn set_rich_presentation(
        &mut self,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        styles: DocumentStyles,
        cx: &mut Context<Self>,
    ) -> Result<(), PresentationError> {
        self.validate_inline_layout(&blocks)?;
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(PresentationError::Blocks(BlockError::MissingRenderer));
        }
        self.model
            .set_rich_presentation(projection, blocks, styles)?;
        self.reconcile_presented_layout_items();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }

    pub fn set_rich_presentation_with_anchors(
        &mut self,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        anchored_blocks: Vec<AnchoredBlock<I>>,
        styles: DocumentStyles,
        cx: &mut Context<Self>,
    ) -> Result<(), PresentationError> {
        self.validate_inline_layout(&blocks)?;
        if self.block_renderer.is_none() {
            if !blocks.is_empty() {
                return Err(PresentationError::Blocks(BlockError::MissingRenderer));
            }
            if !anchored_blocks.is_empty() {
                return Err(PresentationError::AnchoredBlocks(
                    AnchoredBlockError::MissingRenderer,
                ));
            }
        }
        self.model.set_rich_presentation_with_anchors(
            projection,
            blocks,
            anchored_blocks,
            styles,
        )?;
        self.reconcile_presented_layout_items();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }

    pub fn revision(&self) -> DocumentRevision {
        self.model.revision
    }

    pub fn regions(&self) -> &[DocumentRegion<I>] {
        self.model.regions.as_slice()
    }

    /// Mark the first undo record belonging to the next editable draft.
    /// Retire older records only after the submitted version is accepted.
    pub fn undo_checkpoint(&mut self) -> UndoCheckpoint {
        self.undo.checkpoint()
    }

    pub fn retire_undo_before(&mut self, checkpoint: UndoCheckpoint) {
        self.undo.retire_before(checkpoint);
    }

    /// Change only edit permissions without replacing selection, anchors or
    /// the user's undo history. A disabled draft cannot replay undo until it
    /// becomes editable again.
    pub fn set_region_policies(
        &mut self,
        regions: Vec<DocumentRegion<I>>,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let regions = DocumentRegions::new(regions, self.model.text.len())
            .map_err(|error| StructureError::Presentation(PresentationError::Regions(error)))?;
        if !self
            .model
            .regions
            .as_slice()
            .iter()
            .zip(regions.as_slice())
            .all(|(old, new)| {
                old.id() == new.id()
                    && old.range() == new.range()
                    && (old.policy() == new.policy()
                        || matches!(
                            (old.policy(), new.policy()),
                            (EditPolicy::Editable, EditPolicy::Readonly)
                                | (EditPolicy::Readonly, EditPolicy::Editable)
                        ))
            })
            || regions.as_slice().len() != self.model.regions.as_slice().len()
        {
            return Err(StructureError::PrefixChanged);
        }
        // An empty editable node keeps a line of its own for the caret.
        let empty_line_changed = self
            .model
            .regions
            .as_slice()
            .iter()
            .zip(regions.as_slice())
            .any(|(old, new)| old.range().is_empty() && old.policy() != new.policy());
        self.model.regions = regions;
        if empty_line_changed {
            self.reconcile_presented_layout_items();
        }
        self.undo.break_coalescing();
        cx.emit(DocumentEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }

    pub fn selected_range(&self) -> Range<usize> {
        self.model.selected_range()
    }

    pub fn region(&self, id: &I) -> Option<&DocumentRegion<I>> {
        self.model
            .region_index(id)
            .map(|index| &self.model.regions.as_slice()[index])
    }

    /// The region `id`, found from a source `offset` it holds without scanning
    /// every region. Regions are in source order.
    pub(super) fn region_at(&self, id: &I, offset: usize) -> Option<&DocumentRegion<I>> {
        let regions = self.model.regions.as_slice();
        let from = regions.partition_point(|region| region.range().end < offset);
        regions[from..]
            .iter()
            .take_while(|region| region.range().start <= offset)
            .find(|region| region.id() == id)
            .or_else(|| self.region(id))
    }

    pub fn region_text(&self, id: &I) -> Option<String> {
        let range = self.region(id)?.range();
        Some(self.model.text.slice(range).to_string())
    }

    pub fn selected_positions(
        &self,
    ) -> Result<(DocumentPosition<I>, DocumentPosition<I>), PositionError> {
        Ok((
            self.model
                .position_for_anchor(self.model.selection.anchor())?,
            self.model
                .position_for_anchor(self.model.selection.head())?,
        ))
    }

    /// Where a caret at `position` was painted in the last frame, in window
    /// coordinates, whether or not it is in view.
    pub fn position_bounds(&self, position: &DocumentPosition<I>) -> Option<Bounds<Pixels>> {
        self.resolve_position(position).ok()?;
        let (origin, line_height, _, _) = self.text_position_for_position(position)?;
        Some(Bounds::new(origin, gpui::size(px(1.), line_height)))
    }

    /// Where the IME composition in progress starts: the text before it is
    /// committed, the marked text after it is not.
    pub fn composition_start(&self) -> Option<DocumentPosition<I>> {
        let marked = self.model.marked?;
        self.model.position_for_anchor(marked.anchor()).ok()
    }

    /// The window rem size used by the last layout.
    pub fn rem_size(&self) -> Option<Pixels> {
        self.layout_style.as_ref().map(|(_, rem_size)| *rem_size)
    }

    pub fn list_state(&self) -> ListState {
        self.list_state.clone()
    }

    /// Visible selection-head bounds in window coordinates, from the current
    /// text layout. Read after this document's prepaint for same-frame popup
    /// placement. Returns `None` when the caret has no visible text geometry.
    pub fn caret_bounds(&self) -> Option<Bounds<Pixels>> {
        let (_, head) = self.selected_positions().ok()?;
        let bounds = self.position_bounds(&head)?;
        let visible = bounds.intersect(&self.list_state.viewport_bounds());
        (visible.size.width > Pixels::ZERO && visible.size.height > Pixels::ZERO).then_some(bounds)
    }

    pub fn set_dynamic_trailer(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.dynamic_trailer == enabled {
            return;
        }
        self.dynamic_trailer = enabled;
        if !enabled {
            self.trailer_height = None;
            self.trailer_layout = None;
        }
        self.reconcile_presented_layout_items();
        cx.notify();
    }

    pub fn has_dynamic_trailer(&self) -> bool {
        self.dynamic_trailer
    }

    #[cfg(test)]
    fn layout_bottom_for_position(&self, position: &DocumentPosition<I>) -> Option<Pixels> {
        if let Some((point, line_height, _, _)) = self.text_position_for_position(position) {
            return Some(point.y + line_height);
        }
        let (_, point) = self.layout_point_for_position(position)?;
        point.map(|point| point.y)
    }

    pub fn remeasure_block(&mut self, id: &I, cx: &mut Context<Self>) -> bool {
        self.remeasure_node(id, cx)
    }

    /// Invalidates measurements after the host changes a node's custom renderer geometry.
    /// With text rows, layout resolves the node's frames and block heights again.
    pub fn remeasure_node(&mut self, id: &I, cx: &mut Context<Self>) -> bool {
        if self.text_rows.is_some() {
            let cleared = self.clear_row_geometry(|node| node == Some(id));
            if cleared {
                cx.notify();
            }
            return cleared;
        }
        let indices: Vec<_> = self
            .layout_items
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| {
                let owner = match item {
                    DocumentLayoutItem::Text { node_id, .. } => node_id.as_ref(),
                    DocumentLayoutItem::Block { id, .. }
                    | DocumentLayoutItem::AnchoredBlock { id, .. } => Some(id),
                    DocumentLayoutItem::Trailer => None,
                };
                (owner == Some(id)).then_some(ix)
            })
            .collect();
        if indices.is_empty() {
            return false;
        }
        for ix in indices {
            self.list_state.remeasure_items(ix..ix + 1);
        }
        cx.notify();
        true
    }

    /// The first item that ends at or after `source`. Items end in document order.
    fn layout_item_for_source(&self, source: usize) -> usize {
        let found = self
            .layout_items
            .partition_point(|item| item.source_end() < source);
        #[cfg(test)]
        assert_eq!(
            found,
            self.layout_items
                .iter()
                .position(|item| source <= item.source_end())
                .unwrap_or(self.layout_items.len())
        );
        found
    }

    /// Items that may contain source `offset`: those starting at or before it,
    /// back to the first that ends before it. Sources follow document order, so a
    /// lookup costs the items around the offset, not the whole document.
    fn items_around_source(&self, offset: usize) -> Range<usize> {
        let end = self
            .layout_items
            .partition_point(|item| item.source_start() <= offset);
        let mut start = end;
        while start > 0 {
            match &self.layout_items[start - 1] {
                DocumentLayoutItem::Text { source, .. }
                | DocumentLayoutItem::Block { source, .. }
                    if source.end < offset =>
                {
                    break;
                }
                _ => start -= 1,
            }
        }
        start..end
    }

    /// Items that may contain display `offset`, as [`Self::items_around_source`]
    /// does for source offsets. Blocks sit at the display offset of their source.
    fn items_around_display(&self, offset: usize) -> Range<usize> {
        let display_start = |item: &DocumentLayoutItem<I>| match item {
            DocumentLayoutItem::Text { display, .. } => display.start,
            DocumentLayoutItem::Block { source, .. } => self
                .model
                .source_to_display(source.start, Affinity::Before)
                .unwrap_or_default(),
            DocumentLayoutItem::AnchoredBlock { source, .. } => self
                .model
                .source_to_display(*source, Affinity::Before)
                .unwrap_or_default(),
            DocumentLayoutItem::Trailer => usize::MAX,
        };
        let end = self
            .layout_items
            .partition_point(|item| display_start(item) <= offset);
        let mut start = end;
        while start > 0 {
            match &self.layout_items[start - 1] {
                DocumentLayoutItem::Text { display, .. } if display.end < offset => break,
                _ => start -= 1,
            }
        }
        start..end
    }

    /// Presentation changes keep the source, so every unchanged item before
    /// the first difference keeps its list slot. Splicing from item 0 would
    /// move the list's scroll top to the document start.
    fn reconcile_presented_layout_items(&mut self) {
        self.reconcile_layout_items_from(usize::MAX);
    }

    /// `first_changed_item` bounds the reused prefix: source edits pass the
    /// first item they touched, since earlier text may compare equal by accident.
    fn reconcile_layout_items_from(&mut self, first_changed_item: usize) {
        let next = self.next_layout_items();
        let mut prefix = 0;
        while prefix < first_changed_item
            && prefix < self.layout_items.len()
            && prefix < next.len()
            && self.layout_items[prefix].same_row(&next[prefix])
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < self.layout_items.len().saturating_sub(prefix)
            && suffix < next.len().saturating_sub(prefix)
            && self.layout_items[self.layout_items.len() - 1 - suffix]
                .same_row(&next[next.len() - 1 - suffix])
        {
            suffix += 1;
        }
        let old_end = self.layout_items.len() - suffix;
        let replacement_count = next.len() - prefix - suffix;
        if prefix != old_end || replacement_count != 0 {
            self.splice_list(|list| list.splice(prefix..old_end, replacement_count));
            // GPUI 0.3.5 splice does not re-arm measure_all. Keep retained
            // measurements, but measure every inserted/replaced item before
            // publishing the next scroll extent, including offscreen items.
            self.list_state.clone().measure_all();
            self.layout_items_changed();
        }
        let mut next = next;
        // Retained items keep the geometry layout derived for them.
        let split = next.len() - suffix;
        let (head, tail) = next.split_at_mut(split);
        // Rows with a new owner keep their slot; their frames are resolved again.
        let mut reowned = Vec::new();
        let kept = self
            .layout_items
            .range(0..prefix)
            .zip(head.iter_mut())
            .zip(0..)
            .chain(
                self.layout_items
                    .range(old_end..self.layout_items.len())
                    .zip(tail.iter_mut())
                    .zip(split..),
            );
        for ((old, new), ix) in kept {
            new.keep_geometry(old);
            if old.node() != new.node() {
                reowned.push(ix);
            }
        }
        host::forget_touched_geometry(
            &self
                .layout_items
                .range(prefix..old_end)
                .cloned()
                .collect::<Vec<_>>(),
            &mut next,
            prefix..split,
        );
        let retired = std::mem::replace(&mut self.layout_items, next.into());
        self.retire_layout_items(retired);
        self.reframe_rows(&reowned);
    }

    fn next_layout_items(&self) -> Vec<DocumentLayoutItem<I>> {
        let mut items = self
            .model
            .layout_items(self.text_rows.as_ref().map(|rows| &rows.cache));
        if self.dynamic_trailer {
            items.push(DocumentLayoutItem::Trailer);
        }
        items
    }

    pub fn position_for_offset(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> Result<DocumentPosition<I>, PositionError> {
        self.inline_boundary_position(offset, affinity)
            .map_or_else(|| self.model.position_for_offset(offset, affinity), Ok)
    }

    pub fn resolve_position(&self, position: &DocumentPosition<I>) -> Result<usize, PositionError> {
        self.model.resolve_position(position)
    }

    /// Selects node-relative positions without losing ownership at a shared boundary.
    pub fn set_selection_positions(
        &mut self,
        anchor: DocumentPosition<I>,
        head: DocumentPosition<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), PositionError> {
        self.model.replace_selection_positions(anchor, head)?;
        self.undo.break_coalescing();
        self.preferred_x = None;
        self.auto_scroll.stop();
        self.pause_blink_cursor(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
        Ok(())
    }

    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool, cx: &mut Context<Self>) {
        self.undo.break_coalescing();
        self.preferred_x = None;
        self.auto_scroll.stop();
        let start = self.model.text.clip_offset(range.start, Bias::Left);
        let end = self.model.text.clip_offset(range.end, Bias::Right);
        let start_position = self.inline_boundary_position(start, Affinity::After);
        let end_position = self.inline_boundary_position(end, Affinity::Before);
        if (start_position.is_some() || end_position.is_some())
            && let (Ok(start), Ok(end)) = (
                start_position.map_or_else(
                    || self.model.position_for_offset(start, Affinity::After),
                    Ok,
                ),
                end_position
                    .map_or_else(|| self.model.position_for_offset(end, Affinity::Before), Ok),
            )
        {
            let (anchor, head) = if reversed { (end, start) } else { (start, end) };
            self.model
                .replace_selection_positions(anchor, head)
                .expect("inline boundary positions resolve");
        } else if reversed {
            self.model.replace_selection(end, start);
        } else {
            self.model.replace_selection(start, end);
        }
        self.pause_blink_cursor(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    pub fn replace_selection(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    /// Commit a User, Paste or Composition edit, including undo and selection.
    /// Composition commits end the marked range; platform refinements use the
    /// input handler. Host changes must use the validating host transaction API.
    /// Rejected and routed edits leave all retained input state unchanged.
    pub fn apply_transaction(
        &mut self,
        transaction: EditTransaction,
        cx: &mut Context<Self>,
    ) -> EditDecision<I> {
        self.commit_user_transaction(transaction, None, cx)
    }

    /// Replace a consumer-owned suffix as one user edit. Its inverse stays in
    /// the same stack as text edits; the consumer supplies current presentation
    /// when replay is requested, so host-owned history is never snapshotted.
    pub fn apply_structure_transaction(
        &mut self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let selection_before = self.selected_positions().ok();
        let (next, before, after, selection_after) =
            self.prepare_structure(snapshot, start, None, None)?;
        self.undo.record(
            UndoRecord {
                sequence: 0,
                kind: UndoKind::Structure { before, after },
                selection_before,
                selection_after: selection_after.clone(),
            },
            EditOrigin::User,
        );
        self.commit_structure(next, start, selection_after, EditOrigin::User, cx);
        Ok(())
    }

    /// Complete the top pending structure replay only after its full next
    /// presentation has been validated. Failure leaves both stacks unchanged.
    pub fn apply_structure_replay(
        &mut self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        undo: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), StructureError> {
        let record = if undo {
            self.undo.undo.last()
        } else {
            self.undo.redo.last()
        };
        let Some(UndoRecord {
            kind: UndoKind::Structure { before, after },
            ..
        }) = record
        else {
            return Err(StructureError::StaleReplay);
        };
        let (current, target) = if undo {
            (after, before)
        } else {
            (before, after)
        };
        let (next, _, _, selection) =
            self.prepare_structure(snapshot, start, Some(current), Some(target))?;
        let (source_stack, target_stack, origin) = if undo {
            (&mut self.undo.undo, &mut self.undo.redo, EditOrigin::Undo)
        } else {
            (&mut self.undo.redo, &mut self.undo.undo, EditOrigin::Redo)
        };
        let record = source_stack
            .pop()
            .expect("validated replay record is present");
        target_stack.push(record);
        self.undo.break_coalescing();
        self.commit_structure(next, start, selection, origin, cx);
        Ok(())
    }

    /// IDs referenced by reversible suffix edits, for consumer-owned object
    /// snapshots which should be discarded when undo records are evicted.
    pub fn retained_structure_ids(&self) -> Vec<I> {
        self.undo
            .undo
            .iter()
            .chain(&self.undo.redo)
            .filter_map(|record| match &record.kind {
                UndoKind::Structure { before, after } => {
                    Some(before.regions.iter().chain(&after.regions))
                }
                UndoKind::Text { .. } => None,
            })
            .flatten()
            .map(|region| region.id().clone())
            .collect()
    }

    fn prepare_structure(
        &self,
        snapshot: DocumentSnapshot<I>,
        start: usize,
        expected_current: Option<&DocumentStructure<I>>,
        expected_target: Option<&DocumentStructure<I>>,
    ) -> Result<
        (
            DocumentModel<I>,
            DocumentStructure<I>,
            DocumentStructure<I>,
            Option<(DocumentPosition<I>, DocumentPosition<I>)>,
        ),
        StructureError,
    > {
        if self.model.marked.is_some() {
            return Err(StructureError::MarkedText);
        }
        self.validate_inline_layout(&snapshot.blocks)
            .map_err(StructureError::Presentation)?;
        let before =
            DocumentStructure::suffix(&self.model, start).ok_or(StructureError::InvalidBoundary)?;
        if expected_current.is_some_and(|expected| expected != &before) {
            return Err(StructureError::StaleReplay);
        }
        let DocumentSnapshot {
            text,
            regions,
            projection,
            blocks,
            anchored_blocks,
            styles,
            selection,
        } = snapshot;
        if text
            .get(..start)
            .is_none_or(|prefix| self.model.text.slice(..start) != prefix)
        {
            return Err(StructureError::PrefixChanged);
        }
        let mut next = DocumentModel::new(text, regions)
            .map_err(|error| StructureError::Presentation(PresentationError::Regions(error)))?;
        if self
            .model
            .regions
            .as_slice()
            .iter()
            .filter(|region| region.range().start < start)
            .ne(next
                .regions
                .as_slice()
                .iter()
                .filter(|region| region.range().start < start))
        {
            return Err(StructureError::PrefixChanged);
        }
        next.set_rich_presentation_with_anchors(projection, blocks, anchored_blocks, styles)
            .map_err(StructureError::Presentation)?;
        let after =
            DocumentStructure::suffix(&next, start).ok_or(StructureError::InvalidBoundary)?;
        if expected_target.is_some_and(|expected| expected != &after) {
            return Err(StructureError::StaleReplay);
        }
        let selection = selection.or_else(|| self.selected_positions().ok());
        if let Some((anchor, head)) = &selection {
            for position in [anchor, head] {
                next.resolve_position(position).map_err(|error| {
                    StructureError::Presentation(PresentationError::Position(error))
                })?;
            }
        }
        next.revision = self.model.revision.next();
        Ok((next, before, after, selection))
    }

    fn commit_structure(
        &mut self,
        mut next: DocumentModel<I>,
        start: usize,
        selection: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
        origin: EditOrigin,
        cx: &mut Context<Self>,
    ) {
        self.settle_list_scroll();
        let first_item = self.layout_item_for_source(start);
        next.anchors = self.model.anchors.clone();
        next.selection = self.model.selection;
        next.anchors
            .apply_edit(&(start..self.model.text.len()), next.text.len() - start);
        next.reconcile_anchor_nodes();
        if let Some((anchor, head)) = selection {
            next.replace_selection_positions(anchor, head)
                .expect("validated structure selection resolves");
        }
        self.model = next;
        self.reconcile_layout_items_from(first_item);
        self.finish_user_edit(origin, cx);
    }

    fn commit_user_transaction(
        &mut self,
        transaction: EditTransaction,
        composition_selection: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> EditDecision<I> {
        self.settle_list_scroll();
        let origin = transaction.origin();
        if !matches!(
            origin,
            EditOrigin::User | EditOrigin::Paste | EditOrigin::Composition
        ) {
            cx.emit(DocumentEvent::Rejected(
                DocumentEditRejection::InvalidOrigin,
            ));
            return EditDecision::Reject;
        }
        if let Err(reason) = self.model.validate_transaction(&transaction) {
            cx.emit(DocumentEvent::Rejected(reason));
            return EditDecision::Reject;
        }
        if let Some(request) = self.inline_replacement(&transaction) {
            cx.emit(DocumentEvent::ReplaceRequested(request));
            return EditDecision::StructureRequested;
        }
        let target = self
            .model
            .target_region_for_transaction(&transaction, None)
            .ok();
        let undo_seed = target
            .filter(|index| self.model.regions.as_slice()[*index].policy() == EditPolicy::Editable)
            .map(|index| {
                (
                    self.model.regions.as_slice()[index].id().clone(),
                    transaction
                        .edits()
                        .iter()
                        .map(|edit| {
                            let range = edit.range();
                            let start = self.model.regions.as_slice()[index].range().start;
                            UndoChange {
                                removed: self.model.text.slice(range.clone()).to_string(),
                                inserted: edit.replacement().to_owned(),
                                range: range.start - start..range.end - start,
                            }
                        })
                        .collect(),
                    self.selected_positions().ok(),
                )
            });
        let first = &transaction.edits()[0];
        let mut caret = first.range().start + first.replacement().len();
        for edit in &transaction.edits()[1..] {
            caret = transform_offset(
                caret,
                AnchorBias::Right,
                &edit.range(),
                edit.replacement().len(),
            );
        }
        let composition = composition_selection.map(|selected| {
            debug_assert_eq!(transaction.edits().len(), 1);
            let start = first.range().start;
            let replacement = Rope::from(first.replacement());
            (
                start..start + replacement.len(),
                start + replacement.offset_utf16_to_offset(selected.start)
                    ..start + replacement.offset_utf16_to_offset(selected.end),
            )
        });
        let first_changed_item = transaction
            .edits()
            .iter()
            .map(|edit| self.layout_item_for_source(edit.range().start))
            .min()
            .unwrap_or(0);
        // Only the edited node and its line neighbors lay out again.
        let next_was_seam = target.is_some_and(|index| self.model.is_layout_seam(index + 1));
        let lengths = (self.model.text.len(), self.model.display_text().len());
        let wrap_edit = self.text_row_edit(&transaction);
        match self.model.route_and_apply(&transaction, None) {
            RoutingOutcome::Applied => {
                self.relayout_edited_node(
                    target.expect("applied edits have a target region"),
                    next_was_seam,
                    self.model.text.len() as isize - lengths.0 as isize,
                    self.model.display_text().len() as isize - lengths.1 as isize,
                    first_changed_item,
                );
                self.set_text_row_edit(wrap_edit);
            }
            RoutingOutcome::Routed(region_id) => {
                cx.emit(DocumentEvent::Routed(RoutedEdit {
                    region_id: region_id.clone(),
                    transaction,
                }));
                return EditDecision::Route(region_id);
            }
            RoutingOutcome::Rejected(reason) => {
                cx.emit(DocumentEvent::Rejected(reason));
                return EditDecision::Reject;
            }
        }

        let index = target.expect("applied edits have a target region");
        let start = self.model.regions.as_slice()[index].range().start;
        let selected = composition
            .as_ref()
            .map_or(caret..caret, |(_, selected)| selected.clone());
        self.model
            .select_in_region(index, selected.start - start, selected.end - start);
        self.model.set_marked_range(
            composition
                .as_ref()
                .map(|(marked, _)| marked.clone())
                .filter(|marked| !marked.is_empty()),
        );
        if let Some((region_id, changes, selection_before)) = undo_seed {
            self.undo.record(
                UndoRecord {
                    sequence: 0,
                    kind: UndoKind::Text { region_id, changes },
                    selection_before,
                    selection_after: self.selected_positions().ok(),
                },
                origin,
            );
        }
        if self.model.marked_range().is_none() {
            self.undo.finish_composition();
        }
        self.finish_user_edit(origin, cx);
        EditDecision::Apply
    }

    fn finish_user_edit(&mut self, origin: EditOrigin, cx: &mut Context<Self>) {
        self.preferred_x = None;
        self.reveal_caret(cx);
        cx.emit(DocumentEvent::Changed {
            revision: self.model.revision,
            origin,
        });
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    pub fn apply_host_transaction(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        self.apply_host_transaction_inner(transaction, regions, None, None, None, None, cx)
    }

    pub fn apply_host_transaction_with_projection(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        self.apply_host_transaction_inner(
            transaction,
            regions,
            Some(projection),
            None,
            None,
            None,
            cx,
        )
    }

    pub fn apply_host_transaction_with_presentation(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(HostTransactionError::InvalidBlocks(
                BlockError::MissingRenderer,
            ));
        }
        self.apply_host_transaction_inner(
            transaction,
            regions,
            Some(projection),
            Some(blocks),
            None,
            None,
            cx,
        )
    }

    pub fn apply_host_transaction_with_rich_presentation(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        styles: DocumentStyles,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(HostTransactionError::InvalidBlocks(
                BlockError::MissingRenderer,
            ));
        }
        self.apply_host_transaction_inner(
            transaction,
            regions,
            Some(projection),
            Some(blocks),
            None,
            Some(styles),
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_host_transaction_with_anchored_blocks(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        blocks: Vec<DocumentBlock<I>>,
        anchored_blocks: Vec<AnchoredBlock<I>>,
        styles: DocumentStyles,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        if self.block_renderer.is_none() {
            if !blocks.is_empty() {
                return Err(HostTransactionError::InvalidBlocks(
                    BlockError::MissingRenderer,
                ));
            }
            if !anchored_blocks.is_empty() {
                return Err(HostTransactionError::InvalidAnchoredBlocks(
                    AnchoredBlockError::MissingRenderer,
                ));
            }
        }
        self.apply_host_transaction_inner(
            transaction,
            regions,
            Some(projection),
            Some(blocks),
            Some(anchored_blocks),
            Some(styles),
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_host_transaction_inner(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: Option<DocumentProjection>,
        blocks: Option<Vec<DocumentBlock<I>>>,
        anchored_blocks: Option<Vec<AnchoredBlock<I>>>,
        styles: Option<DocumentStyles>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        self.settle_list_scroll();
        if transaction.origin() != EditOrigin::Host {
            return Err(HostTransactionError::InvalidOrigin);
        }
        if transaction.revision() != self.model.revision {
            return Err(HostTransactionError::StaleRevision);
        }
        for edit in transaction.edits() {
            if !valid_source_range(&self.model.text, &edit.range()) {
                return Err(HostTransactionError::InvalidEditRange(edit.range()));
            }
        }

        let editable_regions: Vec<_> = self
            .model
            .regions
            .as_slice()
            .iter()
            .filter(|region| region.policy() == EditPolicy::Editable)
            .map(|region| {
                (
                    region.id().clone(),
                    region.range(),
                    self.model.text.slice(region.range()).to_string(),
                )
            })
            .collect();
        if transaction.edits().iter().any(|edit| {
            let edit = edit.range();
            editable_regions.iter().any(|(_, editable, _)| {
                if edit.is_empty() {
                    editable.start < edit.start && edit.start < editable.end
                } else {
                    edit.start < editable.end && editable.start < edit.end
                }
            })
        }) {
            return Err(HostTransactionError::TouchesEditableRegion);
        }

        let selection = self.model.capture_active_selection_in_editable_region();
        let marked = self.model.capture_marked_in_editable_region();
        let mut next_text = self.model.text.clone();
        for edit in transaction.edits() {
            next_text.replace(edit.range(), edit.replacement());
        }
        let next_regions = DocumentRegions::new(regions, next_text.len())
            .map_err(HostTransactionError::InvalidRegions)?;
        validate_region_boundaries(&next_regions, &next_text)
            .map_err(HostTransactionError::InvalidRegions)?;
        let next_editable: Vec<_> = next_regions
            .as_slice()
            .iter()
            .filter(|region| region.policy() == EditPolicy::Editable)
            .collect();
        if next_editable.len() != editable_regions.len()
            || editable_regions.iter().any(|(id, _, text)| {
                let Some(next) = next_editable.iter().find(|region| region.id() == id) else {
                    return true;
                };
                next_text.slice(next.range()) != text.as_str()
            })
        {
            return Err(HostTransactionError::EditableRegionsChanged);
        }

        let next_projection = match projection {
            Some(projection) => projection,
            None if self.model.projection.is_identity() => {
                DocumentProjection::identity(next_text.len())
            }
            None => return Err(HostTransactionError::ProjectionRequired),
        };
        validate_projection(&next_projection, &next_regions, &next_text)
            .map_err(HostTransactionError::InvalidProjection)?;
        let mut next_blocks = match blocks {
            Some(blocks) => blocks,
            None if self.model.blocks.is_empty() => Vec::new(),
            None => return Err(HostTransactionError::BlocksRequired),
        };
        next_blocks.sort_by_key(|block| {
            let source = block.source();
            (source.start, source.end)
        });
        self.validate_inline_layout(&next_blocks)
            .map_err(HostTransactionError::InvalidFragment)?;
        validate_blocks(
            &next_blocks,
            &next_regions,
            &next_projection,
            next_text.len(),
        )
        .map_err(HostTransactionError::InvalidBlocks)?;
        let next_projection_map = ProjectionMap::new(&next_text, &next_projection);
        let next_anchored = match anchored_blocks {
            Some(anchored) => anchored,
            None if self.model.anchored_blocks.is_empty() => Vec::new(),
            None => return Err(HostTransactionError::AnchoredBlocksRequired),
        };
        DocumentModel::validate_anchored_blocks(
            &next_anchored,
            &next_blocks,
            &next_projection_map,
            &next_regions,
            &next_text,
        )
        .map_err(HostTransactionError::InvalidAnchoredBlocks)?;
        let next_styles = match styles {
            Some(styles) => styles,
            None if self.model.styles.is_empty() => DocumentStyles::default(),
            None => return Err(HostTransactionError::StylesRequired),
        };
        let next_resolved_styles = resolve_document_styles(
            &next_text,
            &next_regions,
            &next_projection_map,
            &next_styles,
            &next_blocks,
        )
        .map_err(HostTransactionError::InvalidStyles)?;
        let first_changed_item = transaction
            .edits()
            .iter()
            .map(|edit| self.layout_item_for_source(edit.range().start))
            .min()
            .unwrap_or(0);
        // One edit that leaves the presentation around it alone changes only
        // the display lines it touches.
        let local_edit = match transaction.edits() {
            [edit] => host::HostEditPresentation {
                edit: edit.range(),
                len: edit.replacement().len(),
                regions: next_regions.as_slice(),
                projection: &next_projection,
                blocks: &next_blocks,
                anchored_blocks: &next_anchored,
                styles: &next_styles,
            }
            .keeps_lines_apart(&self.model)
            .then(|| (edit.range(), edit.replacement().len())),
            _ => None,
        };
        let display_len = self.model.display_text().len();

        for edit in transaction.edits() {
            self.model
                .anchors
                .apply_edit(&edit.range(), edit.replacement().len());
            self.model.text.replace(edit.range(), edit.replacement());
        }
        self.model.regions = next_regions;
        self.model.reconcile_anchor_nodes();
        self.model.projection = next_projection;
        self.model.projection_map = next_projection_map;
        self.model.blocks = next_blocks;
        self.model.anchored_blocks = next_anchored;
        self.model.styles = next_styles;
        self.model.resolved_styles = next_resolved_styles;
        self.model.revision = self.model.revision.next();
        let display_delta = self.model.display_text().len() as isize - display_len as isize;
        if !local_edit.is_some_and(|(edit, len)| {
            self.relayout_host_edit(edit, len, display_delta, first_changed_item)
        }) {
            self.reconcile_layout_items_from(first_changed_item);
        }
        if let Some(selection) = &selection {
            self.model.restore_active_selection(selection);
        }
        self.model.restore_marked(marked.as_ref());
        cx.emit(DocumentEvent::Changed {
            revision: self.model.revision,
            origin: EditOrigin::Host,
        });
        cx.notify();
        Ok(())
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
        origin: EditOrigin,
        composition_selection: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> EditDecision<I> {
        let transaction = match EditTransaction::new(
            self.model.revision,
            origin,
            vec![TextEdit::new(range.clone(), text)],
            self.model.text.len(),
        ) {
            Ok(transaction) => transaction,
            Err(
                TransactionError::Empty
                | TransactionError::InvalidRange { .. }
                | TransactionError::OutOfBounds { .. }
                | TransactionError::Overlap { .. },
            ) => {
                cx.emit(DocumentEvent::Rejected(
                    DocumentEditRejection::OutsideRegion,
                ));
                return EditDecision::Reject;
            }
        };
        self.commit_user_transaction(transaction, composition_selection, cx)
    }

    fn restore_undo_record(
        &mut self,
        record: &UndoRecord<I>,
        undo: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let UndoKind::Text { region_id, changes } = &record.kind else {
            return false;
        };
        self.settle_list_scroll();
        // Replay into a tentative model, so a missing node, changed source or
        // invalid presentation rejects the whole record before any live write.
        let mut model = self.model.clone();
        let origin = if undo {
            EditOrigin::Undo
        } else {
            EditOrigin::Redo
        };
        let changes: Box<dyn Iterator<Item = &UndoChange>> = if undo {
            Box::new(changes.iter().rev())
        } else {
            Box::new(changes.iter())
        };
        let mut first_changed = self.model.text.len();
        for change in changes {
            let Some(index) = model.region_index(region_id) else {
                return false;
            };
            let region = &model.regions.as_slice()[index];
            if region.policy() != EditPolicy::Editable {
                return false;
            }
            let (expected, replacement) = if undo {
                (&change.inserted, &change.removed)
            } else {
                (&change.removed, &change.inserted)
            };
            let start = region.range().start + change.range.start;
            let range = start..start + expected.len();
            if range.end > region.range().end
                || !valid_source_range(&model.text, &range)
                || model.text.slice(range.clone()) != expected.as_str()
            {
                return false;
            }
            let transaction = EditTransaction::new(
                model.revision,
                origin,
                vec![TextEdit::new(range, replacement)],
                model.text.len(),
            )
            .unwrap();
            if !matches!(
                model.route_and_apply(&transaction, Some(region_id)),
                RoutingOutcome::Applied
            ) {
                return false;
            }
            first_changed = first_changed.min(start);
        }
        let selection = if undo {
            &record.selection_before
        } else {
            &record.selection_after
        };
        if let Some((anchor, head)) = selection {
            let _ = model.replace_selection_positions(anchor.clone(), head.clone());
        }
        model.set_marked_range(None);
        model.revision = self.model.revision.next();
        let first_item = self.layout_item_for_source(first_changed);
        self.model = model;
        self.reconcile_layout_items_from(first_item);
        self.finish_user_edit(origin, cx);
        true
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo.break_coalescing();
        if self.model.marked.is_some() {
            return;
        }
        if let Some(UndoRecord {
            kind: UndoKind::Structure { before, after },
            selection_before,
            ..
        }) = self.undo.undo.last()
        {
            cx.emit(DocumentEvent::StructureReplayRequested(StructureReplay {
                target: before.clone(),
                selection: selection_before.clone(),
                undo: true,
                start_node: after.regions[0].id().clone(),
            }));
            return;
        }
        let Some(record) = self.undo.undo.pop() else {
            return;
        };
        if self.restore_undo_record(&record, true, cx) {
            self.undo.redo.push(record);
        } else {
            self.undo.undo.push(record);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo.break_coalescing();
        if self.model.marked.is_some() {
            return;
        }
        if let Some(UndoRecord {
            kind: UndoKind::Structure { before, after },
            selection_after,
            ..
        }) = self.undo.redo.last()
        {
            cx.emit(DocumentEvent::StructureReplayRequested(StructureReplay {
                target: after.clone(),
                selection: selection_after.clone(),
                undo: false,
                start_node: before.regions[0].id().clone(),
            }));
            return;
        }
        let Some(record) = self.undo.redo.pop() else {
            return;
        };
        if self.restore_undo_record(&record, false, cx) {
            self.undo.undo.push(record);
        } else {
            self.undo.redo.push(record);
        }
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.model.text.offset_utf16_to_offset(range.start)
            ..self.model.text.offset_utf16_to_offset(range.end)
    }

    fn input_position_for_offset(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> Option<DocumentPosition<I>> {
        let head = self
            .model
            .position_for_anchor(self.model.selection.head())
            .ok()?;
        let region = self.region(head.node_id())?.range();
        if region.contains_inclusive(offset) {
            Some(DocumentPosition::new(
                head.into_node_id(),
                offset - region.start,
                affinity,
            ))
        } else {
            self.position_for_offset(offset, affinity).ok()
        }
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.model.text.offset_to_offset_utf16(range.start)
            ..self.model.text.offset_to_offset_utf16(range.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        if let Some(block) = self.model.blocks.iter().find(|block| {
            block.inline_size().is_some()
                && block.source().start < offset
                && offset <= block.source().end
        }) {
            return block.source().start;
        }
        self.model
            .text
            .clip_offset(offset.saturating_sub(1), Bias::Left)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        if let Some(block) = self.model.blocks.iter().find(|block| {
            block.inline_size().is_some()
                && block.source().start <= offset
                && offset < block.source().end
        }) {
            return block.source().end;
        }
        self.model
            .text
            .clip_offset(offset.saturating_add(1), Bias::Right)
    }

    fn start_of_line(&self, offset: usize) -> usize {
        self.model.text.line_to_byte_idx(
            self.model.text.byte_to_line_idx(offset, LineType::LF),
            LineType::LF,
        )
    }

    fn end_of_line(&self, offset: usize) -> usize {
        let line = self.model.text.byte_to_line_idx(offset, LineType::LF);
        let start = self.model.text.line_to_byte_idx(line, LineType::LF);
        let text = self.model.text.line(line, LineType::LF).to_string();
        start + text.trim_end_matches(['\r', '\n']).len()
    }

    fn previous_word_boundary(&self, offset: usize) -> usize {
        let text = self.model.text.to_string();
        let mut offset = self.model.text.clip_offset(offset, Bias::Left);
        while offset > 0 {
            let previous = self.model.text.clip_offset(offset - 1, Bias::Left);
            let character = text[previous..offset].chars().next().unwrap();
            if !character.is_whitespace() {
                break;
            }
            offset = previous;
        }
        while offset > 0 {
            let previous = self.model.text.clip_offset(offset - 1, Bias::Left);
            let character = text[previous..offset].chars().next().unwrap();
            if character.is_whitespace() {
                break;
            }
            offset = previous;
        }
        offset
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        let text = self.model.text.to_string();
        let mut offset = self.model.text.clip_offset(offset, Bias::Right);
        while offset < text.len() {
            let next = self.model.text.clip_offset(offset + 1, Bias::Right);
            let character = text[offset..next].chars().next().unwrap();
            if character.is_whitespace() {
                break;
            }
            offset = next;
        }
        while offset < text.len() {
            let next = self.model.text.clip_offset(offset + 1, Bias::Right);
            let character = text[offset..next].chars().next().unwrap();
            if !character.is_whitespace() {
                break;
            }
            offset = next;
        }
        offset
    }

    fn move_selection_head(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        let position = self
            .model
            .position_for_anchor(self.model.selection.head())
            .ok()
            .and_then(|position| {
                let region = self.region(position.node_id())?.range();
                region.contains_inclusive(offset).then(|| {
                    DocumentPosition::new(
                        position.into_node_id(),
                        offset - region.start,
                        Affinity::After,
                    )
                })
            })
            .or_else(|| self.position_for_offset(offset, Affinity::After).ok());
        if let Some(position) = position {
            self.move_selection_to_position(position, extend, cx);
        }
    }

    fn move_selection_to_position(
        &mut self,
        position: DocumentPosition<I>,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.undo.break_coalescing();
        self.preferred_x = None;
        if extend {
            let Ok(anchor) = self
                .model
                .position_for_anchor(self.model.selection.anchor())
            else {
                return;
            };
            self.model
                .replace_selection_positions(anchor, position)
                .expect("navigation positions must resolve");
        } else {
            self.model
                .replace_selection_positions(position.clone(), position)
                .expect("navigation position must resolve");
        }
        self.reveal_caret(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn vertical_position(&mut self, down: bool, page: bool) -> Option<DocumentPosition<I>> {
        let range = self.model.selected_range();
        let cursor = if range.is_empty() {
            self.model.cursor()
        } else if down {
            range.end
        } else {
            range.start
        };
        let cursor = if cursor == self.model.cursor() {
            self.model
                .position_for_anchor(self.model.selection.head())
                .ok()?
        } else {
            self.position_for_offset(cursor, Affinity::After).ok()?
        };
        let Some((position, line_height, item_ix, bounds)) =
            self.text_position_for_position(&cursor)
        else {
            return Some(cursor);
        };
        let x = *self.preferred_x.get_or_insert(position.x);
        // Short cells leave blank space below their final visual line. That
        // space is not another keyboard row, even if a neighboring cell is tall.
        let bounds = self
            .text_layouts
            .iter()
            .find(|record| record.item_ix == item_ix)
            .and_then(|record| record.layout.cell_text_bounds(position))
            .unwrap_or(bounds);
        let distance = if page {
            self.list_state
                .viewport_bounds()
                .size
                .height
                .max(line_height)
        } else {
            line_height
        };
        let y = position.y + line_height / 2. + if down { distance } else { -distance };
        if !page && (y < bounds.top() || y >= bounds.bottom()) {
            // Paragraph gaps and mixed font sizes are not keyboard rows. Move
            // to the adjacent layout item instead of landing back in this gap.
            let adjacent = if down {
                self.text_layouts
                    .iter()
                    .find(|record| record.item_ix > item_ix)
            } else {
                self.text_layouts
                    .iter()
                    .rev()
                    .find(|record| record.item_ix < item_ix)
            };
            if let Some(record) = adjacent {
                let y = if down {
                    record.bounds.top() + record.layout.line_height() / 2.
                } else {
                    record.bounds.bottom() - record.layout.line_height() / 2.
                };
                return self.position_for_point(point(x, y));
            }
        }
        self.position_for_point(point(x, y))
    }

    fn move_vertical(&mut self, down: bool, page: bool, extend: bool, cx: &mut Context<Self>) {
        if let Some(position) = self.vertical_position(down, page) {
            let preferred_x = self.preferred_x;
            self.move_selection_to_position(position, extend, cx);
            self.preferred_x = preferred_x;
        }
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        let offset = if range.is_empty() {
            self.previous_boundary(self.model.cursor())
        } else {
            range.start
        };
        self.move_selection_head(offset, false, cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        let offset = if range.is_empty() {
            self.next_boundary(self.model.cursor())
        } else {
            range.end
        };
        self.move_selection_head(offset, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let (_, head) = self.model.selection_offsets();
        self.move_selection_head(self.previous_boundary(head), true, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let (_, head) = self.model.selection_offsets();
        self.move_selection_head(self.next_boundary(head), true, cx);
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(false, false, false, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(true, false, false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(false, false, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(true, false, true, cx);
    }

    fn move_page_up(&mut self, _: &MovePageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(false, true, false, cx);
    }

    fn move_page_down(&mut self, _: &MovePageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(true, true, false, cx);
    }

    fn move_home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.start_of_line(self.model.cursor()), false, cx);
    }

    fn move_end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.end_of_line(self.model.cursor()), false, cx);
    }

    fn move_to_start_of_line(
        &mut self,
        _: &MoveToStartOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.start_of_line(self.model.cursor()), false, cx);
    }

    fn move_to_end_of_line(&mut self, _: &MoveToEndOfLine, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.end_of_line(self.model.cursor()), false, cx);
    }

    fn move_to_start(&mut self, _: &MoveToStart, _: &mut Window, cx: &mut Context<Self>) {
        // Document-wide jumps choose the endpoint node even when it shares an
        // offset with the current node.
        if let Ok(position) = self.position_for_offset(0, Affinity::Before) {
            self.move_selection_to_position(position, false, cx);
        }
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(position) = self.position_for_offset(self.model.text.len(), Affinity::After) {
            self.move_selection_to_position(position, false, cx);
        }
    }

    fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.previous_word_boundary(self.model.cursor()), false, cx);
    }

    fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.next_word_boundary(self.model.cursor()), false, cx);
    }

    fn select_to_start_of_line(
        &mut self,
        _: &SelectToStartOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.start_of_line(self.model.cursor()), true, cx);
    }

    fn select_to_end_of_line(
        &mut self,
        _: &SelectToEndOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.end_of_line(self.model.cursor()), true, cx);
    }

    fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(position) = self.position_for_offset(0, Affinity::Before) {
            self.move_selection_to_position(position, true, cx);
        }
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(position) = self.position_for_offset(self.model.text.len(), Affinity::After) {
            self.move_selection_to_position(position, true, cx);
        }
    }

    fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.previous_word_boundary(self.model.cursor()), true, cx);
    }

    fn select_to_next_word(
        &mut self,
        _: &SelectToNextWordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection_head(self.next_word_boundary(self.model.cursor()), true, cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        let range = if range.is_empty() {
            self.previous_boundary(self.model.cursor())..self.model.cursor()
        } else {
            range
        };
        if !range.is_empty() {
            self.replace(range, "", EditOrigin::User, None, cx);
        }
    }

    fn delete_to_start_of_line(
        &mut self,
        _: &DeleteToBeginningOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(
            self.start_of_line(cursor)..cursor,
            "",
            EditOrigin::User,
            None,
            cx,
        );
    }

    fn delete_to_end_of_line(
        &mut self,
        _: &DeleteToEndOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(
            cursor..self.end_of_line(cursor),
            "",
            EditOrigin::User,
            None,
            cx,
        );
    }

    fn delete_previous_word(
        &mut self,
        _: &DeleteToPreviousWordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(
            self.previous_word_boundary(cursor)..cursor,
            "",
            EditOrigin::User,
            None,
            cx,
        );
    }

    fn delete_next_word(
        &mut self,
        _: &DeleteToNextWordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(
            cursor..self.next_word_boundary(cursor),
            "",
            EditOrigin::User,
            None,
            cx,
        );
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        let range = if range.is_empty() {
            self.model.cursor()..self.next_boundary(self.model.cursor())
        } else {
            range
        };
        if !range.is_empty() {
            self.replace(range, "", EditOrigin::User, None, cx);
        }
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        self.replace(
            self.model.selected_range(),
            "\n",
            EditOrigin::User,
            None,
            cx,
        );
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.undo.break_coalescing();
        self.preferred_x = None;
        self.model.replace_selection(0, self.model.text.len());
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        if !range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.model.text.slice(range).to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.model.selected_range();
        if range.is_empty() {
            return;
        }
        let text = self.model.text.slice(range.clone()).to_string();
        if self.replace(range, "", EditOrigin::User, None, cx) == EditDecision::Apply {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.replace(
            self.model.selected_range(),
            &text,
            EditOrigin::Paste,
            None,
            cx,
        );
    }

    fn offset_for_point(&self, point: Point<Pixels>) -> usize {
        self.position_for_point(point)
            .and_then(|position| self.resolve_position(&position).ok())
            .unwrap_or_else(|| self.model.cursor())
    }

    fn position_for_point(&self, point: Point<Pixels>) -> Option<DocumentPosition<I>> {
        if self
            .trailer_layout
            .is_some_and(|(_, bounds)| point.y >= bounds.top())
        {
            return self
                .position_for_offset(self.model.text.len(), Affinity::After)
                .ok();
        }

        // Typography and host wrappers leave gaps between text layouts. Those
        // gaps belong to the nearest content boundary, never implicitly to EOF.
        // Include blocks even in their horizontal gutter so dragging across an
        // object resolves to its before/after boundary without entering it.
        let distance = |bounds: Bounds<Pixels>| {
            (bounds.top() - point.y)
                .max(point.y - bounds.bottom())
                .max(Pixels::ZERO)
        };
        let nearest = self
            .text_layouts
            .iter()
            .map(|record| (record.bounds, record.item_ix, Some(record), None))
            .chain(
                self.block_layouts
                    .iter()
                    .map(|record| (record.bounds, record.item_ix, None, Some(record))),
            )
            .min_by(|a, b| {
                f32::from(distance(a.0))
                    .total_cmp(&f32::from(distance(b.0)))
                    .then_with(|| a.1.cmp(&b.1))
            });
        match nearest {
            Some((_, _, Some(record), _)) => self.text_position_for_point(record, point),
            Some((_, _, _, Some(record))) => {
                if point.y < record.bounds.center().y {
                    self.position_for_offset(record.source.start, Affinity::After)
                        .ok()
                } else {
                    self.position_for_offset(record.source.end, Affinity::Before)
                        .ok()
                }
            }
            _ => self
                .model
                .position_for_anchor(self.model.selection.head())
                .ok(),
        }
    }

    /// The source range of the link whose glyph is under `point`, and its
    /// target. Unlike caret placement this is exact: the gap beside a line,
    /// its end or an embedded block is not a link.
    pub(super) fn link_at_point(
        &self,
        point: Point<Pixels>,
    ) -> Option<(Range<usize>, SharedString)> {
        if self
            .block_layouts
            .iter()
            .any(|block| block.bounds.contains(&point))
        {
            return None;
        }
        let record = self
            .text_layouts
            .iter()
            .find(|record| record.bounds.contains(&point))?;
        let local = record.layout.index_at(point)?;
        let offset = self
            .model
            .display_to_source(record.display.start + local, Affinity::After)?;
        let inline = self.model.styles.inline();
        inline[inline.partition_point(|style| style.source().end <= offset)..]
            .iter()
            .take_while(|style| style.source().start <= offset)
            .find_map(|style| {
                let link = style.link()?;
                style.source().contains(&offset).then(|| {
                    let mut range = style.source();
                    let at = inline.partition_point(|style| style.source().end <= range.start);
                    for previous in inline[..at].iter().rev() {
                        if previous.source().end != range.start || previous.link() != Some(link) {
                            break;
                        }
                        range.start = previous.source().start;
                    }
                    let at = inline.partition_point(|style| style.source().start < range.end);
                    for next in &inline[at..] {
                        if next.source().start != range.end || next.link() != Some(link) {
                            break;
                        }
                        range.end = next.source().end;
                    }
                    (range, link.clone())
                })
            })
    }

    fn text_position_for_point(
        &self,
        record: &TextLayoutRecord,
        point: Point<Pixels>,
    ) -> Option<DocumentPosition<I>> {
        let DocumentLayoutItem::Text {
            source,
            node_id,
            line,
            ..
        } = self.layout_items.get(record.item_ix)?
        else {
            return None;
        };
        if source.is_empty()
            && let Some(node_id) = node_id
        {
            let region = self.region(node_id)?.range();
            return Some(DocumentPosition::new(
                node_id.clone(),
                source.start - region.start,
                Affinity::After,
            ));
        }
        let local = if point.y < record.bounds.top() {
            0
        } else if point.y >= record.bounds.bottom() {
            record.display.len()
        } else {
            // Each document item is one logical line. Choose the nearest caret
            // boundary, not the containing glyph; Err also carries a valid
            // index at either edge of the actual soft-wrapped row.
            // A record can predate the text; without it, every glyph counts.
            let text = self
                .model
                .display_text()
                .get(record.display.clone())
                .unwrap_or_default();
            record.layout.closest_index(point, text)
        };
        // A line's end follows the markup its text hides there, as the item
        // does. The caret at a soft-wrapped row's end stays on that row, and
        // at its node's end stays in that node.
        let at_end = local == record.display.len() && local > 0;
        let at_line_end = at_end && record.display.end == line.end;
        let offset = if at_line_end {
            source.end
        } else {
            self.model
                .display_to_source(
                    record.display.start + local,
                    if at_end {
                        Affinity::Before
                    } else {
                        Affinity::After
                    },
                )
                .expect("laid out display positions map to source")
                .clamp(source.start, source.end)
        };
        let node_end = node_id
            .as_ref()
            .and_then(|node| self.region(node))
            .is_some_and(|region| region.range().end == offset);
        let affinity = if at_end && (!at_line_end || node_end) {
            Affinity::Before
        } else {
            Affinity::After
        };
        self.position_for_offset(
            offset,
            if offset == self.model.text.len() {
                Affinity::After
            } else {
                affinity
            },
        )
        .ok()
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.undo.break_coalescing();
        self.preferred_x = None;
        if self
            .block_layouts
            .iter()
            .any(|block| block.bounds.contains(&event.position))
        {
            self.auto_scroll.stop();
            return;
        }
        self.auto_scroll.stop();
        self.pressed_link = (!event.modifiers.shift && event.click_count == 1)
            .then(|| self.link_at_point(event.position))
            .flatten()
            .map(|(source, _)| source);
        let position = self.position_for_point(event.position);
        // Placing the caret in editable text beside followed content keeps
        // following; selecting other content is a deliberate inspection.
        if event.modifiers.shift
            || !position
                .as_ref()
                .is_some_and(|position| self.is_editable_position(position))
        {
            self.stop_following(cx);
        }
        self.cancel_caret_reveal();
        self.focus_handle.focus(window, cx);
        let Some(position) = position else {
            return;
        };
        let anchor = if event.modifiers.shift {
            self.model
                .position_for_anchor(self.model.selection.anchor())
                .unwrap_or_else(|_| position.clone())
        } else {
            position.clone()
        };
        self.auto_scroll.last_drag_position = Some(event.position);
        self.model
            .replace_selection_positions(anchor, position)
            .expect("hit-tested positions must resolve");
        self.pause_blink_cursor(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drag_table(event, cx) {
            return;
        }
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.auto_scroll.stop();
            let link = hovered
                .then(|| self.link_at_point(event.position))
                .flatten()
                .map(|(source, _)| source);
            if link != self.hovered_link {
                // The element picks the cursor while painting.
                self.hovered_link = link;
                cx.notify();
            }
            return;
        }
        if self.auto_scroll.last_drag_position.is_none() {
            return;
        }
        self.auto_scroll.last_drag_position = Some(event.position);
        self.extend_pointer_selection(cx);
        let delta = AutoScroll::compute_delta(event.position.y, self.list_state.viewport_bounds());
        if self.viewport.pin.is_some()
            && (delta.is_some()
                || !self
                    .model
                    .position_for_anchor(self.model.selection.head())
                    .is_ok_and(|head| self.is_editable_position(&head)))
        {
            self.stop_following(cx);
        }
        self.auto_scroll.set(delta, cx, |delta, state, cx| {
            state.scroll_by(delta, cx);
            // Resolve the pointer again after prepaint supplies the newly
            // visible rows, not against the previous frame's text geometry.
            cx.notify();
        });
    }

    pub(super) fn mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != gpui::MouseButton::Left {
            return;
        }
        if self.tables.end_drag() {
            cx.notify();
            return;
        }
        self.auto_scroll.stop();
        // A drag that selected text, even within the link, is not a click.
        if let Some(pressed) = self.pressed_link.take()
            && self.selected_range().is_empty()
            && let Some((source, target)) = self.link_at_point(event.position)
            && source == pressed
        {
            cx.emit(DocumentEvent::LinkClicked(target));
        }
    }

    pub(super) fn extend_pointer_selection(&mut self, cx: &mut Context<Self>) {
        let Some(mut point) = self.auto_scroll.last_drag_position else {
            return;
        };
        let viewport = self.list_state.viewport_bounds();
        point.y = point.y.clamp(viewport.top(), viewport.bottom());
        let Some(head) = self.position_for_point(point) else {
            return;
        };
        // The selection anchor is already tracked through host transactions.
        // Keeping another raw byte offset here would drift during streaming.
        let Ok(anchor) = self
            .model
            .position_for_anchor(self.model.selection.anchor())
        else {
            return;
        };
        if self
            .model
            .position_for_anchor(self.model.selection.head())
            .ok()
            .as_ref()
            != Some(&head)
        {
            self.model
                .replace_selection_positions(anchor, head)
                .expect("drag positions must resolve");
            cx.emit(DocumentEvent::SelectionChanged);
            cx.notify();
        }
    }

    #[cfg(test)]
    fn screen_position_for_source(
        &self,
        source: usize,
        affinity: Affinity,
    ) -> Option<Point<Pixels>> {
        let display = self.model.source_to_display(source, affinity)?;
        self.text_position_for_display(display, affinity)
            .map(|(position, _, _, _)| position)
    }

    fn text_item_for_position(&self, position: &DocumentPosition<I>) -> Option<usize> {
        if let Some((item_ix, _)) = self.node_end_row(position) {
            return Some(item_ix);
        }
        let offset = self.resolve_position(position).ok()?;
        let found =
            self.text_item_for_offset_in(position, offset, self.items_around_source(offset));
        #[cfg(test)]
        assert_eq!(
            found,
            self.text_item_for_offset_in(position, offset, 0..self.layout_items.len())
        );
        found
    }

    fn text_item_for_offset_in(
        &self,
        position: &DocumentPosition<I>,
        offset: usize,
        items: Range<usize>,
    ) -> Option<usize> {
        let mut fallback = None;
        let mut owned = None;
        for (ix, item) in self
            .layout_items
            .range(items.clone())
            .enumerate()
            .map(|(ix, item)| (items.start + ix, item))
        {
            let DocumentLayoutItem::Text {
                source, node_id, ..
            } = item
            else {
                continue;
            };
            let same_node = node_id.as_ref() == Some(position.node_id());
            if !source.contains_inclusive(offset) || (source.is_empty() && !same_node) {
                continue;
            }
            if fallback.is_none() || position.affinity() == Affinity::After {
                fallback = Some(ix);
            }
            if same_node && (owned.is_none() || position.affinity() == Affinity::After) {
                owned = Some(ix);
            }
        }
        owned.or(fallback)
    }

    fn text_position_for_position(
        &self,
        position: &DocumentPosition<I>,
    ) -> Option<(Point<Pixels>, Pixels, usize, Bounds<Pixels>)> {
        let (item_ix, display) = match self.node_end_row(position) {
            Some(row) => row,
            None => {
                let source = self.resolve_position(position).ok()?;
                (
                    self.text_item_for_position(position)?,
                    self.model.source_to_display(source, position.affinity())?,
                )
            }
        };
        let record = self
            .text_layouts
            .iter()
            .find(|record| record.item_ix == item_ix)?;
        if !record.display.contains_inclusive(display) {
            return None;
        }
        let local = display - record.display.start;
        record.layout.position_for_index(local).map(|position| {
            (
                position,
                record.layout.line_height(),
                item_ix,
                record.bounds,
            )
        })
    }

    fn text_position_for_display(
        &self,
        display: usize,
        affinity: Affinity,
    ) -> Option<(Point<Pixels>, Pixels, usize, Bounds<Pixels>)> {
        let item_ix = self.text_item_for_display(display, affinity)?;
        let record = self
            .text_layouts
            .iter()
            .find(|record| record.item_ix == item_ix)?;
        record
            .layout
            .position_for_index(display.saturating_sub(record.display.start))
            .map(|position| {
                (
                    position,
                    record.layout.line_height(),
                    record.item_ix,
                    record.bounds,
                )
            })
    }

    // Resolve against the whole layout, so virtualization cannot transfer a boundary
    // caret to a preceding item when its actual owner is just outside the viewport.
    fn text_item_for_display(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        let found =
            self.text_item_for_display_in(offset, affinity, self.items_around_display(offset));
        #[cfg(test)]
        assert_eq!(
            found,
            self.text_item_for_display_in(offset, affinity, 0..self.layout_items.len())
        );
        found
    }

    fn text_item_for_display_in(
        &self,
        offset: usize,
        affinity: Affinity,
        items: Range<usize>,
    ) -> Option<usize> {
        let mut matches = self
            .layout_items
            .range(items.clone())
            .enumerate()
            .map(|(ix, item)| (items.start + ix, item))
            .filter_map(|(ix, item)| match item {
                DocumentLayoutItem::Text { display, .. }
                    if display.start <= offset && offset <= display.end =>
                {
                    Some(ix)
                }
                _ => None,
            });
        if affinity == Affinity::Before {
            matches.next()
        } else {
            matches.next_back()
        }
    }

    fn text_position_for_display_in_item(
        &self,
        display: usize,
        affinity: Affinity,
        item_ix: usize,
    ) -> Option<(Point<Pixels>, Pixels, usize, Bounds<Pixels>)> {
        self.text_position_for_display(display, affinity)
            .or_else(|| {
                let record = self
                    .text_layouts
                    .iter()
                    .find(|record| record.item_ix == item_ix)?;
                let display = display.clamp(record.display.start, record.display.end);
                record
                    .layout
                    .position_for_index(display - record.display.start)
                    .map(|position| {
                        (
                            position,
                            record.layout.line_height(),
                            record.item_ix,
                            record.bounds,
                        )
                    })
            })
    }

    fn is_editable_position(&self, position: &DocumentPosition<I>) -> bool {
        self.region(position.node_id())
            .is_some_and(|region| region.policy() == EditPolicy::Editable)
    }

    /// The layout item that owns `position` and its window point, when laid out.
    fn layout_point_for_position(
        &self,
        position: &DocumentPosition<I>,
    ) -> Option<(usize, Option<Point<Pixels>>)> {
        // Hidden atomic blocks can share a display offset with one another and
        // with adjacent text. Resolve the owner before consulting visible layout,
        // so an offscreen block cannot borrow a neighbouring text row's geometry.
        let block_ix = self.layout_items.iter().position(|item| {
            matches!(item, DocumentLayoutItem::Block { id, .. }
                    if id == position.node_id())
        });
        if let Some(item_ix) = block_ix {
            let point = self
                .block_layouts
                .iter()
                .find(|record| record.item_ix == item_ix)
                .map(|record| {
                    point(
                        record.bounds.left(),
                        if position.offset() == record.source.len() {
                            record.bounds.bottom()
                        } else {
                            record.bounds.top()
                        },
                    )
                });
            return Some((item_ix, point));
        }
        let item_ix = self.text_item_for_position(position)?;
        Some((
            item_ix,
            self.text_position_for_position(position)
                .map(|(position, _, _, _)| position),
        ))
    }

    /// Where a node's end is drawn: the end of its last text row. The display
    /// offset of the boundary lies after the line break the node's display
    /// ends with, which anchored rows rely on, but that offset belongs to the
    /// next row; a position before the boundary is drawn at the end of this one.
    fn node_end_row(&self, position: &DocumentPosition<I>) -> Option<(usize, usize)> {
        if position.affinity() != Affinity::Before {
            return None;
        }
        let item_ix = self.node_end_item(position)?;
        match &self.layout_items[item_ix] {
            DocumentLayoutItem::Text { display, .. } => Some((item_ix, display.end)),
            _ => None,
        }
    }

    /// The last text row of `position`'s node, when `position` is that node's end.
    fn node_end_item(&self, position: &DocumentPosition<I>) -> Option<usize> {
        let region = self.region(position.node_id())?;
        if position.offset() != region.range().len() {
            return None;
        }
        let end = self
            .layout_items
            .partition_point(|item| item.source_start() <= region.range().end);
        (0..end)
            .rev()
            .take_while(|ix| self.layout_items[*ix].source_start() >= region.range().start)
            .find(|ix| {
                matches!(&self.layout_items[*ix], DocumentLayoutItem::Text { node_id, .. }
                    if node_id.as_ref() == Some(position.node_id()))
            })
    }

    /// Where `position` sits in the viewport, from 0 at the top to 1 at the
    /// bottom, or `None` when it is not laid out in the last frame.
    pub fn viewport_fraction(&self, position: &DocumentPosition<I>) -> Option<f32> {
        self.resolve_position(position).ok()?;
        let (_, point) = self.layout_point_for_position(position)?;
        let point = point?;
        let viewport = self.list_state.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return None;
        }
        Some((point.y - viewport.top()) / viewport.size.height)
    }

    /// Where the text of item `item_ix` was laid out this frame.
    #[cfg(debug_assertions)]
    pub(super) fn text_bounds(&self, item_ix: usize) -> Option<Bounds<Pixels>> {
        self.text_layouts
            .iter()
            .find(|record| record.item_ix == item_ix)
            .map(|record| record.bounds)
    }

    pub(super) fn update_text_layout(
        &mut self,
        item_ix: usize,
        display: Range<usize>,
        layout: impl Into<inline_objects::LineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        let layout = layout.into();
        if let Some(record) = self
            .text_layouts
            .iter_mut()
            .find(|record| record.item_ix == item_ix)
        {
            record.display = display;
            record.layout = layout;
            record.bounds = bounds;
        } else {
            self.text_layouts.push(TextLayoutRecord {
                item_ix,
                display,
                layout,
                bounds,
            });
            self.text_layouts.sort_by_key(|record| record.item_ix);
        }
    }

    pub(super) fn update_block_layout(
        &mut self,
        item_ix: usize,
        _id: I,
        source: Range<usize>,
        bounds: Bounds<Pixels>,
    ) {
        if let Some(record) = self
            .block_layouts
            .iter_mut()
            .find(|record| record.item_ix == item_ix)
        {
            record.source = source;
            record.bounds = bounds;
        } else {
            self.block_layouts.push(BlockLayoutRecord {
                item_ix,
                source,
                bounds,
            });
        }
    }

    pub(super) fn prepare_layout(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let layout_style = (window.text_style(), window.rem_size());
        let style_changed = self
            .layout_style
            .as_ref()
            .is_some_and(|previous| previous != &layout_style);
        let width_changed = self
            .last_bounds
            .is_some_and(|previous| previous.size.width != bounds.size.width);
        if style_changed || width_changed {
            self.preferred_x = None;
        }
        if style_changed {
            self.list_state.remeasure_items(0..self.layout_items.len());
        }
        self.layout_style = Some(layout_style);
        self.last_bounds = Some(bounds);
        self.prepare_text_rows(bounds, window, cx);
    }

    pub(super) fn update_trailer_layout(&mut self, item_ix: usize, bounds: Bounds<Pixels>) {
        self.trailer_layout = Some((item_ix, bounds));
    }

    pub(super) fn focus_handle_snapshot(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    fn display_selection_range(&self) -> Range<usize> {
        let source_range = self.model.selected_range();
        if source_range.is_empty() {
            let offset = self
                .model
                .source_to_display(source_range.start, Affinity::After)
                .unwrap_or(self.model.display_text().len());
            offset..offset
        } else {
            self.model
                .source_to_display(source_range.start, Affinity::Before)
                .unwrap_or(0)
                ..self
                    .model
                    .source_to_display(source_range.end, Affinity::After)
                    .unwrap_or(self.model.display_text().len())
        }
    }

    fn caret_is_editable(&self) -> bool {
        (self.model.selected_range().is_empty() || self.model.marked_range().is_some())
            && self
                .model
                .position_for_anchor(self.model.selection.head())
                .is_ok_and(|position| {
                    self.region(position.node_id())
                        .is_some_and(|region| region.policy() == EditPolicy::Editable)
                })
    }

    pub(super) fn show_cursor(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.is_focused(window)
            && window.is_window_active()
            && self.caret_is_editable()
            && (self.model.marked_range().is_some()
                || cx.reduce_motion()
                || self.blink_cursor.read(cx).visible())
    }

    fn pause_blink_cursor(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_active {
            return;
        }
        self.blink_cursor.update(cx, |cursor, cx| {
            if self.caret_is_editable() {
                cursor.pause(cx);
            } else {
                cursor.stop(cx);
            }
        });
    }

    fn update_cursor_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor_active = self.focus_handle.is_focused(window) && window.is_window_active();
        self.blink_cursor.update(cx, |cursor, cx| {
            if self.cursor_active && self.caret_is_editable() {
                cursor.start(cx);
            } else {
                cursor.stop(cx);
            }
        });
    }

    pub(super) fn prepare_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor_focus_subscriptions.is_some() {
            return;
        }
        // Construction is window-independent; bind focus observers at the first prepaint.
        let focus = self.focus_handle.clone();
        self.cursor_focus_subscriptions = Some([
            cx.on_focus(&focus, window, Self::update_cursor_focus),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.cursor_active = false;
                this.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
            }),
            cx.observe_window_activation(window, Self::update_cursor_focus),
        ]);
        self.update_cursor_focus(window, cx);
    }

    pub(super) fn segment_paint_snapshot(
        &self,
        item_ix: usize,
        display: &Range<usize>,
    ) -> (FocusHandle, Option<Range<usize>>, Option<usize>) {
        let selection = self.display_selection_range();
        let selected_range = if selection.is_empty()
            || selection.end < display.start
            || display.end < selection.start
        {
            None
        } else {
            Some(
                selection.start.max(display.start) - display.start
                    ..selection.end.min(display.end) - display.start,
            )
        };
        let cursor = self
            .model
            .position_for_anchor(self.model.selection.head())
            .ok()
            .filter(|position| self.text_item_for_position(position) == Some(item_ix))
            .and_then(|position| {
                self.model
                    .source_to_display(self.model.cursor(), position.affinity())
            })
            .map(|cursor| cursor.clamp(display.start, display.end) - display.start);
        (self.focus_handle.clone(), selected_range, cursor)
    }

    fn render_layout_item(
        &self,
        item_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut text_context = None;
        let mut hidden = false;
        #[cfg(debug_assertions)]
        let mut text_top = None;
        let child = match self
            .layout_items
            .get(item_ix)
            .expect("GPUI list requested a valid document item")
        {
            DocumentLayoutItem::Text {
                display,
                source,
                node_id,
                presentation,
                line,
                frame,
                wrapped,
                ..
            } => {
                if let Some(node_id) = node_id {
                    let region = self
                        .region_at(node_id, source.start)
                        .map(DocumentRegion::range);
                    text_context = Some(DocumentTextContext {
                        node_id: node_id.clone(),
                        source: source.clone(),
                        node_start: region.as_ref().map_or(0, |region| region.start),
                        first_line: region.is_some_and(|region| source.start <= region.start),
                        first_row: display.start == line.start,
                        last_row: display.end == line.end,
                    });
                }
                let mut presentation = presentation.as_deref().cloned().unwrap_or_default();
                self.retained_row_style(wrapped.0, &mut presentation);
                if self.text_rows.is_some() {
                    // A display row is one line; its wrap was decided with the geometry.
                    presentation.text_style.get_or_insert_default().white_space =
                        Some(gpui::WhiteSpace::Nowrap);
                    hidden = frame.0.is_some_and(|frame| {
                        frame.row_height(display, line, Some(&presentation)) == px(0.)
                    });
                    #[cfg(debug_assertions)]
                    {
                        text_top = frame.0.map(|frame| frame.text_top(display, line));
                    }
                }
                // Tables clip individual cell lines against the viewport; their
                // geometry always addresses the complete row, including pending cells.
                let display = if presentation.table_row.is_some() {
                    display.clone()
                } else {
                    self.pending_row_paint_range(display.clone(), wrapped.0)
                };
                if let Some(hovered) = &self.hovered_link {
                    let mut changed = false;
                    let inline = self.model.styles.inline();
                    let from = inline.partition_point(|style| style.source().end <= hovered.start);
                    for style in inline[from..]
                        .iter()
                        .take_while(|style| style.source().start < hovered.end)
                    {
                        let Some(color) = style.link_hover_color() else {
                            continue;
                        };
                        self.model
                            .projection_map
                            .project_source_range(style.source(), |range| {
                                let start = range.start.max(display.start);
                                let end = range.end.min(display.end);
                                if start < end {
                                    changed = true;
                                    presentation.highlights.push((
                                        start - display.start..end - display.start,
                                        HighlightStyle {
                                            color: Some(color),
                                            underline: Some(gpui::UnderlineStyle {
                                                thickness: px(1.),
                                                color: Some(color),
                                                wavy: false,
                                            }),
                                            ..Default::default()
                                        },
                                    ));
                                }
                            });
                    }
                    if changed {
                        presentation.resolve_inline(display.len());
                    }
                }
                presentation.font_scales.retain_mut(|(range, _)| {
                    range.end = range.end.min(display.len());
                    range.start < range.end
                });
                presentation.highlights.retain_mut(|(range, _)| {
                    range.end = range.end.min(display.len());
                    range.start < range.end
                });
                presentation.font_family_overrides.retain_mut(|(range, _)| {
                    range.end = range.end.min(display.len());
                    range.start < range.end
                });
                presentation
                    .objects
                    .retain(|(range, _)| range.end <= display.len());
                // Planning gives every table row geometry before the list
                // renders; a row not yet planned paints as ordinary text.
                let table = (self.text_rows.is_some() && presentation.table_row.is_some())
                    .then(|| self.table_view(&self.layout_items[item_ix]).zip(frame.0))
                    .flatten();
                if let Some((view, frame)) = table {
                    DocumentChild::Inline {
                        element: super::element::table::Row::new(
                            cx.entity(),
                            item_ix,
                            display.clone(),
                            self.model.display_text()[display.clone()].into(),
                            presentation,
                            frame,
                            view,
                        )
                        .into_any_element(),
                    }
                } else if !presentation.objects.is_empty()
                    || (self.text_rows.is_some() && !presentation.font_scales.is_empty())
                {
                    let frame = frame.0.expect("inline rows have resolved geometry");
                    let limit = self.inline_width_limit(frame);
                    for (_, object) in &mut presentation.objects {
                        *object = object.constrained_to(frame.rem_size(), limit);
                    }
                    let elements = presentation
                        .objects
                        .iter()
                        .map(|(range, _)| {
                            let offset = display.start + range.start;
                            let source = self
                                .model
                                .display_to_source(offset, Affinity::After)
                                .unwrap();
                            let block = self
                                .model
                                .blocks
                                .iter()
                                .find(|block| {
                                    block.source().start == source && block.inline_size().is_some()
                                })
                                .expect("inline projection has a block");
                            (
                                range.clone(),
                                self.block_renderer.as_ref().expect("inline renderer")(
                                    block.id(),
                                    window,
                                    cx,
                                ),
                            )
                        })
                        .collect();
                    DocumentChild::Inline {
                        element: super::element::inline::InlineRow::new(
                            cx.entity(),
                            item_ix,
                            (
                                display.clone(),
                                self.model.display_text()[display.clone()].into(),
                            ),
                            presentation,
                            frame,
                            elements,
                            window,
                        )
                        .into_any_element(),
                    }
                } else {
                    DocumentChild::Text {
                        item_ix,
                        display: display.clone(),
                        // Rows hold no text; only the rows in view slice it.
                        text: self.model.display_text()[display.clone()].to_owned().into(),
                        presentation: Box::new(presentation),
                    }
                }
            }
            DocumentLayoutItem::Block { id, source, .. } => {
                let renderer = self
                    .block_renderer
                    .as_ref()
                    .expect("validated inline blocks require a renderer");
                DocumentChild::Block {
                    item_ix,
                    id: id.clone(),
                    source: source.clone(),
                    element: renderer(id, window, cx),
                }
            }
            DocumentLayoutItem::AnchoredBlock { id, source, .. } => {
                let renderer = self
                    .block_renderer
                    .as_ref()
                    .expect("validated anchored blocks require a renderer");
                DocumentChild::Block {
                    item_ix,
                    id: id.clone(),
                    source: *source..*source,
                    element: renderer(id, window, cx),
                }
            }
            DocumentLayoutItem::Trailer => DocumentChild::Trailer {
                item_ix,
                height: self
                    .trailer_height
                    .unwrap_or_else(|| window.viewport_size().height),
            },
        };
        let element = DocumentElement::render_child(cx.entity(), child);
        let element = match (&self.text_renderer, text_context) {
            (Some(renderer), Some(context)) => renderer(context, element, window, cx),
            _ => element,
        };
        #[cfg(debug_assertions)]
        if let Some(text_top) = text_top.filter(|_| !hidden) {
            return super::element::DocumentRowCheck::new(cx.entity(), item_ix, text_top, element)
                .into_any_element();
        }
        if hidden {
            // A hidden line keeps its text for positions but takes no space.
            return gpui::div()
                .h_0()
                .overflow_hidden()
                .child(element)
                .into_any_element();
        }
        element
    }
}

impl<I: Clone + Eq + Hash + 'static> EventEmitter<DocumentEvent<I>> for DocumentState<I> {}

impl<I: Clone + Eq + Hash + 'static> Focusable for DocumentState<I> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<I: Clone + Eq + Hash + 'static> EntityInputHandler for DocumentState<I> {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.model.text.slice(range).to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.model.selected_range()),
            reversed: self.model.selection_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.model
            .marked_range()
            .map(|range| self.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.model.set_marked_range(None);
        self.undo.finish_composition();
        self.pause_blink_cursor(cx);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let committing_composition = self.model.marked_range().is_some();
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.model.marked_range())
            .unwrap_or_else(|| self.model.selected_range());
        self.replace(
            range,
            text,
            if committing_composition {
                EditOrigin::Composition
            } else {
                EditOrigin::User
            },
            None,
            cx,
        );
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.model.marked_range())
            .unwrap_or_else(|| self.model.selected_range());
        let selected = new_selected_range_utf16.unwrap_or_else(|| {
            let end = text.encode_utf16().count();
            end..end
        });
        self.replace(range, text, EditOrigin::Composition, Some(selected), cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let source = self.range_from_utf16(&range_utf16);
        let start_affinity = if source.is_empty() {
            Affinity::After
        } else {
            Affinity::Before
        };
        let start_display = self.model.source_to_display(source.start, start_affinity)?;
        let end_display = self.model.source_to_display(source.end, Affinity::After)?;
        let start_item = self
            .text_item_for_display(start_display, start_affinity)
            .unwrap_or_else(|| self.layout_item_for_source(source.start));
        let end_item = self
            .text_item_for_display(end_display, Affinity::After)
            .unwrap_or_else(|| self.layout_item_for_source(source.end));
        let (start, line_height, _, _) = self
            .input_position_for_offset(source.start, start_affinity)
            .and_then(|position| self.text_position_for_position(&position))
            .or_else(|| {
                self.text_position_for_display_in_item(start_display, start_affinity, start_item)
            })?;
        let (end, _, _, _) = self
            .input_position_for_offset(source.end, Affinity::After)
            .and_then(|position| self.text_position_for_position(&position))
            .or_else(|| {
                self.text_position_for_display_in_item(end_display, Affinity::After, end_item)
            })?;
        // The platform accepts one rectangle, not a multiline union. For a
        // spanning range use the first caret, including soft-wrapped lines.
        let width = if source.is_empty() {
            Pixels::ZERO
        } else if end.y == start.y {
            (end.x - start.x).max(Pixels::ZERO)
        } else {
            px(1.)
        };
        Some(Bounds::from_corners(
            start,
            point(start.x + width, start.y + line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(
            self.model
                .text
                .offset_to_offset_utf16(self.offset_for_point(point)),
        )
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo.break_coalescing();
        self.preferred_x = None;
        let range = self.range_from_utf16(&range_utf16);
        if let (Some(anchor), Some(head)) = (
            self.input_position_for_offset(range.start, Affinity::After),
            self.input_position_for_offset(range.end, Affinity::Before),
        ) {
            self.model
                .replace_selection_positions(anchor, head)
                .expect("platform selection must resolve");
        } else {
            self.model.replace_selection(range.start, range.end);
        }
        self.pause_blink_cursor(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.model.text.len_utf16())
    }

    fn text_input_editable_range(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.model
            .editable_range_for_cursor()
            .map(|range| self.range_to_utf16(&range))
    }
}

impl<I: Clone + Eq + Hash + 'static> Render for DocumentState<I> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let entity = cx.entity();
        if !self.scroll_handler_installed {
            self.scroll_handler_installed = true;
            self.install_scroll_resolver(cx);
            let weak = entity.downgrade();
            self.list_state.set_scroll_handler(move |event, _, cx| {
                if !event.is_scrolled {
                    return;
                }
                let Some(entity) = weak.upgrade() else {
                    return;
                };
                entity.update(cx, |state, cx| state.list_scrolled(cx));
            });
        }
        let list_entity = entity.clone();
        let content = list(self.list_state.clone(), move |item_ix, window, cx| {
            list_entity.update(cx, |state, cx| {
                state.render_layout_item(item_ix, window, cx)
            })
        })
        .size_full()
        .into_any_element();
        let interaction_entity = entity.clone();
        let scrollbar = Scrollbar::vertical(&DocumentScrollbarHandle {
            list: self.list_state.clone(),
            input_pending: self.scrollbar_input_pending.clone(),
        });
        let interaction_layer = gpui::div()
            .absolute()
            .inset_0()
            .on_mouse_down(
                gpui::MouseButton::Left,
                window.listener_for(&interaction_entity, Self::mouse_down),
            )
            .cursor_text();
        div()
            .id("document-state")
            .relative()
            .key_context(DOCUMENT_INPUT_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(window.listener_for(&entity, Self::backspace))
            .on_action(window.listener_for(&entity, Self::delete))
            .on_action(window.listener_for(&entity, Self::delete_to_start_of_line))
            .on_action(window.listener_for(&entity, Self::delete_to_end_of_line))
            .on_action(window.listener_for(&entity, Self::delete_previous_word))
            .on_action(window.listener_for(&entity, Self::delete_next_word))
            .on_action(window.listener_for(&entity, Self::enter))
            .on_action(window.listener_for(&entity, Self::move_up))
            .on_action(window.listener_for(&entity, Self::move_down))
            .on_action(window.listener_for(&entity, Self::move_left))
            .on_action(window.listener_for(&entity, Self::move_right))
            .on_action(window.listener_for(&entity, Self::move_home))
            .on_action(window.listener_for(&entity, Self::move_end))
            .on_action(window.listener_for(&entity, Self::move_page_up))
            .on_action(window.listener_for(&entity, Self::move_page_down))
            .on_action(window.listener_for(&entity, Self::move_to_start_of_line))
            .on_action(window.listener_for(&entity, Self::move_to_end_of_line))
            .on_action(window.listener_for(&entity, Self::move_to_start))
            .on_action(window.listener_for(&entity, Self::move_to_end))
            .on_action(window.listener_for(&entity, Self::move_to_previous_word))
            .on_action(window.listener_for(&entity, Self::move_to_next_word))
            .on_action(window.listener_for(&entity, Self::select_up))
            .on_action(window.listener_for(&entity, Self::select_down))
            .on_action(window.listener_for(&entity, Self::select_left))
            .on_action(window.listener_for(&entity, Self::select_right))
            .on_action(window.listener_for(&entity, Self::select_to_start_of_line))
            .on_action(window.listener_for(&entity, Self::select_to_end_of_line))
            .on_action(window.listener_for(&entity, Self::select_to_start))
            .on_action(window.listener_for(&entity, Self::select_to_end))
            .on_action(window.listener_for(&entity, Self::select_to_previous_word))
            .on_action(window.listener_for(&entity, Self::select_to_next_word))
            .on_action(window.listener_for(&entity, Self::select_all))
            .on_action(window.listener_for(&entity, Self::copy))
            .on_action(window.listener_for(&entity, Self::cut))
            .on_action(window.listener_for(&entity, Self::paste))
            .on_action(window.listener_for(&entity, Self::undo))
            .on_action(window.listener_for(&entity, Self::redo))
            .size_full()
            .child(interaction_layer)
            .child(DocumentElement::new(entity, content))
            .child(scrollbar)
    }
}

#[cfg(test)]
mod readonly_composition_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Entity, FontWeight, IntoElement, ListOffset, Modifiers, MouseButton, ScrollDelta,
        ScrollWheelEvent, TestAppContext, VisualTestContext, point, px,
    };
    use std::cell::RefCell;

    use crate::{Theme, document::ProjectionSpan};

    struct DocumentRoot(Entity<DocumentState<&'static str>>);

    impl Render for DocumentRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    pub(super) fn document_view(
        cx: &mut TestAppContext,
    ) -> (Entity<DocumentState<&'static str>>, VisualTestContext) {
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(|cx| {
                    DocumentState::new(
                        "history",
                        vec![
                            DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 7..7, EditPolicy::Editable),
                        ],
                        cx,
                    )
                    .unwrap()
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        (
            document.unwrap(),
            VisualTestContext::from_window(window.into(), cx),
        )
    }

    fn model() -> DocumentModel<&'static str> {
        DocumentModel::new(
            "history",
            vec![
                DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                DocumentRegion::new("draft", 7..7, EditPolicy::Editable),
            ],
        )
        .unwrap()
    }

    #[test]
    fn structure_suffix_does_not_retain_an_empty_readonly_prefix() {
        // A send freezes its trailing empty text and creates a new draft at
        // the same offset. Only the latter belongs to subsequent user undo.
        let before = DocumentModel::new(
            "sent",
            vec![
                DocumentRegion::new("sent", 0..4, EditPolicy::Readonly),
                DocumentRegion::new("sent-tail", 4..4, EditPolicy::Readonly),
                DocumentRegion::new("draft", 4..4, EditPolicy::Editable),
            ],
        )
        .unwrap();
        let accepted = DocumentModel::new(
            "",
            vec![DocumentRegion::new("draft", 0..0, EditPolicy::Editable)],
        )
        .unwrap();
        assert_eq!(
            DocumentStructure::suffix(&before, 4),
            DocumentStructure::suffix(&accepted, 0)
        );
    }

    #[gpui::test]
    fn caret_respects_node_identity_selection_and_focus(cx: &mut TestAppContext) {
        let (document, mut view) = document_view(cx);
        view.update(|window, cx| {
            let focus = document.read(cx).focus_handle.clone();
            focus.focus(window, cx);
            window.activate_window();
            let _ = window.draw(cx);
        });
        view.run_until_parked();

        // The readonly end and empty draft share an offset, but not edit ownership.
        for (node, offset, expected) in [("history", 7, false), ("draft", 0, true)] {
            view.update(|window, cx| {
                document.update(cx, |document, cx| {
                    let position = DocumentPosition::new(node, offset, Affinity::After);
                    document
                        .set_selection_positions(position.clone(), position, cx)
                        .unwrap();
                    assert_eq!(document.show_cursor(window, cx), expected);
                });
            });
        }
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "draft", window, cx);
                document.set_selection(7..12, false, cx);
                assert!(!document.show_cursor(window, cx));
                assert_eq!(document.selected_range(), 7..12);
                document.set_selection(12..12, false, cx);
                assert!(document.show_cursor(window, cx));
            });
            let other = cx.focus_handle();
            other.focus(window, cx);
        });
        view.run_until_parked();
        view.update(|window, cx| {
            let state = document.read(cx);
            assert!(!state.show_cursor(window, cx));
            assert!(!state.blink_cursor.read(cx).visible());
            assert_eq!(state.text(), "historydraft");
            document.update(cx, |state, cx| {
                state.set_selection(12..12, false, cx);
                assert!(!state.blink_cursor.read(cx).visible());
            });
        });
    }

    #[gpui::test]
    fn host_stream_preserves_blink_phase_and_ime_keeps_caret_visible(cx: &mut TestAppContext) {
        let (document, mut view) = document_view(cx);
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_selection(7..7, false, cx);
                document.focus_handle.focus(window, cx);
            });
            window.activate_window();
            let _ = window.draw(cx);
        });
        view.run_until_parked();
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "a", window, cx);
                assert!(document.show_cursor(window, cx));
            });
        });
        view.run_until_parked();
        view.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        view.run_until_parked();
        view.update(|window, cx| {
            document.update(cx, |document, cx| {
                assert!(!document.show_cursor(window, cx));
                let transaction = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(0..0, "stream\n")],
                    8,
                )
                .unwrap();
                document
                    .apply_host_transaction(
                        transaction,
                        vec![
                            DocumentRegion::new("history", 0..14, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 14..15, EditPolicy::Editable),
                        ],
                        cx,
                    )
                    .unwrap();
                assert!(!document.show_cursor(window, cx));
                assert_eq!(document.selected_range(), 15..15);
                document.replace_and_mark_text_in_range(None, "你", Some(0..1), window, cx);
            });
        });
        view.run_until_parked();
        view.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        view.run_until_parked();
        view.update(|window, cx| {
            let state = document.read(cx);
            assert!(!state.blink_cursor.read(cx).visible());
            assert!(state.show_cursor(window, cx));
            assert_eq!(state.text(), "stream\nhistorya你");
        });
        view.simulate_keystrokes("left");
        assert!(document.read_with(&view, |state, cx| state.blink_cursor.read(cx).visible()));
    }

    #[test]
    fn unstyled_rows_hold_no_text_or_presentation_and_compare_by_content() {
        let model = DocumentModel::new(
            "same\nsame\nother",
            vec![DocumentRegion::new("text", 0..15, EditPolicy::Readonly)],
        )
        .unwrap();
        let items = model.layout_items(None);
        let DocumentLayoutItem::Text {
            presentation: None, ..
        } = &items[0]
        else {
            panic!("an unstyled row allocates no presentation");
        };
        // Identity follows content, not position, so shifted rows still match.
        assert!(items[0].same_layout_identity(&items[1]));
        assert!(!items[1].same_layout_identity(&items[2]));
    }

    #[test]
    fn insertion_expands_only_the_trailing_editable_region() {
        let mut model = model();
        let transaction = EditTransaction::new(
            model.revision,
            EditOrigin::User,
            vec![TextEdit::new(7..7, " draft")],
            7,
        )
        .unwrap();

        assert!(matches!(
            model.route_and_apply(&transaction, None),
            RoutingOutcome::Applied
        ));
        assert_eq!(model.text(), "history draft");
        assert_eq!(model.regions.as_slice()[0].range(), 0..7);
        assert_eq!(model.regions.as_slice()[1].range(), 7..13);
        assert_eq!(model.revision.value(), 1);
    }

    #[test]
    fn history_edit_is_rejected_without_mutation() {
        let mut model = model();
        let transaction = EditTransaction::new(
            model.revision,
            EditOrigin::User,
            vec![TextEdit::new(0..1, "H")],
            7,
        )
        .unwrap();

        assert!(matches!(
            model.route_and_apply(&transaction, None),
            RoutingOutcome::Rejected(DocumentEditRejection::Readonly)
        ));
        assert_eq!(model.text(), "history");
        assert_eq!(model.revision, DocumentRevision::INITIAL);
    }

    #[test]
    fn routed_region_returns_domain_id_without_mutation() {
        let mut model = DocumentModel::new(
            "old user",
            vec![DocumentRegion::new("resend", 0..8, EditPolicy::Routed)],
        )
        .unwrap();
        let transaction = EditTransaction::new(
            model.revision,
            EditOrigin::User,
            vec![TextEdit::new(3..4, "X")],
            8,
        )
        .unwrap();

        assert!(matches!(
            model.route_and_apply(&transaction, None),
            RoutingOutcome::Routed("resend")
        ));
        assert_eq!(model.text(), "old user");
    }

    #[test]
    fn anchor_bias_distinguishes_both_sides_of_an_insertion() {
        let range = 4..4;
        assert_eq!(transform_offset(4, AnchorBias::Left, &range, 3), 4);
        assert_eq!(transform_offset(4, AnchorBias::Right, &range, 3), 7);
    }

    #[test]
    fn anchored_row_keeps_one_history_region_and_separates_display_lines() {
        let mut model = DocumentModel::new(
            "first\nsecond",
            vec![DocumentRegion::new("history", 0..12, EditPolicy::Readonly)],
        )
        .unwrap();
        let row = AnchoredBlock::new(
            "comment",
            DocumentPosition::new("history", 6, Affinity::Before),
        );
        model
            .set_rich_presentation_with_anchors(
                DocumentProjection::identity(12),
                Vec::new(),
                vec![row.clone()],
                DocumentStyles::default(),
            )
            .unwrap();
        assert_eq!(model.text(), "first\nsecond");
        assert!(matches!(
            model.layout_items(None)[1],
            DocumentLayoutItem::AnchoredBlock {
                id: "comment",
                source: 6,
                ..
            }
        ));
        assert!(matches!(
            model.layout_items(None)[2],
            DocumentLayoutItem::Text { .. }
        ));
        model
            .replace_selection_positions(
                DocumentPosition::new("history", 1, Affinity::After),
                DocumentPosition::new("history", 9, Affinity::Before),
            )
            .unwrap();
        assert_eq!(model.selected_range(), 1..9);
        assert_eq!(
            model
                .position_for_anchor(model.selection.head())
                .unwrap()
                .node_id(),
            &"history"
        );
        assert_eq!(
            model.set_rich_presentation_with_anchors(
                DocumentProjection::identity(12),
                Vec::new(),
                vec![AnchoredBlock::new(
                    "comment",
                    DocumentPosition::new("history", 2, Affinity::Before)
                )],
                DocumentStyles::default(),
            ),
            Err(PresentationError::AnchoredBlocks(
                AnchoredBlockError::NotLineBoundary
            ))
        );
        assert_eq!(model.anchored_blocks, vec![row]);
    }

    #[test]
    fn anchored_rows_need_unique_ids_and_a_known_node() {
        let mut model = DocumentModel::new(
            "a\nb\n",
            vec![
                DocumentRegion::new("a", 0..2, EditPolicy::Readonly),
                DocumentRegion::new("b", 2..4, EditPolicy::Readonly),
            ],
        )
        .unwrap();
        let row =
            |id, node| AnchoredBlock::new(id, DocumentPosition::new(node, 0, Affinity::Before));
        let mut present = |rows| {
            model.set_rich_presentation_with_anchors(
                DocumentProjection::identity(4),
                Vec::new(),
                rows,
                DocumentStyles::default(),
            )
        };
        let duplicate = Err(PresentationError::AnchoredBlocks(
            AnchoredBlockError::DuplicateId,
        ));
        assert_eq!(present(vec![row("b", "a")]), duplicate);
        assert_eq!(present(vec![row("x", "a"), row("x", "b")]), duplicate);
        assert_eq!(
            present(vec![row("x", "missing")]),
            Err(PresentationError::AnchoredBlocks(
                AnchoredBlockError::InvalidPosition(PositionError::NodeNotFound)
            ))
        );
        assert_eq!(present(vec![row("y", "b"), row("x", "a")]), Ok(()));
        let rows: Vec<_> = model
            .layout_items(None)
            .into_iter()
            .filter_map(|item| match item {
                DocumentLayoutItem::AnchoredBlock { id, source, .. } => Some((id, source)),
                _ => None,
            })
            .collect();
        assert_eq!(rows, vec![("x", 0), ("y", 2)]);
    }

    #[test]
    fn anchored_rows_follow_mapped_markdown_lines_in_stable_order() {
        let mut model = DocumentModel::new(
            "**A**\n\nB",
            vec![DocumentRegion::new("history", 0..8, EditPolicy::Readonly)],
        )
        .unwrap();
        let projection = DocumentProjection::new(
            8,
            vec![
                ProjectionSpan::mapped(
                    0..7,
                    "A\n",
                    vec![
                        super::super::ProjectionMapping::new(0..1, 2..3),
                        super::super::ProjectionMapping::new(1..2, 3..7),
                    ],
                ),
                ProjectionSpan::mapped(
                    7..8,
                    "B\n",
                    vec![
                        super::super::ProjectionMapping::new(0..1, 7..8),
                        super::super::ProjectionMapping::new(1..2, 7..8),
                    ],
                ),
            ],
        )
        .unwrap();
        model
            .set_rich_presentation_with_anchors(
                projection,
                Vec::new(),
                ["first", "second"]
                    .map(|id| {
                        AnchoredBlock::new(
                            id,
                            DocumentPosition::new("history", 7, Affinity::Before),
                        )
                    })
                    .to_vec(),
                DocumentStyles::default(),
            )
            .unwrap();
        let items = model.layout_items(None);
        assert!(
            matches!(items[0], DocumentLayoutItem::Text { ref display, .. } if &model.display_text()[display.clone()] == "A")
        );
        assert!(matches!(
            items[1],
            DocumentLayoutItem::AnchoredBlock { id: "first", .. }
        ));
        assert!(matches!(
            items[2],
            DocumentLayoutItem::AnchoredBlock { id: "second", .. }
        ));
        assert!(
            matches!(items[3], DocumentLayoutItem::Text { ref display, .. } if &model.display_text()[display.clone()] == "B")
        );
    }

    #[gpui::test]
    fn anchored_rows_require_a_renderer_before_reset(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        document.update(&mut cx, |document, cx| {
            assert_eq!(
                document.reset(
                    DocumentSnapshot::new(
                        "history",
                        vec![DocumentRegion::new("history", 0..7, EditPolicy::Readonly)],
                        DocumentProjection::identity(7),
                        Vec::new(),
                        DocumentStyles::default(),
                    )
                    .anchored_blocks(vec![AnchoredBlock::new(
                        "note",
                        DocumentPosition::new("history", 7, Affinity::Before),
                    )]),
                    cx,
                ),
                Err(PresentationError::AnchoredBlocks(
                    AnchoredBlockError::MissingRenderer
                ))
            );
            assert_eq!(document.text(), "history");
        });
    }

    #[gpui::test]
    fn anchored_row_survives_host_refresh_without_changing_draft_or_selection(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_block_renderer(|_, _, _| div().h(px(45.)).into_any_element(), cx);
                document
                    .reset(
                        DocumentSnapshot::new(
                            "first\nsecond draft",
                            vec![
                                DocumentRegion::new("history", 0..12, EditPolicy::Readonly),
                                DocumentRegion::new("draft", 12..18, EditPolicy::Editable),
                            ],
                            DocumentProjection::identity(18),
                            Vec::new(),
                            DocumentStyles::default(),
                        )
                        .anchored_blocks(vec![AnchoredBlock::new(
                            "comment",
                            DocumentPosition::new("history", 6, Affinity::Before),
                        )]),
                        cx,
                    )
                    .unwrap();
                document
                    .set_selection_positions(
                        DocumentPosition::new("draft", 3, Affinity::After),
                        DocumentPosition::new("draft", 3, Affinity::After),
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
            document.update(cx, |document, cx| {
                assert!(
                    document
                        .block_layouts
                        .iter()
                        .any(|record| record.source == (6..6))
                );
                assert!(document.remeasure_block(&"comment", cx));
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
                            DocumentRegion::new("history", 0..16, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 16..22, EditPolicy::Editable),
                        ],
                        DocumentProjection::identity(22),
                        Vec::new(),
                        vec![AnchoredBlock::new(
                            "comment",
                            DocumentPosition::new("history", 10, Affinity::Before),
                        )],
                        DocumentStyles::default(),
                        cx,
                    )
                    .unwrap();
                assert_eq!(document.region_text(&"draft").as_deref(), Some(" draft"));
                assert_eq!(
                    document.selected_positions().unwrap().1,
                    DocumentPosition::new("draft", 3, Affinity::After)
                );
                assert_eq!(
                    document
                        .model
                        .layout_items(None)
                        .iter()
                        .filter(|item| matches!(item, DocumentLayoutItem::AnchoredBlock { .. }))
                        .count(),
                    1
                );
            });
        });
    }

    #[gpui::test]
    fn empty_middle_text_keeps_its_identity_through_input_host_updates_and_undo(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_block_renderer(|_, _, _| div().h(px(60.)).into_any_element(), cx);
                document
                    .reset(
                        DocumentSnapshot::new(
                            "h\n中\u{fffc}tail",
                            vec![
                                DocumentRegion::new("history", 0..2, EditPolicy::Readonly),
                                DocumentRegion::new("before", 2..5, EditPolicy::Editable),
                                DocumentRegion::new("object", 5..8, EditPolicy::Atomic),
                                DocumentRegion::new("after", 8..12, EditPolicy::Editable),
                            ],
                            DocumentProjection::new(12, vec![ProjectionSpan::hide(5..8)]).unwrap(),
                            vec![DocumentBlock::new("object", 5..8)],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_selection(2..5, false, cx);
                document.replace_text_in_range(None, "", window, cx);
                assert_eq!(document.region(&"before").unwrap().range(), 2..2);
                assert_eq!(
                    document.selected_positions().unwrap().1.node_id(),
                    &"before"
                );

                let host = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(0..0, "x\n")],
                    document.text().len(),
                )
                .unwrap();
                document
                    .apply_host_transaction_with_presentation(
                        host,
                        vec![
                            DocumentRegion::new("history", 0..4, EditPolicy::Readonly),
                            DocumentRegion::new("before", 4..4, EditPolicy::Editable),
                            DocumentRegion::new("object", 4..7, EditPolicy::Atomic),
                            DocumentRegion::new("after", 7..11, EditPolicy::Editable),
                        ],
                        DocumentProjection::new(11, vec![ProjectionSpan::hide(4..7)]).unwrap(),
                        vec![DocumentBlock::new("object", 4..7)],
                        cx,
                    )
                    .unwrap();
                assert_eq!(document.selected_range(), 4..4);
                assert_eq!(
                    document.selected_positions().unwrap().1.node_id(),
                    &"before"
                );
                document.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
                document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                document.replace_text_in_range(None, "你", window, cx);
                assert_eq!(document.text(), "x\nh\n你\u{fffc}tail");
                assert_eq!(document.model.blocks[0].source(), 7..10);
                document.undo(&Undo, window, cx);
                assert_eq!(document.region_text(&"before").as_deref(), Some(""));
                document.undo(&Undo, window, cx);
                assert_eq!(document.region_text(&"before").as_deref(), Some("中"));
                document.redo(&Redo, window, cx);
                assert_eq!(document.region_text(&"before").as_deref(), Some(""));
                document.redo(&Redo, window, cx);
                assert_eq!(document.region_text(&"before").as_deref(), Some("你"));
                assert_eq!(document.region_text(&"after").as_deref(), Some("tail"));
            });
        });
    }

    #[gpui::test]
    fn empty_text_rows_keep_pointer_caret_and_ime_ownership_between_objects(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .set_block_renderer(|_, _, _| div().h(px(50.)).w_full().into_any_element(), cx);
                document
                    .reset(
                        DocumentSnapshot::new(
                            "\u{fffc}\u{fffc}tail",
                            vec![
                                DocumentRegion::new("left", 0..0, EditPolicy::Editable),
                                DocumentRegion::new("first", 0..3, EditPolicy::Atomic),
                                DocumentRegion::new("middle", 3..3, EditPolicy::Editable),
                                DocumentRegion::new("second", 3..6, EditPolicy::Atomic),
                                DocumentRegion::new("right", 6..10, EditPolicy::Editable),
                            ],
                            DocumentProjection::new(
                                10,
                                vec![ProjectionSpan::hide(0..3), ProjectionSpan::hide(3..6)],
                            )
                            .unwrap(),
                            vec![
                                DocumentBlock::new("first", 0..3),
                                DocumentBlock::new("second", 3..6),
                            ],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
        });
        for id in ["left", "middle"] {
            let click = document.read_with(&cx, |document, _| {
                let position = DocumentPosition::new(id, 0, Affinity::After);
                let (_, line_height, _, bounds) =
                    document.text_position_for_position(&position).unwrap();
                point(bounds.left() + px(1.), bounds.top() + line_height / 2.)
            });
            cx.simulate_click(click, Modifiers::default());
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    assert_eq!(document.selected_positions().unwrap().1.node_id(), &id);
                    let item = document
                        .text_item_for_position(&DocumentPosition::new(id, 0, Affinity::After))
                        .unwrap();
                    let painted = document
                        .text_layouts
                        .iter()
                        .filter(|record| {
                            document
                                .segment_paint_snapshot(record.item_ix, &record.display)
                                .2
                                .is_some()
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(painted.len(), 1);
                    assert_eq!(painted[0].item_ix, item);
                    let expected = painted[0].layout.position_for_index(0).unwrap();
                    let utf16 = document
                        .model
                        .text
                        .offset_to_offset_utf16(document.model.cursor());
                    let bounds = document
                        .bounds_for_range(utf16..utf16, document.last_bounds.unwrap(), window, cx)
                        .unwrap();
                    assert_eq!(bounds.origin, expected);
                });
            });
            cx.simulate_keystrokes("x y backspace backspace");
            document.read_with(&cx, |document, _| {
                assert_eq!(document.region_text(&id).as_deref(), Some(""));
                assert_eq!(document.selected_positions().unwrap().1.node_id(), &id);
                assert_eq!(document.text(), "\u{fffc}\u{fffc}tail");
                validate_blocks(
                    &document.model.blocks,
                    &document.model.regions,
                    &document.model.projection,
                    document.model.text.len(),
                )
                .unwrap();
            });
        }
    }

    #[gpui::test]
    fn platform_input_edits_only_the_editable_region(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, " draft", window, cx);
                assert_eq!(document.text(), "history draft");
                assert_eq!(document.regions()[0].range(), 0..7);
                assert_eq!(document.regions()[1].range(), 7..13);

                document.set_selection(0..0, false, cx);
                document.replace_text_in_range(None, "X", window, cx);
                assert_eq!(document.text(), "history draft");
            });
        });
    }

    #[gpui::test]
    fn an_edit_crossing_readonly_and_editable_regions_is_atomic(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "draft", window, cx);
                document.set_selection(6..8, false, cx);
                document.replace_text_in_range(None, "X", window, cx);
                assert_eq!(document.text(), "historydraft");
                assert_eq!(document.selected_range(), 6..8);
            });
        });
    }

    #[gpui::test]
    fn rejected_and_routed_ime_edits_preserve_composition_and_undo(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        for policy in [EditPolicy::Readonly, EditPolicy::Routed] {
            for refining in [false, true] {
                cx.update(|window, cx| {
                    document.update(cx, |document, cx| {
                        document
                            .reset(
                                DocumentSnapshot::new(
                                    "history",
                                    vec![
                                        DocumentRegion::new("history", 0..7, policy),
                                        DocumentRegion::new("draft", 7..7, EditPolicy::Editable),
                                    ],
                                    DocumentProjection::identity(7),
                                    vec![],
                                    DocumentStyles::default(),
                                ),
                                cx,
                            )
                            .unwrap();
                        document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                        let revision = document.revision();
                        let selection = document.selected_positions().unwrap();
                        if refining {
                            document.replace_and_mark_text_in_range(
                                Some(0..1),
                                "x",
                                Some(1..1),
                                window,
                                cx,
                            );
                        } else {
                            document.replace_text_in_range(Some(0..1), "x", window, cx);
                        }
                        assert_eq!(document.revision(), revision);
                        assert_eq!(document.selected_positions().unwrap(), selection);
                        assert_eq!(document.marked_text_range(window, cx), Some(7..9));
                        document.replace_text_in_range(None, "你", window, cx);
                        assert_eq!(document.text(), "history你");
                        document.undo(&Undo, window, cx);
                        assert_eq!(document.text(), "history");
                        document.redo(&Redo, window, cx);
                        assert_eq!(document.text(), "history你");
                    })
                });
            }
        }
    }

    #[gpui::test]
    fn public_user_transactions_record_undo_and_reject_internal_origins(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "abcdef", window, cx);
                // Public transactions also work when the selection is in another node.
                document.set_selection(0..2, false, cx);
                let selection = document.selected_positions().unwrap();
                let transaction = EditTransaction::new(
                    document.revision(),
                    EditOrigin::User,
                    vec![TextEdit::new(8..9, "中"), TextEdit::new(11..12, "🙂")],
                    document.text().len(),
                )
                .unwrap();
                assert_eq!(
                    document.apply_transaction(transaction, cx),
                    EditDecision::Apply
                );
                assert_eq!(document.text(), "historya中cd🙂f");
                assert_eq!(document.selected_range(), 17..17);
                assert!(matches!(
                    document.viewport.reveal,
                    Some(viewport::Reveal::Caret)
                ));
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "historyabcdef");
                assert_eq!(document.selected_positions().unwrap(), selection);
                document.redo(&Redo, window, cx);
                assert_eq!(document.text(), "historya中cd🙂f");
                for origin in [EditOrigin::Host, EditOrigin::Undo, EditOrigin::Redo] {
                    let revision = document.revision();
                    let transaction = EditTransaction::new(
                        revision,
                        origin,
                        vec![TextEdit::new(7..8, "x")],
                        document.text().len(),
                    )
                    .unwrap();
                    assert_eq!(
                        document.apply_transaction(transaction, cx),
                        EditDecision::Reject
                    );
                    assert_eq!(document.revision(), revision);
                    assert_eq!(document.text(), "historya中cd🙂f");
                }
                document.undo(&Undo, window, cx);
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "history");
            })
        });
    }

    #[gpui::test]
    fn structural_transaction_rejects_history_changes_without_touching_undo(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "alpha", window, cx);
                let revision = document.revision();
                let original = document.selected_positions().unwrap();
                let malformed = DocumentSnapshot::new(
                    "changedalpha",
                    vec![
                        DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 7..12, EditPolicy::Editable),
                    ],
                    DocumentProjection::identity(12),
                    vec![],
                    DocumentStyles::default(),
                );
                assert!(matches!(
                    document.apply_structure_transaction(malformed, 7, cx),
                    Err(StructureError::PrefixChanged)
                ));
                assert_eq!(document.revision(), revision);
                assert_eq!(document.selected_positions().unwrap(), original);
                assert_eq!(document.text(), "historyalpha");
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "history");
            });
        });
    }

    #[gpui::test]
    fn reset_drops_old_pins_and_rejects_old_transactions(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                let stale = EditTransaction::new(
                    document.revision(),
                    EditOrigin::User,
                    vec![TextEdit::new(0..3, "STALE")],
                    document.text().len(),
                )
                .unwrap();
                document
                    .pin_scroll(
                        "pin",
                        &DocumentPosition::new("draft", 0, Affinity::After),
                        0.5,
                        cx,
                    )
                    .unwrap();
                document.scrollbar_input_pending.set(true);
                document
                    .reset(
                        DocumentSnapshot::new(
                            "new",
                            vec![DocumentRegion::new("new", 0..3, EditPolicy::Editable)],
                            DocumentProjection::identity(3),
                            vec![],
                            DocumentStyles::default(),
                        )
                        .selection(DocumentPosition::new(
                            "new",
                            2,
                            Affinity::After,
                        )),
                        cx,
                    )
                    .unwrap();
                assert!(!document.unpin_scroll(&"pin", cx));
                assert_eq!(document.selected_range(), 2..2);
                document
                    .pin_scroll(
                        "new-pin",
                        &DocumentPosition::new("new", 1, Affinity::After),
                        0.5,
                        cx,
                    )
                    .unwrap();
                assert_eq!(document.selected_range(), 2..2);
                assert_eq!(document.pinned_scroll_id(), Some(&"new-pin"));
                assert_eq!(document.apply_transaction(stale, cx), EditDecision::Reject);
                assert_eq!(document.text(), "new");
            })
        });
    }

    #[gpui::test]
    fn undo_retains_only_changes_in_a_large_draft(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                let initial = 1024 * 1024;
                document
                    .reset(
                        DocumentSnapshot::new(
                            "a".repeat(initial),
                            vec![DocumentRegion::new(
                                "draft",
                                0..initial,
                                EditPolicy::Editable,
                            )],
                            DocumentProjection::identity(initial),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_selection(initial..initial, false, cx);
                for _ in 0..500 {
                    document.replace_text_in_range(None, "x", window, cx);
                }
                assert_eq!(document.undo.undo.len(), 1);
                assert_eq!(document.undo.undo[0].bytes(), 500);
                document.undo(&Undo, window, cx);
                assert_eq!(document.model.text.len(), initial);
                assert_eq!(document.selected_range(), initial..initial);
                document.redo(&Redo, window, cx);
                assert_eq!(document.model.text.len(), initial + 500);

                // An explicit selection boundary and a deletion burst stay separate.
                document.set_selection(initial + 500..initial + 500, false, cx);
                document.backspace(&Backspace, window, cx);
                document.backspace(&Backspace, window, cx);
                document.undo(&Undo, window, cx);
                assert_eq!(document.model.text.len(), initial + 500);
                document.undo(&Undo, window, cx);
                assert_eq!(document.model.text.len(), initial);
            })
        });
    }

    #[gpui::test]
    fn composition_range_changes_undo_as_one_atomic_record(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "ab", window, cx);
                document.set_selection(8..8, false, cx);
                document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                document.replace_and_mark_text_in_range(Some(7..10), "hao", Some(3..3), window, cx);
                document.replace_text_in_range(None, "好", window, cx);
                assert_eq!(document.text(), "history好b");
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "historyab");
                assert_eq!(document.selected_range(), 8..8);
                document.redo(&Redo, window, cx);
                assert_eq!(document.text(), "history好b");
                document.undo(&Undo, window, cx);
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "history");
            })
        });
    }

    #[test]
    fn undo_budget_evicts_oldest_records_and_keeps_recent_edits() {
        let mut history = DocumentUndoManager::default();
        for text in ["a", "bb", "ccc", "dddd"] {
            history.break_coalescing();
            history.record(
                UndoRecord {
                    sequence: 0,
                    kind: UndoKind::Text {
                        region_id: "draft",
                        changes: vec![UndoChange {
                            range: 0..0,
                            removed: String::new(),
                            inserted: text.into(),
                        }],
                    },
                    selection_before: None,
                    selection_after: None,
                },
                EditOrigin::Paste,
            );
        }
        history.trim(3, 7);
        assert_eq!(
            history
                .undo
                .iter()
                .map(UndoRecord::bytes)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        history.trim(1, 7);
        assert!(
            matches!(&history.undo[0].kind, UndoKind::Text { changes, .. } if changes[0].inserted == "dddd")
        );
        history.trim(1, 3);
        assert!(history.undo.is_empty());
        let mut reserved = String::with_capacity(1024);
        reserved.push('x');
        history.record(
            UndoRecord {
                sequence: 0,
                kind: UndoKind::Text {
                    region_id: "draft",
                    changes: vec![UndoChange {
                        range: 0..0,
                        removed: String::new(),
                        inserted: reserved,
                    }],
                },
                selection_before: None,
                selection_after: None,
            },
            EditOrigin::Paste,
        );
        history.trim(1, 16);
        assert!(
            history.undo.is_empty(),
            "the budget must include reserved string capacity"
        );
    }

    #[gpui::test]
    fn invalid_utf8_ranges_are_rejected_without_changing_document_state(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                let regions = || {
                    vec![
                        DocumentRegion::new("history", 0..3, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 3..7, EditPolicy::Editable),
                    ]
                };
                document
                    .reset(
                        DocumentSnapshot::new(
                            "中🙂",
                            regions(),
                            DocumentProjection::identity(7),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                let revision = document.revision();
                let selection = document.selected_positions().unwrap();
                assert!(matches!(
                    document.set_projection(
                        DocumentProjection::new(7, vec![ProjectionSpan::hide(1..2)]).unwrap(),
                        cx
                    ),
                    Err(ProjectionError::InvalidBoundary(_))
                ));
                let invalid = DocumentPosition::new("draft", 1, Affinity::After);
                assert!(matches!(
                    document.set_selection_positions(invalid.clone(), invalid, cx),
                    Err(PositionError::InvalidBoundary { .. })
                ));
                assert!(matches!(
                    document.position_for_offset(1, Affinity::After),
                    Err(PositionError::InvalidBoundary { .. })
                ));
                let user = EditTransaction::new(
                    revision,
                    EditOrigin::User,
                    vec![TextEdit::new(4..5, "x")],
                    7,
                )
                .unwrap();
                assert_eq!(document.apply_transaction(user, cx), EditDecision::Reject);
                let host = EditTransaction::new(
                    revision,
                    EditOrigin::Host,
                    vec![TextEdit::new(0..1, "x")],
                    7,
                )
                .unwrap();
                assert!(matches!(
                    document.apply_host_transaction(host, regions(), cx),
                    Err(HostTransactionError::InvalidEditRange(_))
                ));
                let host = EditTransaction::new(
                    revision,
                    EditOrigin::Host,
                    vec![TextEdit::new(0..0, "!")],
                    7,
                )
                .unwrap();
                assert!(matches!(
                    document.apply_host_transaction(
                        host,
                        vec![
                            DocumentRegion::new("history", 0..5, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 5..8, EditPolicy::Editable)
                        ],
                        cx
                    ),
                    Err(HostTransactionError::InvalidRegions(
                        RegionError::InvalidBoundary { .. }
                    ))
                ));
                assert!(matches!(
                    DocumentModel::new(
                        "中",
                        vec![DocumentRegion::new("bad", 1..3, EditPolicy::Editable)]
                    ),
                    Err(RegionError::InvalidBoundary { .. })
                ));
                assert_eq!(document.revision(), revision);
                assert_eq!(document.text(), "中🙂");
                assert_eq!(document.selected_positions().unwrap(), selection);
                assert!(document.undo.undo.is_empty());
            })
        });
    }

    #[gpui::test]
    fn styles_follow_edits_and_undo_before_readonly_text(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            "x\nHead\n",
                            vec![
                                DocumentRegion::new("draft", 0..2, EditPolicy::Editable),
                                DocumentRegion::new("history", 2..7, EditPolicy::Readonly),
                            ],
                            DocumentProjection::identity(7),
                            vec![],
                            DocumentStyles::new(
                                vec![super::super::DocumentParagraphStyle::new(
                                    2..7,
                                    TextStyleRefinement::default(),
                                )],
                                vec![super::super::DocumentInlineStyle::new(
                                    2..6,
                                    HighlightStyle {
                                        font_weight: Some(FontWeight::BOLD),
                                        ..Default::default()
                                    },
                                )],
                            ),
                        ),
                        cx,
                    )
                    .unwrap();
                document.replace_text_in_range(None, "zz\n", window, cx);
                assert_eq!(document.model.styles.inline()[0].source(), 5..9);
                assert_eq!(document.model.resolved_styles.inline[0].display, 5..9);
                assert_eq!(document.model.styles.paragraphs()[0].source(), 5..10);
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "x\nHead\n");
                assert_eq!(document.model.styles.inline()[0].source(), 2..6);
                document.redo(&Redo, window, cx);
                assert_eq!(document.model.styles.inline()[0].source(), 5..9);
                let revision = document.revision();
                // Removing the paragraph boundary cannot leave partially updated styles.
                document.replace_text_in_range(Some(4..5), "", window, cx);
                assert_eq!(document.text(), "zz\nx\nHead\n");
                assert_eq!(document.revision(), revision);
                assert_eq!(document.model.styles.inline()[0].source(), 5..9);
            })
        });
    }

    #[gpui::test]
    fn vertical_navigation_keeps_the_column_across_short_lines(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            "abcdef\nx\nabcdef",
                            vec![DocumentRegion::new("draft", 0..15, EditPolicy::Editable)],
                            DocumentProjection::identity(15),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_selection(5..5, false, cx);
                document.focus_handle(cx).focus(window, cx);
            });
            let _ = window.draw(cx);
        });
        for (keys, expected) in [
            ("down down", 14..14),
            ("up up", 5..5),
            ("down left down", 9..9),
        ] {
            cx.simulate_keystrokes(keys);
            document.read_with(&cx, |document, _| {
                assert_eq!(document.selected_range(), expected)
            });
        }
        document.update(&mut cx, |document, cx| {
            document.set_selection(5..5, false, cx);
            let cursor = document.selected_positions().unwrap().1;
            let (position, _, _, _) = document.text_position_for_position(&cursor).unwrap();
            // Native glyph positions are fractional; converting absolute x back
            // to local coordinates may put it just past the final glyph start.
            document.preferred_x = Some(position.x + px(0.01));
        });
        cx.simulate_keystrokes("down down");
        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), 14..14)
        });
        document.update(&mut cx, |document, cx| {
            document.set_selection(5..5, false, cx)
        });
        cx.simulate_keystrokes("shift-down shift-down");
        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), 5..14)
        });
    }

    #[gpui::test]
    fn multiline_ime_bounds_return_a_valid_first_caret(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            "abcdef\nx",
                            vec![DocumentRegion::new("draft", 0..8, EditPolicy::Editable)],
                            DocumentProjection::identity(8),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
            document.update(cx, |document, cx| {
                let bounds = document.last_bounds.unwrap();
                let caret = document.bounds_for_range(4..4, bounds, window, cx).unwrap();
                let spanning = document.bounds_for_range(4..8, bounds, window, cx).unwrap();
                assert_eq!(spanning.origin, caret.origin);
                assert_eq!(spanning.size.height, caret.size.height);
                assert!(spanning.size.width > Pixels::ZERO);
                assert!(spanning.size.height > Pixels::ZERO);
            });
        });
    }

    #[gpui::test]
    fn ime_refinement_replaces_one_marked_range(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
                document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                document.replace_text_in_range(None, "你", window, cx);

                assert_eq!(document.text(), "history你");
                assert_eq!(document.marked_text_range(window, cx), None);
                assert_eq!(document.selected_range(), 10..10);
                assert_eq!(document.text_input_editable_range(window, cx), Some(7..8));
            });
        });
    }

    #[gpui::test]
    fn boundary_caret_has_one_owner_and_matching_ime_geometry(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        for text in [
            String::new(),
            "history".into(),
            "# title\nbody\n".into(),
            format!("{}\ndraft", "wrapped text ".repeat(100)),
        ] {
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    let end = text.len();
                    let spans = if text.starts_with('#') {
                        vec![ProjectionSpan::hide(0..2)]
                    } else {
                        vec![]
                    };
                    document
                        .reset(
                            DocumentSnapshot::new(
                                text.clone(),
                                vec![
                                    DocumentRegion::new("history", 0..end, EditPolicy::Readonly),
                                    DocumentRegion::new("draft", end..end, EditPolicy::Editable),
                                ],
                                DocumentProjection::new(end, spans).unwrap(),
                                vec![],
                                DocumentStyles::default(),
                            ),
                            cx,
                        )
                        .unwrap();
                });
                let _ = window.draw(cx);
                document.update(cx, |document, cx| {
                    if text == "# title\nbody\n" {
                        for record in &document.text_layouts {
                            assert_eq!(record.bounds.size.height, record.layout.line_height());
                        }
                    }
                    let offsets = document
                        .text_layouts
                        .iter()
                        .flat_map(|record| [record.display.start, record.display.end])
                        .collect::<Vec<_>>();
                    for display in offsets {
                        let source = document
                            .model
                            .display_to_source(display, Affinity::After)
                            .unwrap();
                        document.set_selection(source..source, false, cx);
                        let painted = document
                            .text_layouts
                            .iter()
                            .filter_map(|record| {
                                document
                                    .segment_paint_snapshot(record.item_ix, &record.display)
                                    .2
                                    .map(|cursor| (record, cursor))
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(painted.len(), 1, "display boundary {display} in {text:?}");
                        let (record, cursor) = painted[0];
                        let position = record.layout.position_for_index(cursor).unwrap();
                        let utf16 = document.model.text.offset_to_offset_utf16(source);
                        let bounds = document
                            .bounds_for_range(
                                utf16..utf16,
                                document.last_bounds.unwrap(),
                                window,
                                cx,
                            )
                            .unwrap();
                        assert_eq!(bounds.origin, position);
                        assert_eq!(bounds.size.width, Pixels::ZERO);
                    }
                    // Soft wraps live inside one text layout; they use the same glyph
                    // position for both caret paint and the platform input rectangle.
                    if text.starts_with("wrapped") {
                        let record = &document.text_layouts[0];
                        let origin = record.layout.position_for_index(0).unwrap();
                        let wrap = (1..record.display.end)
                            .find(|ix| {
                                record
                                    .layout
                                    .position_for_index(*ix)
                                    .is_some_and(|p| p.y > origin.y)
                            })
                            .expect("the long paragraph should wrap");
                        let position = record.layout.position_for_index(wrap).unwrap();
                        document.set_selection(wrap..wrap, false, cx);
                        assert_eq!(
                            document.segment_paint_snapshot(0, &(0..text.len())).2,
                            Some(wrap)
                        );
                        let bounds = document
                            .bounds_for_range(wrap..wrap, document.last_bounds.unwrap(), window, cx)
                            .unwrap();
                        assert_eq!(bounds.origin, position);
                    }
                });
            });
        }
    }

    #[gpui::test]
    fn ime_candidate_bounds_follow_the_editable_caret(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            document.update(cx, |document, cx| {
                let bounds = document.last_bounds.unwrap();
                let before = document
                    .bounds_for_range(7..7, bounds, window, cx)
                    .expect("the editable caret should be laid out");
                document.replace_and_mark_text_in_range(None, "nihao", Some(5..5), window, cx);
                let marked = document
                    .bounds_for_range(12..12, bounds, window, cx)
                    .expect("marked text should retain the previous caret bounds until layout");
                assert_eq!(marked, before);
            });
            let _ = window.draw(cx);
            document.update(cx, |document, cx| {
                let bounds = document.last_bounds.unwrap();
                let drawn = document
                    .bounds_for_range(12..12, bounds, window, cx)
                    .expect("marked text should have precise bounds after layout");
                assert!(drawn.origin.x > bounds.left());
            });
        });
    }

    #[gpui::test]
    fn document_element_accepts_typing_after_a_blank_surface_click(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let click = point(px(300.), px(180.));
        cx.simulate_mouse_down(click, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(click, MouseButton::Left, Modifiers::default());
        cx.simulate_keystrokes("x");

        document.read_with(&cx, |document, _| {
            assert_eq!(document.text(), "historyx");
            assert_eq!(document.selected_range(), 8..8);
        });
    }

    #[gpui::test]
    fn clicking_link_text_reports_its_target_unless_it_selects(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let clicked = Rc::new(RefCell::new(Vec::new()));
        let observed = clicked.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&document, move |_, event, _| {
                if let DocumentEvent::LinkClicked(target) = event {
                    observed.borrow_mut().push(target.clone());
                }
            })
        });
        let glyph = |offset: usize, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
                document.update(cx, |document, cx| {
                    let bounds = document.last_bounds.unwrap();
                    let start = document
                        .bounds_for_range(offset..offset, bounds, window, cx)
                        .unwrap();
                    let end = document
                        .bounds_for_range(offset + 1..offset + 1, bounds, window, cx)
                        .unwrap();
                    point(
                        (start.left() + end.left()) / 2.,
                        start.top() + start.size.height / 2.,
                    )
                })
            })
        };
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                document
                    .set_node_inline_styles(
                        &"history",
                        vec![
                            super::super::DocumentInlineStyle::new(2..3, Default::default())
                                .with_link("https://example.com")
                                .with_link_hover_color(gpui::blue()),
                            super::super::DocumentInlineStyle::new(
                                3..5,
                                HighlightStyle {
                                    font_weight: Some(gpui::FontWeight::BOLD),
                                    ..Default::default()
                                },
                            )
                            .with_link("https://example.com")
                            .with_link_hover_color(gpui::blue()),
                        ],
                        cx,
                    )
                    .unwrap();
            });
        });

        let inside = glyph(3, &mut cx);
        cx.simulate_mouse_move(inside, None, Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.hovered_link, Some(2..5));
        });
        cx.simulate_mouse_down(inside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(inside, MouseButton::Left, Modifiers::default());
        assert_eq!(
            *clicked.borrow(),
            vec![SharedString::from("https://example.com")]
        );

        let outside = glyph(0, &mut cx);
        cx.simulate_mouse_move(outside, None, Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.hovered_link, None);
        });
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());

        // Selecting the link's text by dragging across it opens nothing.
        let (start, end) = (glyph(2, &mut cx), glyph(4, &mut cx));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert!(!document.selected_range().is_empty());
        });
        assert_eq!(clicked.borrow().len(), 1);
    }

    #[gpui::test]
    fn document_element_handles_document_and_selection_navigation(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            document.read(cx).focus_handle(cx).focus(window, cx);
        });

        let keys = if cfg!(target_os = "macos") {
            "cmd-down shift-left cmd-shift-up"
        } else {
            "ctrl-end shift-left ctrl-shift-home"
        };
        cx.simulate_keystrokes(keys);

        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), 0..7);
            assert_eq!(document.model.selection_offsets(), (7, 0));
        });
    }

    #[gpui::test]
    fn document_boundary_navigation_reaches_empty_text_regions(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let keys = if cfg!(target_os = "macos") {
            ["cmd-down", "cmd-shift-down", "cmd-up", "cmd-shift-up"]
        } else {
            ["ctrl-end", "ctrl-shift-end", "ctrl-home", "ctrl-shift-home"]
        };
        for (key, (target, extend, expected)) in keys.into_iter().zip([
            ("after", false, "historyx"),
            ("after", true, "history"),
            ("before", false, "xhistory"),
            ("before", true, "history"),
        ]) {
            let initial = DocumentPosition::new("history", 3, Affinity::After);
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    document
                        .reset(
                            DocumentSnapshot::new(
                                "history",
                                vec![
                                    DocumentRegion::new("before", 0..0, EditPolicy::Editable),
                                    DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                                    DocumentRegion::new("after", 7..7, EditPolicy::Editable),
                                ],
                                DocumentProjection::identity(7),
                                Vec::new(),
                                DocumentStyles::default(),
                            )
                            .selection(initial.clone()),
                            cx,
                        )
                        .unwrap();
                    document.focus_handle(cx).focus(window, cx);
                });
                let _ = window.draw(cx);
            });
            cx.simulate_keystrokes(key);
            document.read_with(&cx, |document, _| {
                let (anchor, head) = document.selected_positions().unwrap();
                assert_eq!(head.node_id(), &target, "{key}");
                assert_eq!(head.offset(), 0, "{key}");
                assert_eq!(anchor, if extend { initial } else { head }, "{key}");
            });
            cx.simulate_keystrokes("x");
            document.read_with(&cx, |document, _| {
                assert_eq!(document.text(), expected, "{key}");
            });
        }
    }

    #[gpui::test]
    fn dynamic_trailer_fills_the_viewport_and_routes_blank_clicks_to_eof(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_dynamic_trailer(true, cx);
            });
            let _ = window.draw(cx);
            let _ = window.draw(cx);
        });

        let click = document.read_with(&cx, |document, _| {
            assert!(document.has_dynamic_trailer());
            assert_eq!(document.list_state.item_count(), 2);
            let (_, bounds) = document.trailer_layout.expect("trailer must be visible");
            assert_eq!(
                bounds.size.height,
                document.list_state.viewport_bounds().size.height
            );
            bounds.center()
        });
        cx.simulate_mouse_down(click, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(click, MouseButton::Left, Modifiers::default());
        cx.simulate_keystrokes("x");

        document.read_with(&cx, |document, _| {
            assert_eq!(document.text(), "historyx");
            assert_eq!(document.selected_range(), 8..8);
        });
    }

    #[gpui::test]
    fn local_host_splices_match_full_presentation_and_preserve_composition(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        let fragment = |id, text: &str| {
            DocumentSnapshot::new(
                text,
                vec![DocumentRegion::new(id, 0..text.len(), EditPolicy::Readonly)],
                DocumentProjection::identity(text.len()),
                vec![],
                DocumentStyles::default(),
            )
        };
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            "first\nlast\n",
                            vec![
                                DocumentRegion::new("first", 0..6, EditPolicy::Readonly),
                                DocumentRegion::new("last", 6..11, EditPolicy::Readonly),
                                DocumentRegion::new("draft", 11..11, EditPolicy::Editable),
                            ],
                            DocumentProjection::identity(11),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_selection(11..11, false, cx);
                document.replace_text_in_range(None, "draft", window, cx);
                document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                for (first, end, replacement) in [
                    ("first", "last", fragment("first", "中文🙂\nfirst\n")),
                    ("last", "last", fragment("middle", "middle\n")),
                    (
                        "middle",
                        "last",
                        DocumentSnapshot::new(
                            "",
                            vec![],
                            DocumentProjection::identity(0),
                            vec![],
                            DocumentStyles::default(),
                        ),
                    ),
                    ("last", "draft", fragment("last", "last & tail\n")),
                ] {
                    document
                        .apply_host_splice(
                            document.revision(),
                            Some(&first),
                            Some(&end),
                            replacement,
                            cx,
                        )
                        .unwrap();
                    assert_eq!(document.layout_items, document.model.layout_items(None));
                    let rebuilt =
                        ProjectionMap::new(&document.model.text, &document.model.projection);
                    assert_eq!(document.display_text(), rebuilt.display_text());
                    let text = document.text();
                    for (offset, _) in text.char_indices() {
                        for affinity in [Affinity::Before, Affinity::After] {
                            assert_eq!(
                                document.model.source_to_display(offset, affinity),
                                rebuilt.source_to_display(offset, affinity)
                            );
                        }
                    }
                    let end = text.len();
                    let utf16_end = text.encode_utf16().count();
                    assert_eq!(
                        document.marked_text_range(window, cx),
                        Some(utf16_end - 2..utf16_end)
                    );
                    assert_eq!(document.selected_range(), end..end);
                }
                document.replace_text_in_range(None, "你", window, cx);
                document.undo(&Undo, window, cx);
                assert!(document.text().ends_with("draft"));
                document.undo(&Undo, window, cx);
                assert!(document.text().ends_with("last & tail\n"));
                document.redo(&Redo, window, cx);
                document.redo(&Redo, window, cx);
                assert!(document.text().ends_with("draft你"));
            });
        });
    }

    #[gpui::test]
    fn local_host_splice_rejects_invalid_seams_without_publishing(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        document.update(&mut cx, |document, cx| {
            let text = document.text();
            let revision = document.revision();
            let layout = document.layout_items.clone();
            let snapshot = DocumentSnapshot::new(
                "changed",
                vec![DocumentRegion::new("history", 0..7, EditPolicy::Readonly)],
                DocumentProjection::identity(7),
                vec![],
                DocumentStyles::default(),
            );
            assert_eq!(
                document.apply_host_splice(revision, Some(&"draft"), Some(&"draft"), snapshot, cx),
                Err(HostTransactionError::FragmentBoundary)
            );
            assert_eq!(document.text(), text);
            assert_eq!(document.revision(), revision);
            assert_eq!(document.layout_items, layout);
        });
    }

    #[gpui::test]
    fn host_insertion_preserves_draft_selection_and_user_undo(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "draft", window, cx);
                let host = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(7..7, "stream ")],
                    document.text().len(),
                )
                .unwrap();
                document
                    .apply_host_transaction(
                        host,
                        vec![
                            DocumentRegion::new("history", 0..14, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 14..19, EditPolicy::Editable),
                        ],
                        cx,
                    )
                    .unwrap();

                assert_eq!(document.text(), "historystream draft");
                assert_eq!(document.selected_range(), 19..19);
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "historystream ");
                assert_eq!(document.regions()[1].range(), 14..14);
                assert_eq!(document.selected_range(), 14..14);
                document.redo(&Redo, window, cx);
                assert_eq!(document.text(), "historystream draft");
                assert_eq!(document.selected_range(), 19..19);
            });
        });
    }

    #[gpui::test]
    fn host_insertion_preserves_an_active_ime_range(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                let host = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(7..7, "stream ")],
                    document.text().len(),
                )
                .unwrap();
                document
                    .apply_host_transaction(
                        host,
                        vec![
                            DocumentRegion::new("history", 0..14, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 14..16, EditPolicy::Editable),
                        ],
                        cx,
                    )
                    .unwrap();

                assert_eq!(document.marked_text_range(window, cx), Some(14..16));
                document.replace_text_in_range(None, "你", window, cx);
                assert_eq!(document.text(), "historystream 你");
                document.undo(&Undo, window, cx);
                assert_eq!(document.text(), "historystream ");
            });
        });
    }

    #[gpui::test]
    fn host_transaction_cannot_touch_editable_content(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "draft", window, cx);
                let host = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(7..8, "D")],
                    document.text().len(),
                )
                .unwrap();
                assert_eq!(
                    document.apply_host_transaction(
                        host,
                        vec![
                            DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 7..12, EditPolicy::Editable),
                        ],
                        cx,
                    ),
                    Err(HostTransactionError::TouchesEditableRegion)
                );
                assert_eq!(document.text(), "historydraft");
            });
        });
    }

    #[gpui::test]
    fn stale_host_transaction_is_rejected(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                let stale = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(0..0, "old")],
                    document.text().len(),
                )
                .unwrap();
                document.replace_text_in_range(None, "draft", window, cx);
                assert_eq!(
                    document.apply_host_transaction(
                        stale,
                        vec![
                            DocumentRegion::new("history", 0..10, EditPolicy::Readonly),
                            DocumentRegion::new("draft", 10..15, EditPolicy::Editable),
                        ],
                        cx,
                    ),
                    Err(HostTransactionError::StaleRevision)
                );
            });
        });
    }

    #[gpui::test]
    fn host_insertion_preserves_the_viewport_source_anchor(cx: &mut TestAppContext) {
        let history = (0..100)
            .map(|line| format!("line {line:03}\n"))
            .collect::<String>();
        let history_len = history.len();
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new({
                    let history = history.clone();
                    move |cx| {
                        DocumentState::new(
                            history,
                            vec![
                                DocumentRegion::new(
                                    "history",
                                    0..history_len,
                                    EditPolicy::Readonly,
                                ),
                                DocumentRegion::new(
                                    "draft",
                                    history_len..history_len,
                                    EditPolicy::Editable,
                                ),
                            ],
                            cx,
                        )
                        .unwrap()
                    }
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
            document.read_with(cx, |document, _| {
                document.list_state.scroll_to(ListOffset {
                    item_ix: 40,
                    offset_in_item: px(5.),
                });
            });
            let _ = window.draw(cx);
        });
        let (old_top, old_scroll) = document.read_with(&cx, |document, _| {
            let viewport = document.list_state.viewport_bounds();
            let top = document.offset_for_point(point(viewport.left(), viewport.top() + px(1.)));
            (top, document.list_state.logical_scroll_top())
        });

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                let host = EditTransaction::new(
                    document.revision(),
                    EditOrigin::Host,
                    vec![TextEdit::new(0..0, "new\n")],
                    document.text().len(),
                )
                .unwrap();
                document
                    .apply_host_transaction(
                        host,
                        vec![
                            DocumentRegion::new(
                                "history",
                                0..history_len + 4,
                                EditPolicy::Readonly,
                            ),
                            DocumentRegion::new(
                                "draft",
                                history_len + 4..history_len + 4,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
            let _ = window.draw(cx);
        });

        document.read_with(&cx, |document, _| {
            let viewport = document.list_state.viewport_bounds();
            let new_top =
                document.offset_for_point(point(viewport.left(), viewport.top() + px(1.)));
            assert_eq!(new_top, old_top + 4);
            assert!(document.list_state.logical_scroll_top().item_ix > old_scroll.item_ix);
        });
    }

    #[gpui::test]
    fn long_documents_only_layout_the_viewport_and_overdraw(cx: &mut TestAppContext) {
        let history = (0..1_000)
            .map(|index| format!("row {index:04}\n"))
            .collect::<String>();
        let history_len = history.len();
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    DocumentState::new(
                        history,
                        vec![
                            DocumentRegion::new("history", 0..history_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                history_len..history_len,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap()
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.list_state.item_count(), 1_001);
            assert!(document.text_layouts.len() < 100);
        });
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_selection(history_len..history_len, false, cx);
                document.reveal_caret(cx);
            });
            let _ = window.draw(cx);
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert!(
                document
                    .text_layouts
                    .iter()
                    .any(|record| record.item_ix == 1_000)
            );
            document.list_state.scroll_to(ListOffset {
                item_ix: 900,
                offset_in_item: Pixels::ZERO,
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            let visible_items = document
                .text_layouts
                .iter()
                .map(|record| record.item_ix)
                .collect::<Vec<_>>();
            assert!(visible_items.len() < 100);
            assert!(visible_items.contains(&900));
            assert!(!visible_items.iter().any(|item| *item < 850));
            let viewport = document.list_state.viewport_bounds();
            let source = document.offset_for_point(point(viewport.left(), viewport.top() + px(1.)));
            assert!(source >= 9 * 900);
        });
    }

    #[gpui::test]
    fn typing_in_a_visible_draft_preserves_its_screen_position(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let history = "history\n".repeat(12);
        let end = history.len();
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            history,
                            vec![
                                DocumentRegion::new("history", 0..end, EditPolicy::Readonly),
                                DocumentRegion::new("draft", end..end, EditPolicy::Editable),
                            ],
                            DocumentProjection::identity(end),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_dynamic_trailer(true, cx);
                document.set_selection(end..end, false, cx);
                document.focus_handle.focus(window, cx);
            });
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        let before = document.read_with(&cx, |document, _| {
            document
                .screen_position_for_source(end, Affinity::After)
                .unwrap()
                .y
        });
        for keys in ["x", "y", "backspace", "backspace"] {
            cx.simulate_keystrokes(keys);
            cx.update(|window, cx| {
                for _ in 0..3 {
                    let _ = window.draw(cx);
                }
            });
            document.read_with(&cx, |document, _| {
                let after = document
                    .screen_position_for_source(document.model.cursor(), Affinity::After)
                    .unwrap()
                    .y;
                assert!(
                    (after - before).abs() < px(1.),
                    "visible draft moved from {before:?} to {after:?}"
                );
            });
        }
        cx.simulate_keystrokes("x enter y");
        document.read_with(&cx, |document, _| {
            let after = document
                .screen_position_for_source(end, Affinity::After)
                .unwrap()
                .y;
            assert!(
                (after - before).abs() < px(1.),
                "newline moved the draft to the top"
            );
        });
        let mut previous_y = before;
        for _ in 0..45 {
            cx.simulate_keystrokes("enter");
            cx.update(|window, cx| {
                for _ in 0..3 {
                    let _ = window.draw(cx);
                }
            });
            let y = document.read_with(&cx, |document, _| {
                document
                    .screen_position_for_source(document.model.cursor(), Affinity::After)
                    .unwrap()
                    .y
            });
            assert!(
                y >= previous_y - px(1.),
                "caret jumped upward at the viewport edge: {previous_y:?} -> {y:?}"
            );
            previous_y = y;
        }
    }

    #[gpui::test]
    fn scroll_pin_tracks_a_document_anchor_and_user_scroll_stops_following(
        cx: &mut TestAppContext,
    ) {
        let history = (0..200)
            .map(|index| format!("row {index:04}\n"))
            .collect::<String>();
        let history_len = history.len();
        let anchor_offset = 9 * 100;
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    DocumentState::new(
                        history,
                        vec![
                            DocumentRegion::new("history", 0..history_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                history_len..history_len,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap()
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
            document.update(cx, |document, cx| {
                assert_eq!(
                    document.pin_scroll(
                        "turn-1",
                        &DocumentPosition::new("history", anchor_offset, Affinity::After,),
                        0.381_966,
                        cx,
                    ),
                    Ok(())
                );
            });
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        document.read_with(&cx, |document, _| {
            let viewport = document.list_state.viewport_bounds();
            let position = document
                .screen_position_for_source(anchor_offset, Affinity::After)
                .unwrap();
            let target = viewport.top() + viewport.size.height * 0.381_966;
            assert!((position.y - target).abs() < px(1.));
            assert_eq!(document.pinned_scroll_id(), Some(&"turn-1"));
        });

        // A stream refresh must not first jump the anchor to the top and then
        // correct it in a later frame.
        let scroll =
            document.read_with(&cx, |document, _| document.list_state.logical_scroll_top());
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                document
                    .pin_scroll(
                        "turn-1",
                        &DocumentPosition::new("history", anchor_offset, Affinity::After),
                        0.381_966,
                        cx,
                    )
                    .unwrap();
                let after = document.list_state.logical_scroll_top();
                assert_eq!(after.item_ix, scroll.item_ix);
                assert_eq!(after.offset_in_item, scroll.offset_in_item);
            });
        });

        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                assert!(!document.unpin_scroll(&"turn-2", cx));
                assert_eq!(document.pinned_scroll_id(), Some(&"turn-1"));
            });
        });
        let viewport = document.read_with(&cx, |document, _| document.list_state.viewport_bounds());
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(Pixels::ZERO, px(-80.))),
            ..Default::default()
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
        });

        // Scrollbar writes bypass ListState's wheel callback. Exercise the
        // rendered track and thumb, including a click that does not drag.
        let stopped = Rc::new(Cell::new(0));
        let observed = stopped.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&document, move |_, event, _| {
                if matches!(event, DocumentEvent::StopFollowingRequested { .. }) {
                    observed.set(observed.get() + 1);
                }
            })
        });
        cx.update(|window, cx| {
            Theme::global_mut(cx).scrollbar =
                crate::ScrollbarTheme::new().with_mode(crate::ScrollbarMode::Always);
            document.update(cx, |document, cx| {
                document
                    .pin_scroll(
                        "turn-3",
                        &DocumentPosition::new("history", anchor_offset, Affinity::After),
                        0.381_966,
                        cx,
                    )
                    .unwrap();
            });
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        let selection = document.read_with(&cx, |document, _| document.model.selected_range());
        let track_top = point(viewport.right() - px(8.), viewport.top() + px(8.));
        cx.simulate_click(track_top, Modifiers::default());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
            assert_eq!(document.model.selected_range(), selection);
        });
        assert_eq!(stopped.get(), 1);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .pin_scroll(
                        "turn-4",
                        &DocumentPosition::new("history", 0, Affinity::After),
                        0.381_966,
                        cx,
                    )
                    .unwrap();
            });
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        cx.simulate_mouse_down(track_top, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(track_top.x, viewport.center().y),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(track_top.x, viewport.center().y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.update(|window, cx| {
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
            assert_eq!(document.model.selected_range(), selection);
            assert!(document.list_state.logical_scroll_top().item_ix > 0);
        });
        assert_eq!(stopped.get(), 2);
    }

    #[gpui::test]
    fn editing_beside_a_scroll_pin_keeps_following_until_inspection(cx: &mut TestAppContext) {
        let history = (0..200)
            .map(|index| format!("row {index:04}\n"))
            .collect::<String>();
        let history_len = history.len();
        let text = format!("{history}draft");
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    let mut document = DocumentState::new(
                        text,
                        vec![
                            DocumentRegion::new("history", 0..history_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                history_len..history_len + 5,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap();
                    document.set_dynamic_trailer(true, cx);
                    document
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let frontier = DocumentPosition::new("history", history_len, Affinity::Before);
        let pin = |id: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    document.pin_scroll(id, &frontier, 0.381_966, cx).unwrap();
                });
                for _ in 0..3 {
                    let _ = window.draw(cx);
                }
            });
        };
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        pin("run", &mut cx);
        let fraction = document.read_with(&cx, |document, _| {
            document.viewport_fraction(&frontier).unwrap()
        });
        assert!((fraction - 0.381_966).abs() < 0.01, "{fraction}");

        let events = Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&document, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });
        // Placing the caret in the visible draft and typing keeps the pin.
        let draft = document.read_with(&cx, |document, _| {
            document
                .screen_position_for_source(history_len + 5, Affinity::Before)
                .unwrap()
        });
        cx.simulate_click(draft + point(px(2.), px(4.)), Modifiers::default());
        cx.simulate_keystrokes("x");
        cx.update(|window, cx| {
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        document.read_with(&cx, |document, _| {
            assert!(document.text().ends_with("draftx"));
            assert_eq!(document.pinned_scroll_id(), Some(&"run"));
        });
        assert!(
            events
                .borrow()
                .iter()
                .all(|event| !matches!(event, DocumentEvent::StopFollowingRequested { .. }))
        );

        // Clicking history is a deliberate inspection.
        let row = document.read_with(&cx, |document, _| {
            document
                .screen_position_for_source(history_len - 9 * 3, Affinity::After)
                .unwrap()
        });
        cx.simulate_click(row + point(px(2.), px(4.)), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
        });

        // Moving the caret out of view stops following and reveals it.
        pin("run-2", &mut cx);
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-up"
        } else {
            "ctrl-home"
        });
        cx.update(|window, cx| {
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
            assert_eq!(document.list_state.logical_scroll_top().item_ix, 0);
        });

        // User scrolling is reported whether or not anything was pinned.
        events.borrow_mut().clear();
        let viewport = document.read_with(&cx, |document, _| document.list_state.viewport_bounds());
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(Pixels::ZERO, px(-80.))),
            ..Default::default()
        });
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, DocumentEvent::ViewportScrolled))
        );
    }

    #[gpui::test]
    fn moving_a_scroll_pin_eases_and_settling_lands_before_release(cx: &mut TestAppContext) {
        let history = (0..200)
            .map(|index| format!("row {index:04}\n"))
            .collect::<String>();
        let history_len = history.len();
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    let mut document = DocumentState::new(
                        history,
                        vec![DocumentRegion::new(
                            "history",
                            0..history_len,
                            EditPolicy::Readonly,
                        )],
                        cx,
                    )
                    .unwrap();
                    document.set_dynamic_trailer(true, cx);
                    document
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let anchor = DocumentPosition::new("history", 9 * 150, Affinity::After);
        // Returns whether the pin eases to its place over the next frames.
        let pin = |fraction: f32, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                document.update(cx, |document, cx| {
                    document.pin_scroll("run", &anchor, fraction, cx).unwrap();
                    document.viewport.pin.as_ref().unwrap().is_easing()
                })
            })
        };
        // One frame: an easing pin asks for the next one.
        let draw = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
                let _ = window.draw(cx);
            });
        };
        let fraction = |cx: &mut VisualTestContext| {
            document.read_with(cx, |document, _| {
                document.viewport_fraction(&anchor).unwrap()
            })
        };
        let stopped = Rc::new(Cell::new(0));
        let observed = stopped.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&document, move |_, event, _| {
                if matches!(event, DocumentEvent::StopFollowingRequested { .. }) {
                    observed.set(observed.get() + 1);
                }
            })
        });
        draw(&mut cx);
        // A new pin lands directly.
        assert!(!pin(0.2, &mut cx));
        for _ in 0..3 {
            draw(&mut cx);
        }
        assert!((fraction(&mut cx) - 0.2).abs() < 0.01);

        // Moving it approaches the new place over several frames, then
        // tracks the same target directly.
        assert!(pin(0.8, &mut cx));
        for _ in 0..12 {
            draw(&mut cx);
        }
        assert!((fraction(&mut cx) - 0.8).abs() < 0.01);
        assert!(!pin(0.8, &mut cx));

        // With reduced motion it moves directly.
        cx.update(|_, cx| cx.set_reduce_motion(true));
        assert!(!pin(0.2, &mut cx));
        for _ in 0..3 {
            draw(&mut cx);
        }
        assert!((fraction(&mut cx) - 0.2).abs() < 0.01);
        cx.update(|_, cx| cx.set_reduce_motion(false));

        // Settling applies the final move, then releases without a stop event.
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                document.pin_scroll("run", &anchor, 0.8, cx).unwrap();
                assert!(document.settle_scroll_pin(&"run", cx));
            })
        });
        for _ in 0..3 {
            draw(&mut cx);
        }
        assert!((fraction(&mut cx) - 0.8).abs() < 0.01);
        document.read_with(&cx, |document, _| {
            assert_eq!(document.pinned_scroll_id(), None);
        });
        assert_eq!(stopped.get(), 0);
    }

    #[gpui::test]
    fn a_pin_above_trailing_content_keeps_it_clear_of_the_viewport_bottom(cx: &mut TestAppContext) {
        let history = (0..200)
            .map(|index| format!("row {index:04}\n"))
            .collect::<String>();
        let history_len = history.len();
        let text = format!("{history}d");
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    let mut document = DocumentState::new(
                        text,
                        vec![
                            DocumentRegion::new("history", 0..history_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                history_len..history_len + 1,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap();
                    document.set_dynamic_trailer(true, cx);
                    document
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(gpui::size(px(600.), px(400.)));
        let target = DocumentPosition::new("history", history_len - 9 * 5, Affinity::After);
        let draft_end = |document: &DocumentState<&'static str>| {
            DocumentPosition::new(
                "draft",
                document.text().len() - history_len,
                Affinity::After,
            )
        };
        let clearance = px(40.);
        let draw = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                for _ in 0..3 {
                    let _ = window.draw(cx);
                }
            });
        };
        draw(&mut cx);
        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                let draft = draft_end(document);
                document
                    .pin_scroll_above("run", &target, &draft, clearance, 0.3..=0.9, cx)
                    .unwrap();
            })
        });
        draw(&mut cx);
        // Returns the held fraction and the draft's bottom gap to the viewport.
        let measure = |cx: &mut VisualTestContext| {
            document.read_with(cx, |document, _| {
                let draft = draft_end(document);
                let held = document
                    .scroll_hold(&target, &draft, clearance, 0.3..=0.9)
                    .unwrap()
                    .fraction();
                assert!((document.viewport_fraction(&target).unwrap() - held).abs() < 0.01);
                assert_eq!(document.pinned_scroll_id(), Some(&"run"));
                let viewport = document.list_state.viewport_bounds();
                let gap = document
                    .layout_bottom_for_position(&draft)
                    .map(|bottom| viewport.bottom() - bottom);
                (held, gap)
            })
        };
        let (short, gap) = measure(&mut cx);
        assert!(short > 0.3 && short < 0.9, "{short}");
        assert!(
            gap.is_some_and(|gap| (gap - clearance).abs() < px(1.)),
            "{gap:?}"
        );

        // Growing trailing content raises the hold, down to its highest point.
        let draft = document.read_with(&cx, |document, _| {
            document
                .screen_position_for_source(history_len + 1, Affinity::Before)
                .unwrap()
        });
        cx.simulate_click(draft + point(px(2.), px(4.)), Modifiers::default());
        cx.simulate_keystrokes("enter a enter b");
        draw(&mut cx);
        let (taller, gap) = measure(&mut cx);
        assert!(taller < short - 0.02, "{short} -> {taller}");
        assert!(
            gap.is_some_and(|gap| (gap - clearance).abs() < px(1.)),
            "{gap:?}"
        );
        cx.simulate_keystrokes(&"enter x ".repeat(30));
        draw(&mut cx);
        document.read_with(&cx, |document, _| {
            // Typing past the viewport reveals the caret and stops following.
            assert_eq!(document.pinned_scroll_id(), None);
        });
    }

    #[gpui::test]
    fn scroll_pin_reveals_the_owned_block_across_hidden_projection_boundaries(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        cx.simulate_resize(gpui::size(px(600.), px(400.)));
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_block_renderer(
                    |id, _, _| {
                        div()
                            .h(px(if *id == "target" { 60. } else { 800. }))
                            .into_any_element()
                    },
                    cx,
                );
                document
                    .reset(
                        DocumentSnapshot::new(
                            "\u{fffc}\u{fffc}\u{fffc}draft",
                            vec![
                                DocumentRegion::new("before", 0..3, EditPolicy::Atomic),
                                DocumentRegion::new("target", 3..6, EditPolicy::Atomic),
                                DocumentRegion::new("after", 6..9, EditPolicy::Atomic),
                                DocumentRegion::new("draft", 9..14, EditPolicy::Editable),
                            ],
                            DocumentProjection::new(
                                14,
                                vec![
                                    ProjectionSpan::hide(0..3),
                                    ProjectionSpan::hide(3..6),
                                    ProjectionSpan::hide(6..9),
                                ],
                            )
                            .unwrap(),
                            vec![
                                DocumentBlock::new("before", 0..3),
                                DocumentBlock::new("target", 3..6),
                                DocumentBlock::new("after", 6..9),
                            ],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
        });
        for (offset, affinity) in [(0, Affinity::After), (3, Affinity::Before)] {
            cx.update(|window, cx| {
                document.update(cx, |document, cx| {
                    document.unpin_scroll(&"pin", cx);
                    document.list_state.scroll_to(ListOffset::default());
                });
                let _ = window.draw(cx);
                document.update(cx, |document, cx| {
                    assert!(
                        document
                            .block_layouts
                            .iter()
                            .all(|record| record.item_ix != 1)
                    );
                    document
                        .pin_scroll(
                            "pin",
                            &DocumentPosition::new("target", offset, affinity),
                            0.5,
                            cx,
                        )
                        .unwrap();
                });
                for _ in 0..3 {
                    let _ = window.draw(cx);
                }
            });
            document.read_with(&cx, |document, _| {
                let bounds = document
                    .list_state
                    .bounds_for_item(1)
                    .expect("pinned block");
                let edge = if offset == 0 {
                    bounds.top()
                } else {
                    bounds.bottom()
                };
                assert!((edge - document.list_state.viewport_bounds().center().y).abs() < px(1.));
            });
        }
    }

    #[gpui::test]
    fn drag_scroll_keeps_a_tracked_anchor_and_stops_on_release_outside(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let history = "history line\n".repeat(200);
        let history_len = history.len();
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .reset(
                        DocumentSnapshot::new(
                            history,
                            vec![
                                DocumentRegion::new(
                                    "history",
                                    0..history_len,
                                    EditPolicy::Readonly,
                                ),
                                DocumentRegion::new(
                                    "draft",
                                    history_len..history_len,
                                    EditPolicy::Editable,
                                ),
                            ],
                            DocumentProjection::identity(history_len),
                            vec![],
                            DocumentStyles::default(),
                        ),
                        cx,
                    )
                    .unwrap();
                document.list_state.scroll_to(ListOffset {
                    item_ix: 100,
                    offset_in_item: Pixels::ZERO,
                });
            });
            let _ = window.draw(cx);
        });
        let (start, outside, initial_top) = document.read_with(&cx, |document, _| {
            let viewport = document.list_state.viewport_bounds();
            (
                viewport.center(),
                point(viewport.left() + px(30.), viewport.bottom() + px(20.)),
                document.list_state.logical_scroll_top(),
            )
        });
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        let anchor = document.read_with(&cx, |document, _| document.model.selection_offsets().0);
        assert!(anchor > 0);
        cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::default());
        let initial_head = document.read_with(&cx, |document, _| {
            assert!(document.auto_scroll.is_active());
            assert!(document.selected_range().end < history_len);
            document.model.cursor()
        });
        cx.run_until_parked();
        for _ in 0..5 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(20));
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        document.read_with(&cx, |document, _| {
            let top = document.list_state.logical_scroll_top();
            assert!(
                top.item_ix > initial_top.item_ix
                    || top.offset_in_item > initial_top.offset_in_item
            );
            assert!(document.model.cursor() > initial_head);
            assert_eq!(document.model.selection_offsets().0, anchor);
        });

        // Host insertion moves the real selection anchor while dragging.
        document.update(&mut cx, |document, cx| {
            let insertion = "new line\n";
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..0, insertion)],
                document.text().len(),
            )
            .unwrap();
            let end = history_len + insertion.len();
            document
                .apply_host_transaction(
                    transaction,
                    vec![
                        DocumentRegion::new("history", 0..end, EditPolicy::Readonly),
                        DocumentRegion::new("draft", end..end, EditPolicy::Editable),
                    ],
                    cx,
                )
                .unwrap();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(
                document.model.selection_offsets().0,
                anchor + "new line\n".len()
            )
        });
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
        let stopped = document.read_with(&cx, |document, _| {
            assert!(!document.auto_scroll.is_active());
            assert!(document.auto_scroll.last_drag_position.is_none());
            (
                document.list_state.logical_scroll_top(),
                document.selected_range(),
            )
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), stopped.1)
        });
        document.read_with(&cx, |document, _| {
            let top = document.list_state.logical_scroll_top();
            assert_eq!(
                (top.item_ix, top.offset_in_item),
                (stopped.0.item_ix, stopped.0.offset_in_item)
            );
        });
    }

    #[gpui::test]
    fn hanging_marker_aligns_wrapped_rows_and_later_lines(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let marker = "• ";
        let item = format!("{marker}{}\nnext line\n", "中文 words ".repeat(10));
        let source = format!("{item}after");
        let end = source.len();
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_text_renderer(
                    |_, text, _, _| div().w(px(200.)).child(text).into_any_element(),
                    cx,
                );
                document
                    .reset(
                        DocumentSnapshot::new(
                            source.clone(),
                            vec![
                                DocumentRegion::new("history", 0..end, EditPolicy::Readonly),
                                DocumentRegion::new("draft", end..end, EditPolicy::Editable),
                            ],
                            DocumentProjection::new(end, vec![]).unwrap(),
                            vec![],
                            DocumentStyles::new(
                                vec![
                                    super::super::DocumentParagraphStyle::new(
                                        0..item.len(),
                                        TextStyleRefinement::default(),
                                    )
                                    .with_hanging_marker(marker),
                                ],
                                vec![],
                            ),
                        ),
                        cx,
                    )
                    .unwrap();
            });
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            let [first, second, after, ..] = document.text_layouts.as_slice() else {
                panic!("expected three text lines");
            };
            let text_x = first.layout.position_for_index(marker.len()).unwrap().x;
            assert!(text_x > first.bounds.left());

            // Wrapped rows start under the text, and the indent itself maps to
            // the start of the row.
            let line = first.layout.text().line_layout_for_index(0).unwrap();
            let wrap = line.rows()[1].range().start;
            let row = first.bounds.top() + first.layout.line_height() * 1.5;
            assert_eq!(
                first
                    .layout
                    .text()
                    .index_for_position(point(text_x + px(1.), row)),
                Ok(wrap)
            );
            assert_eq!(
                first
                    .layout
                    .text()
                    .index_for_position(point(first.bounds.left() + px(1.), row)),
                Err(wrap)
            );
            // Later lines of the paragraph keep that alignment; the next
            // paragraph does not.
            assert_eq!(second.layout.position_for_index(0).unwrap().x, text_x);
            assert_eq!(
                after.layout.position_for_index(0).unwrap().x,
                after.bounds.left()
            );
        });
    }

    #[gpui::test]
    fn pointer_selection_covers_rich_text_gaps_wrapping_blocks_and_trailer(
        cx: &mut TestAppContext,
    ) {
        let (document, mut cx) = document_view(cx);
        let heading = "# Heading\n";
        let body = format!("{}\n", "中文🙂 words ".repeat(12));
        let block_start = heading.len() + body.len();
        let draft_start = block_start + '\u{fffc}'.len_utf8();
        let source = format!("{heading}{body}\u{fffc}draft");
        let end = source.len();
        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.set_block_renderer(
                    |_, _, _| div().h(px(80.)).w(px(120.)).into_any_element(),
                    cx,
                );
                document.set_text_renderer(
                    |_, text, _, _| {
                        div()
                            .w(px(260.))
                            .pt(px(24.))
                            .pl(px(30.))
                            .child(text)
                            .into_any_element()
                    },
                    cx,
                );
                document
                    .reset(
                        DocumentSnapshot::new(
                            source,
                            vec![
                                DocumentRegion::new(
                                    "history",
                                    0..block_start,
                                    EditPolicy::Readonly,
                                ),
                                DocumentRegion::new(
                                    "object",
                                    block_start..draft_start,
                                    EditPolicy::Atomic,
                                ),
                                DocumentRegion::new(
                                    "draft",
                                    draft_start..end,
                                    EditPolicy::Editable,
                                ),
                            ],
                            DocumentProjection::new(
                                end,
                                vec![
                                    ProjectionSpan::hide(0..2),
                                    ProjectionSpan::hide(block_start..draft_start),
                                ],
                            )
                            .unwrap(),
                            vec![DocumentBlock::new("object", block_start..draft_start)],
                            DocumentStyles::new(
                                vec![super::super::DocumentParagraphStyle::new(
                                    0..heading.len(),
                                    TextStyleRefinement {
                                        font_size: Some(px(24.).into()),
                                        ..Default::default()
                                    },
                                )],
                                vec![],
                            ),
                        ),
                        cx,
                    )
                    .unwrap();
                document.set_dynamic_trailer(true, cx);
                document.focus_handle.focus(window, cx);
            });
            let _ = window.draw(cx);
        });
        let (start, lower_gap, left, upper_gap, wrap_left, wrap_source) =
            document.read_with(&cx, |document, _| {
                let title = &document.text_layouts[0];
                let body = &document.text_layouts[1];
                let origin = title.layout.position_for_index(1).unwrap();
                let start = point(origin.x, origin.y + title.layout.line_height() / 2.);
                let line = body.layout.text().line_layout_for_index(0).unwrap();
                let wrap = line.rows()[1].range().start;
                let wrap_y = body.bounds.top() + body.layout.line_height();
                (
                    start,
                    point(start.x, title.bounds.bottom() + px(1.)),
                    point(title.bounds.left() - px(10.), start.y),
                    point(start.x, body.bounds.top() - px(1.)),
                    point(
                        body.bounds.left() - px(10.),
                        wrap_y + body.layout.line_height() / 2.,
                    ),
                    heading.len() + wrap,
                )
            });
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(lower_gap, Some(MouseButton::Left), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), 3..heading.len() - 1)
        });
        cx.simulate_mouse_move(left, Some(MouseButton::Left), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.model.selection_offsets(), (3, 2))
        });
        cx.simulate_mouse_move(upper_gap, Some(MouseButton::Left), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.model.selection_offsets(), (3, heading.len()))
        });
        cx.simulate_mouse_move(wrap_left, Some(MouseButton::Left), Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.model.selection_offsets(), (3, wrap_source))
        });
        cx.simulate_mouse_up(wrap_left, MouseButton::Left, Modifiers::default());

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
        cx.simulate_keystrokes("shift-down");
        document.read_with(&cx, |document, _| {
            assert!(document.selected_range().end >= heading.len());
            assert!(document.selected_range().end < block_start);
            let block = &document.block_layouts[0];
            assert_eq!(
                document.offset_for_point(point(
                    block.bounds.right() + px(20.),
                    block.bounds.top() + px(1.)
                )),
                block_start
            );
            assert_eq!(
                document.offset_for_point(point(
                    block.bounds.right() + px(20.),
                    block.bounds.bottom() - px(1.)
                )),
                draft_start
            );
            let (_, trailer) = document.trailer_layout.unwrap();
            assert_eq!(document.offset_for_point(trailer.center()), end);
        });
        let (block_center, selected) = document.read_with(&cx, |document, _| {
            (
                document.block_layouts[0].bounds.center(),
                document.selected_range(),
            )
        });
        cx.simulate_mouse_down(block_center, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(block_center, MouseButton::Left, Modifiers::default());
        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), selected)
        });
    }

    #[gpui::test]
    fn projection_maps_pointer_selection_and_draft_edits(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document
                    .set_projection(
                        DocumentProjection::new(
                            document.text().len(),
                            vec![super::super::ProjectionSpan::hide(0..3)],
                        )
                        .unwrap(),
                        cx,
                    )
                    .unwrap();
                assert_eq!(document.display_text(), "tory");
            });
            let _ = window.draw(cx);
        });

        document.read_with(&cx, |document, _| {
            let point = document
                .text_layouts
                .first()
                .unwrap()
                .layout
                .position_for_index(1)
                .unwrap();
            assert_eq!(document.offset_for_point(point), 4);
            assert_eq!(
                document.position_for_offset(7, Affinity::Before).unwrap(),
                DocumentPosition::new("history", 7, Affinity::Before)
            );
            assert_eq!(
                document.position_for_offset(7, Affinity::After).unwrap(),
                DocumentPosition::new("draft", 0, Affinity::After)
            );
        });

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "x", window, cx);
                assert_eq!(document.text(), "historyx");
                assert_eq!(document.display_text(), "toryx");
                assert_eq!(document.segment_paint_snapshot(0, &(0..5)).2, Some(5));
            });
        });
    }

    #[gpui::test]
    fn rich_presentation_maps_paragraph_and_inline_styles_through_projection(
        cx: &mut TestAppContext,
    ) {
        const SOURCE: &str = "# Head\nplain **bold**\n";
        let source_len = SOURCE.len();
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    let mut document = DocumentState::new(
                        SOURCE,
                        vec![
                            DocumentRegion::new("history", 0..source_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                source_len..source_len,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap();
                    document
                        .set_rich_presentation(
                            DocumentProjection::new(
                                source_len,
                                vec![
                                    super::super::ProjectionSpan::hide(0..2),
                                    super::super::ProjectionSpan::hide(13..15),
                                    super::super::ProjectionSpan::hide(19..21),
                                ],
                            )
                            .unwrap(),
                            Vec::new(),
                            DocumentStyles::new(
                                vec![super::super::DocumentParagraphStyle::new(
                                    0..7,
                                    TextStyleRefinement {
                                        font_size: Some(px(24.).into()),
                                        ..Default::default()
                                    },
                                )],
                                vec![super::super::DocumentInlineStyle::new(
                                    15..19,
                                    HighlightStyle {
                                        font_weight: Some(FontWeight::BOLD),
                                        ..Default::default()
                                    },
                                )],
                            ),
                            cx,
                        )
                        .unwrap();
                    document
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.display_text(), "Head\nplain bold\n");
            let DocumentLayoutItem::Text { presentation, .. } = &document.layout_items[0] else {
                panic!("first item must be text");
            };
            let presentation = presentation.as_deref().unwrap();
            assert_eq!(
                presentation.text_style.as_ref().unwrap().font_size,
                Some(px(24.).into())
            );
            let DocumentLayoutItem::Text { presentation, .. } = &document.layout_items[1] else {
                panic!("second item must be text");
            };
            let presentation = presentation.as_deref().unwrap();
            assert_eq!(
                presentation
                    .highlights
                    .iter()
                    .map(|(range, style)| { (range.clone(), style.font_weight) })
                    .collect::<Vec<_>>(),
                vec![(0..6, None), (6..10, Some(FontWeight::BOLD))]
            );
            assert!(
                document.text_layouts[0].bounds.size.height
                    > document.text_layouts[1].bounds.size.height
            );
        });
    }

    #[gpui::test]
    fn styled_host_updates_require_the_next_style_descriptors(cx: &mut TestAppContext) {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                "history",
                vec![
                    DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                    DocumentRegion::new("draft", 7..7, EditPolicy::Editable),
                ],
                cx,
            )
            .unwrap();
            document
                .set_rich_presentation(
                    DocumentProjection::identity(7),
                    Vec::new(),
                    DocumentStyles::new(
                        vec![super::super::DocumentParagraphStyle::new(
                            0..7,
                            TextStyleRefinement::default(),
                        )],
                        Vec::new(),
                    ),
                    cx,
                )
                .unwrap();
            document
        });

        document.update(cx, |document, cx| {
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..0, "x")],
                document.text().len(),
            )
            .unwrap();
            let regions = vec![
                DocumentRegion::new("history", 0..8, EditPolicy::Readonly),
                DocumentRegion::new("draft", 8..8, EditPolicy::Editable),
            ];
            assert_eq!(
                document.apply_host_transaction(transaction.clone(), regions.clone(), cx),
                Err(HostTransactionError::StylesRequired)
            );
            document
                .apply_host_transaction_with_rich_presentation(
                    transaction,
                    regions,
                    DocumentProjection::identity(8),
                    Vec::new(),
                    DocumentStyles::new(
                        vec![super::super::DocumentParagraphStyle::new(
                            0..8,
                            TextStyleRefinement::default(),
                        )],
                        Vec::new(),
                    ),
                    cx,
                )
                .unwrap();
            assert_eq!(document.text(), "xhistory");
        });
    }

    #[gpui::test]
    fn projection_cannot_replace_editable_source(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|window, cx| {
            document.update(cx, |document, cx| {
                document.replace_text_in_range(None, "x", window, cx);
                assert_eq!(
                    document.set_projection(
                        DocumentProjection::new(
                            document.text().len(),
                            vec![super::super::ProjectionSpan::hide(7..8)],
                        )
                        .unwrap(),
                        cx,
                    ),
                    Err(ProjectionError::EditableOverlap(7..8))
                );
                assert_eq!(document.display_text(), "historyx");
            });
        });
    }

    #[gpui::test]
    fn projected_host_update_requires_the_next_projection(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);

        cx.update(|_, cx| {
            document.update(cx, |document, cx| {
                document
                    .set_projection(
                        DocumentProjection::new(
                            document.text().len(),
                            vec![super::super::ProjectionSpan::hide(0..3)],
                        )
                        .unwrap(),
                        cx,
                    )
                    .unwrap();
                let revision = document.revision();
                let source_len = document.text().len();
                let host = || {
                    EditTransaction::new(
                        revision,
                        EditOrigin::Host,
                        vec![TextEdit::new(0..0, "new")],
                        source_len,
                    )
                    .unwrap()
                };
                let regions = || {
                    vec![
                        DocumentRegion::new("history", 0..10, EditPolicy::Readonly),
                        DocumentRegion::new("draft", 10..10, EditPolicy::Editable),
                    ]
                };

                assert_eq!(
                    document.apply_host_transaction(host(), regions(), cx),
                    Err(HostTransactionError::ProjectionRequired)
                );
                document
                    .apply_host_transaction_with_projection(
                        host(),
                        regions(),
                        DocumentProjection::new(10, vec![super::super::ProjectionSpan::hide(3..6)])
                            .unwrap(),
                        cx,
                    )
                    .unwrap();
                assert_eq!(document.text(), "newhistory");
                assert_eq!(document.display_text(), "newtory");
            });
        });
    }

    #[gpui::test]
    fn inline_block_occupies_document_flow_and_maps_hits_to_atomic_boundaries(
        cx: &mut TestAppContext,
    ) {
        const SOURCE: &str = "before\n\u{fffc}\nafter\n";
        let source_len = SOURCE.len();
        let mut document = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.set_global(Theme::default());
                crate::init(cx);
                document = Some(cx.new(move |cx| {
                    let mut document = DocumentState::new(
                        SOURCE,
                        vec![
                            DocumentRegion::new("before", 0..7, EditPolicy::Readonly),
                            DocumentRegion::new("tool", 7..10, EditPolicy::Atomic),
                            DocumentRegion::new("after", 10..source_len, EditPolicy::Readonly),
                            DocumentRegion::new(
                                "draft",
                                source_len..source_len,
                                EditPolicy::Editable,
                            ),
                        ],
                        cx,
                    )
                    .unwrap();
                    document.set_block_renderer(
                        |_, _, _| div().w_full().h(px(80.)).into_any_element(),
                        cx,
                    );
                    document
                        .set_presentation(
                            DocumentProjection::new(
                                source_len,
                                vec![super::super::ProjectionSpan::hide(7..10)],
                            )
                            .unwrap(),
                            vec![DocumentBlock::new("tool", 7..10)],
                            cx,
                        )
                        .unwrap();
                    document
                }));
                cx.new(|_| DocumentRoot(document.clone().unwrap()))
            })
            .unwrap()
        });
        let document = document.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        document.read_with(&cx, |document, _| {
            assert_eq!(document.model.display_text(), "before\n\nafter\n");
            assert_eq!(document.block_layouts.len(), 1);
            assert_eq!(document.text_layouts.len(), 4);
            let block = &document.block_layouts[0];
            assert_eq!(block.source, 7..10);
            assert_eq!(block.bounds.size.height, px(80.));
            assert!(document.text_layouts[0].bounds.bottom() <= block.bounds.top());
            assert!(block.bounds.bottom() <= document.text_layouts[1].bounds.top());
            assert_eq!(
                document
                    .offset_for_point(point(block.bounds.center().x, block.bounds.top() + px(10.))),
                7
            );
            assert_eq!(
                document.offset_for_point(point(
                    block.bounds.center().x,
                    block.bounds.bottom() - px(10.)
                )),
                10
            );
        });
    }

    #[gpui::test]
    fn inline_blocks_require_a_renderer_and_hidden_atomic_source(cx: &mut TestAppContext) {
        let document = cx.new(|cx| {
            DocumentState::new(
                "abc",
                vec![DocumentRegion::new("tool", 0..3, EditPolicy::Atomic)],
                cx,
            )
            .unwrap()
        });

        document.update(cx, |document, cx| {
            let projection =
                DocumentProjection::new(3, vec![super::super::ProjectionSpan::hide(0..3)]).unwrap();
            assert_eq!(
                document.set_presentation(
                    projection.clone(),
                    vec![DocumentBlock::new("tool", 0..3)],
                    cx,
                ),
                Err(PresentationError::Blocks(BlockError::MissingRenderer))
            );
            document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
            assert_eq!(
                document.set_presentation(
                    DocumentProjection::identity(3),
                    vec![DocumentBlock::new("tool", 0..3)],
                    cx,
                ),
                Err(PresentationError::Blocks(BlockError::VisibleSource(0..3)))
            );
            document
                .set_presentation(projection, vec![DocumentBlock::new("tool", 0..3)], cx)
                .unwrap();
        });
    }

    #[gpui::test]
    fn host_updates_with_blocks_require_and_install_the_next_presentation(cx: &mut TestAppContext) {
        let document = cx.new(|cx| {
            let mut document = DocumentState::new(
                "A\u{fffc}draft",
                vec![
                    DocumentRegion::new("history", 0..1, EditPolicy::Readonly),
                    DocumentRegion::new("tool", 1..4, EditPolicy::Atomic),
                    DocumentRegion::new("draft", 4..9, EditPolicy::Editable),
                ],
                cx,
            )
            .unwrap();
            document.set_block_renderer(|_, _, _| div().h(px(40.)).into_any_element(), cx);
            document
                .set_presentation(
                    DocumentProjection::new(9, vec![super::super::ProjectionSpan::hide(1..4)])
                        .unwrap(),
                    vec![DocumentBlock::new("tool", 1..4)],
                    cx,
                )
                .unwrap();
            document
        });

        document.update(cx, |document, cx| {
            let transaction = EditTransaction::new(
                document.revision(),
                EditOrigin::Host,
                vec![TextEdit::new(0..0, "Z")],
                document.text().len(),
            )
            .unwrap();
            assert_eq!(
                document.apply_host_transaction(
                    transaction.clone(),
                    vec![
                        DocumentRegion::new("history", 0..2, EditPolicy::Readonly),
                        DocumentRegion::new("tool", 2..5, EditPolicy::Atomic),
                        DocumentRegion::new("draft", 5..10, EditPolicy::Editable),
                    ],
                    cx,
                ),
                Err(HostTransactionError::ProjectionRequired)
            );
            document
                .apply_host_transaction_with_presentation(
                    transaction,
                    vec![
                        DocumentRegion::new("history", 0..2, EditPolicy::Readonly),
                        DocumentRegion::new("tool", 2..5, EditPolicy::Atomic),
                        DocumentRegion::new("draft", 5..10, EditPolicy::Editable),
                    ],
                    DocumentProjection::new(10, vec![super::super::ProjectionSpan::hide(2..5)])
                        .unwrap(),
                    vec![DocumentBlock::new("tool", 2..5)],
                    cx,
                )
                .unwrap();
            assert_eq!(document.text(), "ZA\u{fffc}draft");
            assert_eq!(document.display_text(), "ZAdraft");
            assert_eq!(document.model.blocks[0].source(), 2..5);
        });
    }
}
