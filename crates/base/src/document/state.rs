use std::{cell::Cell, collections::BTreeMap, error::Error, fmt, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, InteractiveElement as _, IntoElement as _, ListAlignment,
    ListOffset, ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, SharedString, Styled as _, TextStyleRefinement, UTF16Selection, Window,
    div, list, point, px,
};
use ropey::{LineType, Rope};
use sum_tree::Bias;

use crate::{
    AutoScroll, Scrollbar, ScrollbarHandle,
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
    Affinity, AnchorBias, BlockError, DocumentAnchor, DocumentBlock, DocumentElement,
    DocumentPosition, DocumentProjection, DocumentRegion, DocumentRegions, DocumentRevision,
    DocumentSelection, DocumentStyleError, DocumentStyles, EditDecision, EditOrigin, EditPolicy,
    EditTransaction, PositionError, ProjectionError, RegionError, TextEdit, TransactionError,
    block::{transform_blocks, validate_blocks},
    element::DocumentChild,
    position::{shift_offset, transform_offset},
    projection::ProjectionMap,
};

const DOCUMENT_INPUT_CONTEXT: &str = "Input";

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
    StylesRequired,
    InvalidStyles(DocumentStyleError),
}

impl fmt::Display for HostTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    Styles(DocumentStyleError),
    Position(PositionError),
}

impl fmt::Display for PresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Regions(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
            Self::Blocks(error) => error.fmt(formatter),
            Self::Styles(error) => error.fmt(formatter),
            Self::Position(error) => error.fmt(formatter),
        }
    }
}

impl Error for PresentationError {}

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
    Rejected(DocumentEditRejection),
    SelectionChanged,
    ProjectionChanged,
    PresentationChanged,
    StopFollowingRequested {
        pin_id: I,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTextContext<I> {
    node_id: I,
    source: Range<usize>,
    first_line: bool,
}

impl<I> DocumentTextContext<I> {
    pub fn node_id(&self) -> &I {
        &self.node_id
    }

    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub fn is_first_line(&self) -> bool {
        self.first_line
    }
}

pub struct DocumentSnapshot<I> {
    text: String,
    regions: Vec<DocumentRegion<I>>,
    projection: DocumentProjection,
    blocks: Vec<DocumentBlock<I>>,
    styles: DocumentStyles,
    selection: Option<DocumentPosition<I>>,
}

impl<I> DocumentSnapshot<I> {
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
            styles,
            selection: None,
        }
    }

    pub fn selection(mut self, selection: DocumentPosition<I>) -> Self {
        self.selection = Some(selection);
        self
    }
}

#[derive(Clone, Debug)]
struct AnchorRecord<I> {
    offset: usize,
    bias: AnchorBias,
    node_id: Option<I>,
    affinity: Affinity,
}

#[derive(Clone, Debug)]
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
            },
        );
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
            record.offset = transform_offset(record.offset, record.bias, range, replacement_len);
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
struct UndoRecord<I> {
    region_id: I,
    changes: Vec<UndoChange>,
    selection_before: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
    selection_after: Option<(DocumentPosition<I>, DocumentPosition<I>)>,
}

impl<I> UndoRecord<I> {
    fn bytes(&self) -> usize {
        self.changes
            .iter()
            .map(|change| change.removed.len() + change.inserted.len())
            .sum()
    }

    fn allocated_bytes(&self) -> usize {
        self.changes
            .iter()
            .map(|change| change.removed.capacity() + change.inserted.capacity())
            .sum()
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
}

impl<I> Default for DocumentUndoManager<I> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing: None,
        }
    }
}

impl<I: Clone + Eq> DocumentUndoManager<I> {
    fn record(&mut self, mut record: UndoRecord<I>, origin: EditOrigin) {
        self.redo.clear();
        let previous = self.undo.last_mut().filter(|previous| {
            self.coalescing == Some(origin)
                && previous.region_id == record.region_id
                && previous.selection_after == record.selection_before
        });
        let merged = previous.is_some_and(|previous| {
            if origin == EditOrigin::Composition {
                if let ([left], [right]) =
                    (previous.changes.as_mut_slice(), record.changes.as_slice())
                    && right.range == (left.range.start..left.range.start + left.inserted.len())
                    && right.removed == left.inserted
                {
                    if left.removed.len() + right.inserted.len() > MAX_UNDO_BYTES {
                        return false;
                    }
                    left.inserted = right.inserted.clone();
                } else if previous.changes.len() + record.changes.len() <= MAX_UNDO_CHANGES {
                    if previous.bytes() + record.bytes() > MAX_UNDO_BYTES {
                        return false;
                    }
                    previous.changes.append(&mut record.changes);
                } else {
                    return false;
                }
            } else if origin == EditOrigin::User {
                let ([left], [right]) =
                    (previous.changes.as_mut_slice(), record.changes.as_slice())
                else {
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
        if self.undo.last().is_some_and(|record| {
            record
                .changes
                .iter()
                .all(|change| change.removed == change.inserted)
        }) {
            self.undo.pop();
            self.break_coalescing();
        } else {
            self.coalescing =
                matches!(origin, EditOrigin::User | EditOrigin::Composition).then_some(origin);
        }
        self.trim(MAX_UNDO_RECORDS, MAX_UNDO_BYTES);
    }

    fn trim(&mut self, records: usize, bytes: usize) {
        if self
            .undo
            .last()
            .is_some_and(|record| record.changes.len() > MAX_UNDO_CHANGES)
        {
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
    styles: DocumentStyles,
    resolved_styles: ResolvedDocumentStyles,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ResolvedDocumentStyles {
    paragraphs: Vec<(Range<usize>, TextStyleRefinement)>,
    inline: Vec<ResolvedInlineStyle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedInlineStyle {
    display: Range<usize>,
    highlight: HighlightStyle,
    font_family: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DocumentTextPresentation {
    pub(super) text_style: Option<TextStyleRefinement>,
    pub(super) highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub(super) font_family_overrides: Vec<(Range<usize>, SharedString)>,
}

#[derive(Clone, Debug, PartialEq)]
enum DocumentLayoutItem<I> {
    Text {
        display: Range<usize>,
        source: Range<usize>,
        node_id: Option<I>,
        text: SharedString,
        presentation: Box<DocumentTextPresentation>,
    },
    Block {
        id: I,
        source: Range<usize>,
    },
    Trailer,
}

impl<I: Eq> DocumentLayoutItem<I> {
    fn same_layout_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Text {
                    text: left,
                    node_id: left_node,
                    presentation: left_presentation,
                    ..
                },
                Self::Text {
                    text: right,
                    node_id: right_node,
                    presentation: right_presentation,
                    ..
                },
            ) => {
                left_node == right_node && left == right && left_presentation == right_presentation
            }
            (Self::Block { id: left, .. }, Self::Block { id: right, .. }) => left == right,
            (Self::Trailer, Self::Trailer) => true,
            _ => false,
        }
    }
}

impl<I: Clone + Eq> DocumentModel<I> {
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
        if !self.blocks.is_empty() {
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
        mut blocks: Vec<DocumentBlock<I>>,
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
        let resolved_styles =
            resolve_document_styles(&self.text, &self.regions, &projection_map, &styles)
                .map_err(PresentationError::Styles)?;
        self.projection_map = projection_map;
        self.projection = projection;
        self.blocks = blocks;
        self.styles = styles;
        self.resolved_styles = resolved_styles;
        Ok(())
    }

    fn source_to_display(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        self.projection_map.source_to_display(offset, affinity)
    }

    fn display_to_source(&self, offset: usize, affinity: Affinity) -> Option<usize> {
        self.projection_map.display_to_source(offset, affinity)
    }

    fn layout_items(&self) -> Vec<DocumentLayoutItem<I>> {
        let display_text = self.display_text();
        let mut display_cursor = 0;
        let mut source_cursor = 0;
        let mut items = Vec::new();
        let mut blocks = self.blocks.iter().peekable();
        for (region_ix, region) in self.regions.as_slice().iter().enumerate() {
            let block = blocks
                .peek()
                .filter(|block| block.id() == region.id())
                .copied();
            let empty_text = region.policy() == EditPolicy::Editable
                && region.range().is_empty()
                && region_ix + 1 != self.regions.as_slice().len();
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
                &mut items,
                display_text,
                display_cursor..display_start,
                source_cursor..source.start,
                false,
            );
            if let Some(block) = block {
                items.push(DocumentLayoutItem::Block {
                    id: block.id().clone(),
                    source: source.clone(),
                });
                blocks.next();
            } else {
                let mut item = self.text_layout_item(
                    display_text,
                    display_start..display_start,
                    source.clone(),
                );
                if let DocumentLayoutItem::Text { node_id, .. } = &mut item {
                    *node_id = Some(region.id().clone());
                }
                items.push(item);
            }
            display_cursor = display_end;
            source_cursor = source.end;
        }
        self.push_text_layout_items(
            &mut items,
            display_text,
            display_cursor..display_text.len(),
            source_cursor..self.text.len(),
            true,
        );
        items
    }

    fn push_text_layout_items(
        &self,
        items: &mut Vec<DocumentLayoutItem<I>>,
        display_text: &str,
        range: Range<usize>,
        source: Range<usize>,
        include_empty_tail: bool,
    ) {
        let mut start = range.start;
        for newline in display_text[range.clone()].match_indices('\n') {
            // The line terminator separates layout items. Passing it to StyledText
            // creates another visual row inside each item and doubles line spacing.
            let end = range.start + newline.0;
            items.push(self.text_layout_item(display_text, start..end, source.clone()));
            start = end + 1;
        }
        if start < range.end || (include_empty_tail && start == range.end) {
            items.push(self.text_layout_item(display_text, start..range.end, source));
        }
    }

    fn text_layout_item(
        &self,
        display_text: &str,
        display: Range<usize>,
        segment: Range<usize>,
    ) -> DocumentLayoutItem<I> {
        // A hidden object shares display offsets with both adjacent text runs.
        // Keep the source extent so caret ownership does not cross that object.
        let source_start = self
            .display_to_source(display.start, Affinity::After)
            .unwrap_or(segment.end)
            .clamp(segment.start, segment.end);
        let source_end = self
            .display_to_source(display.end, Affinity::Before)
            .unwrap_or(segment.end)
            .clamp(source_start, segment.end);
        let source = source_start..source_end;
        let node_id = self
            .position_for_offset(source_start, Affinity::After)
            .ok()
            .map(DocumentPosition::into_node_id);
        let text_style = self
            .resolved_styles
            .paragraphs
            .iter()
            .find(|(range, _)| range.start <= display.start && display.end <= range.end)
            .map(|(_, style)| style.clone());
        let mut highlights = Vec::new();
        let mut font_family_overrides = Vec::new();
        for inline in &self.resolved_styles.inline {
            let start = display.start.max(inline.display.start);
            let end = display.end.min(inline.display.end);
            if start >= end {
                continue;
            }
            let local = start - display.start..end - display.start;
            highlights.push((local.clone(), inline.highlight));
            if let Some(font_family) = &inline.font_family {
                font_family_overrides.push((local, font_family.clone()));
            }
        }
        DocumentLayoutItem::Text {
            text: display_text[display.clone()].to_string().into(),
            display,
            source,
            node_id,
            presentation: Box::new(DocumentTextPresentation {
                text_style,
                highlights,
                font_family_overrides,
            }),
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
        let index = self
            .region_index(position.node_id())
            .ok_or(PositionError::NodeNotFound)?;
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
        // Validate the resulting regions before mutating any text or anchors.
        let mut net_delta = 0isize;
        for edit in transaction.edits() {
            let range = edit.range();
            net_delta += edit.replacement().len() as isize - range.len() as isize;
        }
        let regions = self
            .regions
            .as_slice()
            .iter()
            .enumerate()
            .map(|(index, region)| {
                let mut range = region.range();
                if index == target {
                    range.end = shift_offset(range.end, net_delta);
                } else if index > target {
                    range.start = shift_offset(range.start, net_delta);
                    range.end = shift_offset(range.end, net_delta);
                }
                DocumentRegion::new(region.id().clone(), range, region.policy())
            })
            .collect();
        let regions = DocumentRegions::new(regions, shift_offset(self.text.len(), net_delta))
            .map_err(|_| DocumentEditRejection::InvalidRegions)?;
        let mut text = self.text.clone();
        for edit in transaction.edits() {
            text.replace(edit.range(), edit.replacement());
        }
        let projection = self.projection.transformed(transaction.edits(), text.len());
        let blocks = transform_blocks(&self.blocks, transaction.edits());
        validate_projection(&projection, &regions, &text)
            .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        validate_blocks(&blocks, &regions, &projection, text.len())
            .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        let projection_map = ProjectionMap::new(&text, &projection);
        let styles = self.styles.transformed(transaction.edits());
        let resolved_styles = resolve_document_styles(&text, &regions, &projection_map, &styles)
            .map_err(|_| DocumentEditRejection::InvalidPresentation)?;
        for edit in transaction.edits() {
            self.anchors
                .apply_edit(&edit.range(), edit.replacement().len());
        }
        self.text = text;
        self.projection = projection;
        self.projection_map = projection_map;
        self.blocks = blocks;
        self.styles = styles;
        self.resolved_styles = resolved_styles;
        self.regions = regions;
        self.reconcile_anchor_nodes();
        self.revision = self.revision.next();
        Ok(())
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
                    })
            })
    }

    fn restore_active_selection(&mut self, capture: &RegionSelection<I>) {
        if let Some(index) = self.region_index(&capture.region_id) {
            let range = self.regions.as_slice()[index].range();
            self.select_in_region(
                index,
                capture.selection.anchor.min(range.len()),
                capture.selection.head.min(range.len()),
            );
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

fn resolve_document_styles<I: Eq>(
    text: &Rope,
    regions: &DocumentRegions<I>,
    projection: &ProjectionMap,
    styles: &DocumentStyles,
) -> Result<ResolvedDocumentStyles, DocumentStyleError> {
    validate_style_ranges(
        text,
        regions,
        styles.paragraphs().iter().map(|style| style.source()),
    )?;
    validate_style_ranges(
        text,
        regions,
        styles.inline().iter().map(|style| style.source()),
    )?;

    let display_text = projection.display_text();
    let mut paragraphs = Vec::with_capacity(styles.paragraphs().len());
    for style in styles.paragraphs() {
        let source = style.source();
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
        paragraphs.push((display, style.text_style().clone()));
    }

    let mut inline = Vec::with_capacity(styles.inline().len());
    for style in styles.inline() {
        let source = style.source();
        let display = projection
            .source_to_display(source.start, Affinity::Before)
            .expect("validated style start must map")
            ..projection
                .source_to_display(source.end, Affinity::After)
                .expect("validated style end must map");
        if display.is_empty() {
            continue;
        }
        inline.push(ResolvedInlineStyle {
            display,
            highlight: style.highlight(),
            font_family: style.font_family_override().cloned(),
        });
    }
    Ok(ResolvedDocumentStyles { paragraphs, inline })
}

fn validate_style_ranges<I: Eq>(
    text: &Rope,
    regions: &DocumentRegions<I>,
    ranges: impl IntoIterator<Item = Range<usize>>,
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
        for region in regions.as_slice() {
            let region_range = region.range();
            if range.start < region_range.end && region_range.start < range.end {
                match region.policy() {
                    EditPolicy::Editable => {
                        return Err(DocumentStyleError::EditableRange(range));
                    }
                    EditPolicy::Atomic => {
                        return Err(DocumentStyleError::AtomicRange(range));
                    }
                    EditPolicy::Readonly | EditPolicy::Routed => {}
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
fn closest_caret_index(
    line: &gpui::WrappedLineLayout,
    position: Point<Pixels>,
    line_height: Pixels,
) -> usize {
    let row = ((position.y / line_height).max(0.) as usize).min(line.wrap_boundaries().len());
    let boundary = |boundary: &gpui::WrapBoundary| {
        let glyph = &line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix];
        (glyph.index, glyph.position.x)
    };
    let (start, origin_x) = row
        .checked_sub(1)
        .and_then(|row| line.wrap_boundaries().get(row))
        .map(boundary)
        .unwrap_or((0, Pixels::ZERO));
    let (end, end_x) = line
        .wrap_boundaries()
        .get(row)
        .map(boundary)
        .unwrap_or((line.len(), line.unwrapped_layout.width));
    let mut closest = start;
    let mut distance = position.x.abs();
    for glyph in line.runs().iter().flat_map(|run| &run.glyphs) {
        if glyph.index < start || glyph.index >= end {
            continue;
        }
        let delta = (glyph.position.x - origin_x - position.x).abs();
        if delta < distance {
            closest = glyph.index;
            distance = delta;
        }
    }
    if (end_x - origin_x - position.x).abs() < distance {
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
        if regions.as_slice().iter().any(|region| {
            region.policy() == EditPolicy::Editable
                && span_source.start < region.range().end
                && region.range().start < span_source.end
        }) {
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
    layout_items: Vec<DocumentLayoutItem<I>>,
    list_state: ListState,
    focus_handle: FocusHandle,
    text_layouts: Vec<TextLayoutRecord>,
    block_layouts: Vec<BlockLayoutRecord>,
    trailer_layout: Option<(usize, Bounds<Pixels>)>,
    last_bounds: Option<Bounds<Pixels>>,
    layout_style: Option<(gpui::TextStyle, Pixels)>,
    auto_scroll: AutoScroll,
    undo: DocumentUndoManager<I>,
    preferred_x: Option<Pixels>,
    pending_viewport_anchor: Option<PendingViewportAnchor>,
    viewport_restore_scheduled: bool,
    dynamic_trailer: bool,
    trailer_height: Option<Pixels>,
    scroll_pin: Option<ScrollPin<I>>,
    scroll_pin_adjustment_scheduled: bool,
    caret_reveal_pending: bool,
    caret_reveal_scheduled: bool,
    scroll_handler_installed: bool,
    scrollbar_input_pending: Rc<Cell<bool>>,
    block_renderer: Option<BlockRenderer<I>>,
    text_renderer: Option<TextRenderer<I>>,
}

struct TextLayoutRecord {
    item_ix: usize,
    display: Range<usize>,
    layout: gpui::TextLayout,
    bounds: Bounds<Pixels>,
}

struct BlockLayoutRecord {
    item_ix: usize,
    source: Range<usize>,
    bounds: Bounds<Pixels>,
}

type BlockRenderer<I> = Rc<dyn Fn(&I, &mut Window, &mut App) -> AnyElement>;
type TextRenderer<I> =
    Rc<dyn Fn(DocumentTextContext<I>, AnyElement, &mut Window, &mut App) -> AnyElement>;

struct PendingViewportAnchor {
    anchor: DocumentAnchor,
    viewport_y: Pixels,
}

struct ScrollPin<I> {
    id: I,
    anchor: DocumentAnchor,
    affinity: Affinity,
    viewport_fraction: f32,
}

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

impl<I: Clone + Eq + 'static> DocumentState<I> {
    pub fn new(
        text: impl Into<String>,
        regions: Vec<DocumentRegion<I>>,
        cx: &mut Context<Self>,
    ) -> Result<Self, RegionError> {
        let model = DocumentModel::new(text, regions)?;
        let layout_items = model.layout_items();
        let list_state =
            ListState::new(layout_items.len(), ListAlignment::Top, px(400.)).measure_all();
        Ok(Self {
            model,
            layout_items,
            list_state,
            focus_handle: cx.focus_handle(),
            text_layouts: Vec::new(),
            block_layouts: Vec::new(),
            trailer_layout: None,
            last_bounds: None,
            layout_style: None,
            auto_scroll: AutoScroll::default(),
            undo: DocumentUndoManager::default(),
            preferred_x: None,
            pending_viewport_anchor: None,
            viewport_restore_scheduled: false,
            dynamic_trailer: false,
            trailer_height: None,
            scroll_pin: None,
            scroll_pin_adjustment_scheduled: false,
            caret_reveal_pending: false,
            caret_reveal_scheduled: false,
            scroll_handler_installed: false,
            scrollbar_input_pending: Rc::default(),
            block_renderer: None,
            text_renderer: None,
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
        self.reconcile_layout_items_from(0);
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
        if !snapshot.blocks.is_empty() && self.block_renderer.is_none() {
            return Err(PresentationError::Blocks(BlockError::MissingRenderer));
        }
        let mut model = DocumentModel::new(snapshot.text, snapshot.regions)
            .map_err(PresentationError::Regions)?;
        model.set_rich_presentation(snapshot.projection, snapshot.blocks, snapshot.styles)?;
        if let Some(selection) = snapshot.selection {
            model
                .replace_selection_positions(selection.clone(), selection)
                .map_err(PresentationError::Position)?;
        }
        model.revision = self.model.revision.next();
        self.model = model;
        self.auto_scroll.stop();
        self.undo = DocumentUndoManager::default();
        self.preferred_x = None;
        self.pending_viewport_anchor = None;
        self.scroll_pin = None;
        self.scrollbar_input_pending.set(false);
        self.caret_reveal_pending = false;
        self.text_layouts.clear();
        self.block_layouts.clear();
        self.trailer_layout = None;
        self.layout_items = self.next_layout_items();
        self.list_state.reset(self.layout_items.len());
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
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(PresentationError::Blocks(BlockError::MissingRenderer));
        }
        self.model.set_presentation(projection, blocks)?;
        self.reconcile_layout_items_from(0);
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
        if !blocks.is_empty() && self.block_renderer.is_none() {
            return Err(PresentationError::Blocks(BlockError::MissingRenderer));
        }
        self.model
            .set_rich_presentation(projection, blocks, styles)?;
        self.reconcile_layout_items_from(0);
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

    pub fn selected_range(&self) -> Range<usize> {
        self.model.selected_range()
    }

    pub fn region(&self, id: &I) -> Option<&DocumentRegion<I>> {
        self.model
            .region_index(id)
            .map(|index| &self.model.regions.as_slice()[index])
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

    pub fn list_state(&self) -> ListState {
        self.list_state.clone()
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
        self.reconcile_layout_items_from(0);
        cx.notify();
    }

    pub fn has_dynamic_trailer(&self) -> bool {
        self.dynamic_trailer
    }

    pub fn pin_scroll(
        &mut self,
        pin_id: I,
        position: &DocumentPosition<I>,
        viewport_fraction: f32,
        cx: &mut Context<Self>,
    ) -> Result<(), ScrollPinError> {
        if !viewport_fraction.is_finite() || !(0.0..=1.0).contains(&viewport_fraction) {
            return Err(ScrollPinError::InvalidViewportFraction);
        }
        let offset = self
            .resolve_position(position)
            .map_err(ScrollPinError::InvalidPosition)?;
        if let Some(previous) = self.scroll_pin.take() {
            self.model.anchors.remove(previous.anchor);
        }
        let anchor = self.model.anchors.create_for_node(
            offset,
            if position.affinity() == Affinity::Before {
                AnchorBias::Left
            } else {
                AnchorBias::Right
            },
            Some(position.node_id().clone()),
            position.affinity(),
        );
        self.scroll_pin = Some(ScrollPin {
            id: pin_id,
            anchor,
            affinity: position.affinity(),
            viewport_fraction,
        });
        self.caret_reveal_pending = false;
        self.clear_pending_viewport_anchor();
        cx.notify();
        Ok(())
    }

    pub fn pinned_scroll_id(&self) -> Option<&I> {
        self.scroll_pin.as_ref().map(|pin| &pin.id)
    }

    pub fn unpin_scroll(&mut self, pin_id: &I, cx: &mut Context<Self>) -> bool {
        let Some(pin) = self.scroll_pin.as_ref() else {
            return false;
        };
        if &pin.id != pin_id {
            return false;
        }
        let pin = self.scroll_pin.take().expect("scroll pin was present");
        self.model.anchors.remove(pin.anchor);
        cx.notify();
        true
    }

    pub fn reveal_position(
        &mut self,
        position: &DocumentPosition<I>,
        cx: &mut Context<Self>,
    ) -> Result<(), PositionError> {
        let offset = self.resolve_position(position)?;
        self.stop_following(cx);
        self.list_state
            .scroll_to_reveal_item(self.layout_item_for_source(offset));
        cx.notify();
        Ok(())
    }

    pub fn reveal_caret(&mut self, cx: &mut Context<Self>) {
        self.request_caret_reveal(cx);
    }

    fn stop_following(&mut self, cx: &mut Context<Self>) {
        let Some(pin) = self.scroll_pin.take() else {
            return;
        };
        self.model.anchors.remove(pin.anchor);
        cx.emit(DocumentEvent::StopFollowingRequested { pin_id: pin.id });
        cx.notify();
    }

    fn apply_scrollbar_input(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar_input_pending.replace(false) {
            self.caret_reveal_pending = false;
            if let Some(pending) = self.pending_viewport_anchor.take() {
                self.model.anchors.remove(pending.anchor);
            }
            self.stop_following(cx);
        }
    }

    pub fn remeasure_block(&mut self, id: &I, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.layout_items.iter().position(
            |item| matches!(item, DocumentLayoutItem::Block { id: block_id, .. } if block_id == id),
        ) else {
            return false;
        };
        self.capture_viewport_anchor();
        self.list_state.remeasure_items(index..index + 1);
        cx.notify();
        true
    }

    fn layout_item_for_source(&self, source: usize) -> usize {
        self.layout_items
            .iter()
            .position(|item| match item {
                DocumentLayoutItem::Text { source: text, .. } => source <= text.end,
                DocumentLayoutItem::Block { source: block, .. } => source <= block.end,
                DocumentLayoutItem::Trailer => true,
            })
            .unwrap_or(self.layout_items.len())
    }

    fn reconcile_layout_items_from(&mut self, first_changed_item: usize) {
        let next = self.next_layout_items();
        let mut prefix = 0;
        while prefix < first_changed_item
            && prefix < self.layout_items.len()
            && prefix < next.len()
            && self.layout_items[prefix].same_layout_identity(&next[prefix])
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < self.layout_items.len().saturating_sub(prefix)
            && suffix < next.len().saturating_sub(prefix)
            && self.layout_items[self.layout_items.len() - 1 - suffix]
                .same_layout_identity(&next[next.len() - 1 - suffix])
        {
            suffix += 1;
        }
        let old_end = self.layout_items.len() - suffix;
        let replacement_count = next.len() - prefix - suffix;
        if prefix != old_end || replacement_count != 0 {
            self.list_state.splice(prefix..old_end, replacement_count);
            // GPUI 0.3.5 splice does not re-arm measure_all. Keep retained
            // measurements, but measure every inserted/replaced item before
            // publishing the next scroll extent, including offscreen items.
            self.list_state.clone().measure_all();
        }
        self.layout_items = next;
    }

    fn next_layout_items(&self) -> Vec<DocumentLayoutItem<I>> {
        let mut items = self.model.layout_items();
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
        self.model.position_for_offset(offset, affinity)
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
        if reversed {
            self.model.replace_selection(end, start);
        } else {
            self.model.replace_selection(start, end);
        }
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

    fn commit_user_transaction(
        &mut self,
        transaction: EditTransaction,
        composition_selection: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> EditDecision<I> {
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
        match self.model.route_and_apply(&transaction, None) {
            RoutingOutcome::Applied => self.reconcile_layout_items_from(first_changed_item),
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
                    region_id,
                    changes,
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
        self.request_caret_reveal(cx);
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
        self.apply_host_transaction_inner(transaction, regions, None, None, None, cx)
    }

    pub fn apply_host_transaction_with_projection(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: DocumentProjection,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
        self.apply_host_transaction_inner(transaction, regions, Some(projection), None, None, cx)
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
            Some(styles),
            cx,
        )
    }

    fn apply_host_transaction_inner(
        &mut self,
        transaction: EditTransaction,
        regions: Vec<DocumentRegion<I>>,
        projection: Option<DocumentProjection>,
        blocks: Option<Vec<DocumentBlock<I>>>,
        styles: Option<DocumentStyles>,
        cx: &mut Context<Self>,
    ) -> Result<(), HostTransactionError> {
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
        validate_blocks(
            &next_blocks,
            &next_regions,
            &next_projection,
            next_text.len(),
        )
        .map_err(HostTransactionError::InvalidBlocks)?;
        let next_projection_map = ProjectionMap::new(&next_text, &next_projection);
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
        )
        .map_err(HostTransactionError::InvalidStyles)?;
        let first_changed_item = transaction
            .edits()
            .iter()
            .map(|edit| self.layout_item_for_source(edit.range().start))
            .min()
            .unwrap_or(0);

        self.capture_viewport_anchor();
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
        self.model.styles = next_styles;
        self.model.resolved_styles = next_resolved_styles;
        self.model.revision = self.model.revision.next();
        self.reconcile_layout_items_from(first_changed_item);
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
        // Replay into a tentative model, so a missing node, changed source or
        // invalid presentation rejects the whole record before any live write.
        let mut model = self.model.clone();
        let origin = if undo {
            EditOrigin::Undo
        } else {
            EditOrigin::Redo
        };
        let changes: Box<dyn Iterator<Item = &UndoChange>> = if undo {
            Box::new(record.changes.iter().rev())
        } else {
            Box::new(record.changes.iter())
        };
        let mut first_changed = self.model.text.len();
        for change in changes {
            let Some(index) = model.region_index(&record.region_id) else {
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
                model.route_and_apply(&transaction, Some(&record.region_id)),
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
        self.model
            .text
            .clip_offset(offset.saturating_sub(1), Bias::Left)
    }

    fn next_boundary(&self, offset: usize) -> usize {
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
        self.request_caret_reveal(cx);
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

    fn text_position_for_point(
        &self,
        record: &TextLayoutRecord,
        point: Point<Pixels>,
    ) -> Option<DocumentPosition<I>> {
        let DocumentLayoutItem::Text {
            source, node_id, ..
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
            record.layout.line_layout_for_index(0).map_or(0, |line| {
                closest_caret_index(
                    &line,
                    point - record.bounds.origin,
                    record.layout.line_height(),
                )
            })
        };
        let affinity = if local == record.display.len() && local > 0 {
            Affinity::Before
        } else {
            Affinity::After
        };
        let offset = self
            .model
            .display_to_source(record.display.start + local, affinity)
            .expect("laid out display positions map to source")
            .clamp(source.start, source.end);
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
        self.stop_following(cx);
        self.clear_pending_viewport_anchor();
        self.caret_reveal_pending = false;
        self.focus_handle.focus(window, cx);
        let Some(position) = self.position_for_point(event.position) else {
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
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.auto_scroll.stop();
            return;
        }
        if self.auto_scroll.last_drag_position.is_none() {
            return;
        }
        self.auto_scroll.last_drag_position = Some(event.position);
        self.extend_pointer_selection(cx);
        let delta = AutoScroll::compute_delta(event.position.y, self.list_state.viewport_bounds());
        self.auto_scroll.set(delta, cx, |delta, state, cx| {
            state.list_state.scroll_by(delta);
            // Resolve the pointer again after prepaint supplies the newly
            // visible rows, not against the previous frame's text geometry.
            cx.notify();
        });
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.button == gpui::MouseButton::Left {
            self.auto_scroll.stop();
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

    fn capture_viewport_anchor(&mut self) {
        if self.pending_viewport_anchor.is_some()
            || self.scroll_pin.is_some()
            || self.caret_reveal_pending
        {
            return;
        }
        if self.text_layouts.is_empty() && self.block_layouts.is_empty() {
            return;
        }
        let viewport = self.list_state.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return;
        }
        let probe = point(viewport.left(), viewport.top() + gpui::px(1.));
        let Some(position) = self.position_for_point(probe) else {
            return;
        };
        let Ok(offset) = self.resolve_position(&position) else {
            return;
        };
        let viewport_y = self
            .text_position_for_position(&position)
            .map_or(probe.y, |(point, _, _, _)| point.y);
        self.pending_viewport_anchor = Some(PendingViewportAnchor {
            anchor: self.model.anchors.create_for_node(
                offset,
                AnchorBias::Right,
                Some(position.node_id().clone()),
                position.affinity(),
            ),
            viewport_y,
        });
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
        let offset = self.resolve_position(position).ok()?;
        let mut fallback = None;
        let mut owned = None;
        for (ix, item) in self.layout_items.iter().enumerate() {
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
        let source = self.resolve_position(position).ok()?;
        let display = self.model.source_to_display(source, position.affinity())?;
        let item_ix = self.text_item_for_position(position)?;
        let record = self
            .text_layouts
            .iter()
            .find(|record| record.item_ix == item_ix)?;
        let local = display.clamp(record.display.start, record.display.end) - record.display.start;
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
        let mut matches =
            self.layout_items
                .iter()
                .enumerate()
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

    fn restore_pending_viewport_anchor(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_viewport_anchor.as_ref() else {
            return;
        };
        let Ok(position) = self.model.position_for_anchor(pending.anchor) else {
            return;
        };
        let Some((position, _, _, _)) = self.text_position_for_position(&position) else {
            return;
        };
        let pending = self
            .pending_viewport_anchor
            .take()
            .expect("pending viewport anchor was present");
        self.list_state.scroll_by(position.y - pending.viewport_y);
        self.model.anchors.remove(pending.anchor);
        cx.notify();
    }

    fn schedule_pending_viewport_restore(&mut self, cx: &mut Context<Self>) {
        if self.pending_viewport_anchor.is_none() || self.viewport_restore_scheduled {
            return;
        }
        self.viewport_restore_scheduled = true;
        let entity = cx.entity();
        cx.defer(move |cx| {
            entity.update(cx, |state, cx| {
                state.viewport_restore_scheduled = false;
                state.restore_pending_viewport_anchor(cx);
            });
        });
    }

    fn schedule_scroll_pin_adjustment(&mut self, cx: &mut Context<Self>) {
        if self.scroll_pin.is_none() || self.scroll_pin_adjustment_scheduled {
            return;
        }
        self.scroll_pin_adjustment_scheduled = true;
        let entity = cx.entity();
        cx.defer(move |cx| {
            entity.update(cx, |state, cx| {
                state.scroll_pin_adjustment_scheduled = false;
                state.enforce_scroll_pin(cx);
            });
        });
    }

    fn request_caret_reveal(&mut self, cx: &mut Context<Self>) {
        self.stop_following(cx);
        self.clear_pending_viewport_anchor();
        self.caret_reveal_pending = true;
        // Edits invalidate the current line's measurement. Decide whether to
        // scroll only after the new layout, otherwise every insertion at EOF
        // looks like a caret outside the viewport.
        cx.notify();
    }

    fn clear_pending_viewport_anchor(&mut self) {
        if let Some(pending) = self.pending_viewport_anchor.take() {
            self.model.anchors.remove(pending.anchor);
        }
    }

    fn schedule_caret_reveal(&mut self, cx: &mut Context<Self>) {
        if !self.caret_reveal_pending || self.caret_reveal_scheduled {
            return;
        }
        self.caret_reveal_scheduled = true;
        let entity = cx.entity();
        cx.defer(move |cx| {
            entity.update(cx, |state, cx| {
                state.caret_reveal_scheduled = false;
                state.enforce_caret_reveal(cx);
            });
        });
    }

    fn enforce_caret_reveal(&mut self, cx: &mut Context<Self>) {
        if !self.caret_reveal_pending {
            return;
        }
        let Ok(caret) = self.model.position_for_anchor(self.model.selection.head()) else {
            return;
        };
        let Some((position, line_height, _, _)) = self.text_position_for_position(&caret) else {
            if let Some(item_ix) = self.text_item_for_position(&caret) {
                if self.list_state.bounds_for_item(item_ix).is_some() {
                    self.list_state.scroll_to_reveal_item(item_ix);
                } else {
                    self.list_state.scroll_to(ListOffset {
                        item_ix,
                        offset_in_item: Pixels::ZERO,
                    });
                }
                cx.notify();
            }
            return;
        };
        let viewport = self.list_state.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return;
        }
        self.caret_reveal_pending = false;
        let top_limit = viewport.top() + line_height;
        let bottom_limit = viewport.bottom() - line_height;
        let delta = if position.y < top_limit {
            position.y - top_limit
        } else if position.y + line_height > bottom_limit {
            position.y + line_height - bottom_limit
        } else {
            Pixels::ZERO
        };
        if delta.abs() >= px(0.5) {
            self.list_state.scroll_by(delta);
            cx.notify();
        }
    }

    fn enforce_scroll_pin(&mut self, cx: &mut Context<Self>) {
        self.apply_scrollbar_input(cx);
        let Some((anchor, affinity, viewport_fraction)) = self
            .scroll_pin
            .as_ref()
            .map(|pin| (pin.anchor, pin.affinity, pin.viewport_fraction))
        else {
            return;
        };
        let Some(offset) = self.model.anchors.resolve(anchor) else {
            return;
        };
        let Some(display) = self.model.source_to_display(offset, affinity) else {
            return;
        };
        let position = self
            .block_layouts
            .iter()
            .find(|record| {
                if affinity == Affinity::Before {
                    record.source.start < offset && offset <= record.source.end
                } else {
                    record.source.start <= offset && offset < record.source.end
                }
            })
            .map(|record| {
                point(
                    record.bounds.left(),
                    if offset == record.source.end {
                        record.bounds.bottom()
                    } else {
                        record.bounds.top()
                    },
                )
            })
            .or_else(|| {
                self.text_position_for_display(display, affinity)
                    .map(|(position, _, _, _)| position)
            });
        let Some(position) = position else {
            let item_ix = self
                .text_item_for_display(display, affinity)
                .unwrap_or_else(|| self.layout_item_for_source(offset));
            if self.list_state.logical_scroll_top().item_ix != item_ix {
                self.list_state.scroll_to(ListOffset {
                    item_ix,
                    offset_in_item: Pixels::ZERO,
                });
                cx.notify();
            }
            return;
        };
        let viewport = self.list_state.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return;
        }
        let target_y = viewport.top() + viewport.size.height * viewport_fraction;
        let delta = position.y - target_y;
        if delta.abs() < px(0.5) {
            return;
        }
        let top = self.list_state.logical_scroll_top();
        if delta < Pixels::ZERO && top.item_ix == 0 && top.offset_in_item <= Pixels::ZERO {
            return;
        }
        if delta > Pixels::ZERO
            && self
                .list_state
                .bounds_for_item(self.layout_items.len().saturating_sub(1))
                .is_some_and(|bounds| bounds.bottom() <= viewport.bottom())
        {
            return;
        }
        self.list_state.scroll_by(delta);
        cx.notify();
    }

    pub(super) fn update_text_layout(
        &mut self,
        item_ix: usize,
        display: Range<usize>,
        layout: gpui::TextLayout,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
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
        self.schedule_pending_viewport_restore(cx);
        self.schedule_scroll_pin_adjustment(cx);
        self.schedule_caret_reveal(cx);
    }

    pub(super) fn update_block_layout(
        &mut self,
        item_ix: usize,
        _id: I,
        source: Range<usize>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
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
        self.schedule_pending_viewport_restore(cx);
        self.schedule_scroll_pin_adjustment(cx);
        self.schedule_caret_reveal(cx);
    }

    pub(super) fn prepare_layout(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_scrollbar_input(cx);
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
            self.capture_viewport_anchor();
        }
        if style_changed {
            self.list_state.remeasure_items(0..self.layout_items.len());
        }
        self.layout_style = Some(layout_style);
        self.last_bounds = Some(bounds);
        if self.dynamic_trailer
            && self.trailer_height != Some(bounds.size.height)
            && bounds.size.height > Pixels::ZERO
        {
            // The viewport is known before List::prepaint measures its items.
            // Include the actual trailer height in this frame's total.
            self.trailer_height = Some(bounds.size.height);
            if let Some(index) = self
                .layout_items
                .iter()
                .position(|item| matches!(item, DocumentLayoutItem::Trailer))
            {
                self.list_state.remeasure_items(index..index + 1);
            }
        }
        self.text_layouts.clear();
        self.block_layouts.clear();
        self.trailer_layout = None;
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
        let child = match self
            .layout_items
            .get(item_ix)
            .expect("GPUI list requested a valid document item")
        {
            DocumentLayoutItem::Text {
                display,
                source,
                node_id,
                text,
                presentation,
            } => {
                if let Some(node_id) = node_id {
                    let region = self.region(node_id).map(DocumentRegion::range);
                    text_context = Some(DocumentTextContext {
                        node_id: node_id.clone(),
                        source: source.clone(),
                        first_line: region.is_some_and(|region| source.start <= region.start),
                    });
                }
                DocumentChild::Text {
                    item_ix,
                    display: display.clone(),
                    text: text.clone(),
                    presentation: presentation.clone(),
                }
            }
            DocumentLayoutItem::Block { id, source } => {
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
            DocumentLayoutItem::Trailer => DocumentChild::Trailer {
                item_ix,
                height: self
                    .trailer_height
                    .unwrap_or_else(|| window.viewport_size().height),
            },
        };
        let element = DocumentElement::render_child(cx.entity(), child);
        match (&self.text_renderer, text_context) {
            (Some(renderer), Some(context)) => renderer(context, element, window, cx),
            _ => element,
        }
    }
}

impl<I: Clone + Eq + 'static> EventEmitter<DocumentEvent<I>> for DocumentState<I> {}

impl<I: Clone + Eq + 'static> Focusable for DocumentState<I> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<I: Clone + Eq + 'static> EntityInputHandler for DocumentState<I> {
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

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.model.set_marked_range(None);
        self.undo.finish_composition();
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

impl<I: Clone + Eq + 'static> Render for DocumentState<I> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let entity = cx.entity();
        if !self.scroll_handler_installed {
            self.scroll_handler_installed = true;
            let weak = entity.downgrade();
            self.list_state.set_scroll_handler(move |event, _, cx| {
                if !event.is_scrolled {
                    return;
                }
                let Some(entity) = weak.upgrade() else {
                    return;
                };
                entity.update(cx, |state, cx| {
                    state.caret_reveal_pending = false;
                    state.stop_following(cx);
                });
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
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Entity, FontWeight, IntoElement, Modifiers, MouseButton, ScrollDelta,
        ScrollWheelEvent, TestAppContext, VisualTestContext, point, px,
    };

    use crate::{Theme, document::ProjectionSpan};

    struct DocumentRoot(Entity<DocumentState<&'static str>>);

    impl Render for DocumentRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn document_view(
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
                assert!(document.caret_reveal_pending);
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
                document.apply_scrollbar_input(cx);
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
                    region_id: "draft",
                    changes: vec![UndoChange {
                        range: 0..0,
                        removed: String::new(),
                        inserted: text.into(),
                    }],
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
        assert_eq!(history.undo[0].changes[0].inserted, "dddd");
        history.trim(1, 3);
        assert!(history.undo.is_empty());
        let mut reserved = String::with_capacity(1024);
        reserved.push('x');
        history.record(
            UndoRecord {
                region_id: "draft",
                changes: vec![UndoChange {
                    range: 0..0,
                    removed: String::new(),
                    inserted: reserved,
                }],
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
                document.request_caret_reveal(cx);
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
                let line = body.layout.line_layout_for_index(0).unwrap();
                let boundary = line.wrap_boundaries[0];
                let wrap = line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index;
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
            assert_eq!(
                presentation.text_style.as_ref().unwrap().font_size,
                Some(px(24.).into())
            );
            let DocumentLayoutItem::Text { presentation, .. } = &document.layout_items[1] else {
                panic!("second item must be text");
            };
            assert_eq!(presentation.highlights.len(), 1);
            assert_eq!(presentation.highlights[0].0, 6..10);
            assert_eq!(
                presentation.highlights[0].1.font_weight,
                Some(FontWeight::BOLD)
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
