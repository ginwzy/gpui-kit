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
    Scrollbar, ScrollbarHandle,
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
    projection::ProjectionMap,
};

const DOCUMENT_INPUT_CONTEXT: &str = "Input";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentEditRejection {
    StaleRevision,
    OutsideRegion,
    CrossesRegions,
    Readonly,
    Atomic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostTransactionError {
    InvalidOrigin,
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

#[derive(Debug)]
struct AnchorRecord {
    offset: usize,
    bias: AnchorBias,
}

#[derive(Debug, Default)]
struct AnchorStore {
    next_id: u64,
    anchors: BTreeMap<DocumentAnchor, AnchorRecord>,
}

impl AnchorStore {
    fn create(&mut self, offset: usize, bias: AnchorBias) -> DocumentAnchor {
        let anchor = DocumentAnchor::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.anchors.insert(anchor, AnchorRecord { offset, bias });
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

fn transform_offset(
    offset: usize,
    bias: AnchorBias,
    range: &Range<usize>,
    replacement_len: usize,
) -> usize {
    if range.is_empty() {
        return if offset < range.start {
            offset
        } else if offset > range.start {
            shift_offset(offset, replacement_len as isize)
        } else if bias == AnchorBias::Right {
            range.start + replacement_len
        } else {
            range.start
        };
    }

    if offset < range.start {
        offset
    } else if offset > range.end {
        shift_offset(offset, replacement_len as isize - range.len() as isize)
    } else if offset == range.end {
        range.start + replacement_len
    } else if bias == AnchorBias::Right {
        range.start + replacement_len
    } else {
        range.start
    }
}

fn shift_offset(offset: usize, delta: isize) -> usize {
    if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs())
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

#[derive(Clone, Debug)]
struct UndoRecord<I> {
    region_id: I,
    before: String,
    after: String,
    selection_before: RelativeSelection,
    selection_after: RelativeSelection,
}

#[derive(Debug)]
struct DocumentUndoManager<I> {
    undo: Vec<UndoRecord<I>>,
    redo: Vec<UndoRecord<I>>,
    composition_open: bool,
}

impl<I> Default for DocumentUndoManager<I> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            composition_open: false,
        }
    }
}

impl<I: Clone + Eq> DocumentUndoManager<I> {
    fn record(&mut self, record: UndoRecord<I>, origin: EditOrigin) {
        if origin == EditOrigin::Composition
            && self.composition_open
            && let Some(previous) = self.undo.last_mut()
            && previous.region_id == record.region_id
        {
            previous.after = record.after;
            previous.selection_after = record.selection_after;
        } else {
            self.undo.push(record);
        }
        self.composition_open = origin == EditOrigin::Composition;
        self.redo.clear();
    }

    fn finish_composition(&mut self) {
        self.composition_open = false;
    }
}

struct DocumentModel<I> {
    text: Rope,
    revision: DocumentRevision,
    regions: DocumentRegions<I>,
    anchors: AnchorStore,
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
                    presentation: left_presentation,
                    ..
                },
                Self::Text {
                    text: right,
                    presentation: right_presentation,
                    ..
                },
            ) => left == right && left_presentation == right_presentation,
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
        let initial_offset = regions
            .as_slice()
            .iter()
            .rev()
            .find(|region| region.policy() == EditPolicy::Editable)
            .map(|region| region.range().start)
            .unwrap_or(0);
        let mut anchors = AnchorStore::default();
        let cursor = anchors.create(initial_offset, AnchorBias::Right);
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
        let mut items = Vec::new();
        for block in &self.blocks {
            let source = block.source();
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
                false,
            );
            items.push(DocumentLayoutItem::Block {
                id: block.id().clone(),
                source,
            });
            display_cursor = display_end;
        }
        self.push_text_layout_items(
            &mut items,
            display_text,
            display_cursor..display_text.len(),
            true,
        );
        items
    }

    fn push_text_layout_items(
        &self,
        items: &mut Vec<DocumentLayoutItem<I>>,
        display_text: &str,
        range: Range<usize>,
        include_empty_tail: bool,
    ) {
        let mut start = range.start;
        for newline in display_text[range.clone()].match_indices('\n') {
            let end = range.start + newline.0 + 1;
            items.push(self.text_layout_item(display_text, start..end));
            start = end;
        }
        if start < range.end || (include_empty_tail && start == range.end) {
            items.push(self.text_layout_item(display_text, start..range.end));
        }
    }

    fn text_layout_item(&self, display_text: &str, display: Range<usize>) -> DocumentLayoutItem<I> {
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

    fn replace_selection(&mut self, anchor_offset: usize, head_offset: usize) {
        self.release_selection(self.selection);
        let anchor = self.anchors.create(anchor_offset, AnchorBias::Left);
        let head = self.anchors.create(head_offset, AnchorBias::Right);
        self.selection = DocumentSelection::new(anchor, head);
    }

    fn collapse_selection(&mut self, offset: usize) {
        self.release_selection(self.selection);
        let cursor = self.anchors.create(offset, AnchorBias::Right);
        self.selection = DocumentSelection::new(cursor, cursor);
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
            let start = self.anchors.create(range.start, AnchorBias::Left);
            let end = self.anchors.create(range.end, AnchorBias::Right);
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

    fn route_and_apply(&mut self, transaction: &EditTransaction) -> RoutingOutcome<I> {
        if transaction.revision() != self.revision {
            return RoutingOutcome::Rejected(DocumentEditRejection::StaleRevision);
        }

        let target = match self.target_region_for_transaction(transaction) {
            Ok(target) => target,
            Err(reason) => return RoutingOutcome::Rejected(reason),
        };
        let region = &self.regions.as_slice()[target];
        match region.policy() {
            EditPolicy::Readonly => RoutingOutcome::Rejected(DocumentEditRejection::Readonly),
            EditPolicy::Atomic => RoutingOutcome::Rejected(DocumentEditRejection::Atomic),
            EditPolicy::Routed => RoutingOutcome::Routed(region.id().clone()),
            EditPolicy::Editable => {
                self.apply_to_editable_region(target, transaction);
                RoutingOutcome::Applied
            }
        }
    }

    fn target_region_for_transaction(
        &self,
        transaction: &EditTransaction,
    ) -> Result<usize, DocumentEditRejection> {
        let mut target = None;
        for edit in transaction.edits() {
            let Some(index) = self.region_for_range(&edit.range()) else {
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

    fn apply_to_editable_region(&mut self, target: usize, transaction: &EditTransaction) {
        let mut net_delta = 0isize;
        for edit in transaction.edits() {
            let range = edit.range();
            let replacement_len = edit.replacement().len();
            self.anchors.apply_edit(&range, replacement_len);
            self.text.replace(range.clone(), edit.replacement());
            net_delta += replacement_len as isize - range.len() as isize;
        }
        self.projection = self
            .projection
            .transformed(transaction.edits(), self.text.len());
        self.blocks = transform_blocks(&self.blocks, transaction.edits());
        self.projection_map = ProjectionMap::new(&self.text, &self.projection);

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
        self.regions = DocumentRegions::new(regions, self.text.len())
            .expect("an in-region edit must preserve region invariants");
        self.revision = self.revision.next();
    }

    fn editable_range_for_cursor(&self) -> Option<Range<usize>> {
        let cursor = self.cursor();
        let index = self.region_for_range(&(cursor..cursor))?;
        let region = &self.regions.as_slice()[index];
        (region.policy() == EditPolicy::Editable).then(|| region.range())
    }

    fn editable_group_start_for_cursor(&self) -> Option<usize> {
        let cursor = self.cursor();
        let mut index = self.region_for_range(&(cursor..cursor))?;
        if self.regions.as_slice()[index].policy() != EditPolicy::Editable {
            return None;
        }
        while index > 0
            && matches!(
                self.regions.as_slice()[index - 1].policy(),
                EditPolicy::Editable | EditPolicy::Atomic
            )
        {
            index -= 1;
        }
        Some(self.regions.as_slice()[index].range().start)
    }

    fn region_index(&self, id: &I) -> Option<usize> {
        self.regions
            .as_slice()
            .iter()
            .position(|region| region.id() == id)
    }

    fn region_text(&self, index: usize) -> String {
        self.text
            .slice(self.regions.as_slice()[index].range())
            .to_string()
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
            self.replace_selection(
                range.start + capture.selection.anchor.min(range.len()),
                range.start + capture.selection.head.min(range.len()),
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
    drag_anchor: Option<usize>,
    undo: DocumentUndoManager<I>,
    pending_viewport_anchor: Option<PendingViewportAnchor>,
    viewport_restore_scheduled: bool,
    dynamic_trailer: bool,
    trailer_height: Option<Pixels>,
    pending_trailer_height: Option<Pixels>,
    trailer_resize_scheduled: bool,
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
        let list_state = ListState::new(layout_items.len(), ListAlignment::Top, px(400.))
            .with_uniform_item_height(px(21.));
        Ok(Self {
            model,
            layout_items,
            list_state,
            focus_handle: cx.focus_handle(),
            text_layouts: Vec::new(),
            block_layouts: Vec::new(),
            trailer_layout: None,
            last_bounds: None,
            drag_anchor: None,
            undo: DocumentUndoManager::default(),
            pending_viewport_anchor: None,
            viewport_restore_scheduled: false,
            dynamic_trailer: false,
            trailer_height: None,
            pending_trailer_height: None,
            trailer_resize_scheduled: false,
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
            let index = model
                .region_index(selection.node_id())
                .ok_or(PresentationError::Position(PositionError::NodeNotFound))?;
            let region = &model.regions.as_slice()[index];
            if selection.offset() > region.range().len() {
                return Err(PresentationError::Position(
                    PositionError::NodeOffsetOutOfBounds {
                        offset: selection.offset(),
                        node_len: region.range().len(),
                    },
                ));
            }
            let offset = region.range().start + selection.offset();
            let bias = match selection.affinity() {
                Affinity::Before => AnchorBias::Left,
                Affinity::After => AnchorBias::Right,
            };
            let cursor = model.anchors.create(offset, bias);
            model.selection = DocumentSelection::new(cursor, cursor);
        }
        self.model = model;
        self.undo = DocumentUndoManager::default();
        self.pending_viewport_anchor = None;
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
        let (anchor, head) = self.model.selection_offsets();
        Ok((
            self.position_for_offset(anchor, Affinity::Before)?,
            self.position_for_offset(head, Affinity::After)?,
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
            self.pending_trailer_height = None;
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
        let anchor = self.model.anchors.create(
            offset,
            if position.affinity() == Affinity::Before {
                AnchorBias::Left
            } else {
                AnchorBias::Right
            },
        );
        self.scroll_pin = Some(ScrollPin {
            id: pin_id,
            anchor,
            viewport_fraction,
        });
        self.list_state.scroll_to(ListOffset {
            item_ix: self.layout_item_for_source(offset),
            offset_in_item: Pixels::ZERO,
        });
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
        self.list_state.remeasure_items(index..index + 1);
        cx.notify();
        true
    }

    fn layout_item_for_source(&self, source: usize) -> usize {
        self.layout_items
            .iter()
            .position(|item| match item {
                DocumentLayoutItem::Text { display, .. } => {
                    let end = self
                        .model
                        .display_to_source(display.end, Affinity::After)
                        .unwrap_or(self.model.text.len());
                    source <= end
                }
                DocumentLayoutItem::Block { source: block, .. } => source <= block.end,
                DocumentLayoutItem::Trailer => true,
            })
            .unwrap_or(self.layout_items.len())
    }

    fn layout_item_for_source_after(&self, source: usize) -> usize {
        self.layout_items
            .iter()
            .position(|item| match item {
                DocumentLayoutItem::Text { display, .. } => {
                    let start = self
                        .model
                        .display_to_source(display.start, Affinity::After)
                        .unwrap_or(0);
                    let end = self
                        .model
                        .display_to_source(display.end, Affinity::After)
                        .unwrap_or(self.model.text.len());
                    (start <= source && source < end) || (start == end && source == start)
                }
                DocumentLayoutItem::Block { source: block, .. } => {
                    (block.start <= source && source < block.end)
                        || (block.is_empty() && source == block.start)
                }
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
        self.list_state.splice(prefix..old_end, replacement_count);
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
        if offset > self.model.text.len() {
            return Err(PositionError::SourceOffsetOutOfBounds {
                offset,
                source_len: self.model.text.len(),
            });
        }
        let mut candidates = self.model.regions.as_slice().iter().filter(|region| {
            let range = region.range();
            range.start <= offset && offset <= range.end
        });
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

    pub fn resolve_position(&self, position: &DocumentPosition<I>) -> Result<usize, PositionError> {
        let index = self
            .model
            .region_index(position.node_id())
            .ok_or(PositionError::NodeNotFound)?;
        let range = self.model.regions.as_slice()[index].range();
        if position.offset() > range.len() {
            return Err(PositionError::NodeOffsetOutOfBounds {
                offset: position.offset(),
                node_len: range.len(),
            });
        }
        Ok(range.start + position.offset())
    }

    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool, cx: &mut Context<Self>) {
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

    pub fn apply_transaction(
        &mut self,
        transaction: EditTransaction,
        cx: &mut Context<Self>,
    ) -> EditDecision<I> {
        let first_changed_item = transaction
            .edits()
            .iter()
            .map(|edit| self.layout_item_for_source(edit.range().start))
            .min()
            .unwrap_or(0);
        match self.model.route_and_apply(&transaction) {
            RoutingOutcome::Applied => {
                self.reconcile_layout_items_from(first_changed_item);
                let revision = self.model.revision;
                let origin = transaction.origin();
                cx.emit(DocumentEvent::Changed { revision, origin });
                cx.notify();
                EditDecision::Apply
            }
            RoutingOutcome::Routed(region_id) => {
                cx.emit(DocumentEvent::Routed(RoutedEdit {
                    region_id: region_id.clone(),
                    transaction,
                }));
                EditDecision::Route(region_id)
            }
            RoutingOutcome::Rejected(reason) => {
                cx.emit(DocumentEvent::Rejected(reason));
                EditDecision::Reject
            }
        }
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
        let undo_seed = if matches!(
            origin,
            EditOrigin::User | EditOrigin::Paste | EditOrigin::Composition
        ) {
            self.model
                .target_region_for_transaction(&transaction)
                .ok()
                .filter(|index| {
                    self.model.regions.as_slice()[*index].policy() == EditPolicy::Editable
                })
                .and_then(|index| {
                    self.model
                        .capture_selection_in_region(self.model.selection, index)
                        .map(|selection| {
                            (
                                self.model.regions.as_slice()[index].id().clone(),
                                self.model.region_text(index),
                                selection,
                            )
                        })
                })
        } else {
            None
        };

        let decision = self.apply_transaction(transaction, cx);
        if decision == EditDecision::Apply {
            self.model.collapse_selection(range.start + text.len());
            if let Some((region_id, before, selection_before)) = undo_seed
                && let Some(index) = self.model.region_index(&region_id)
                && let Some(selection_after) = self
                    .model
                    .capture_selection_in_region(self.model.selection, index)
            {
                self.undo.record(
                    UndoRecord {
                        region_id,
                        before,
                        after: self.model.region_text(index),
                        selection_before,
                        selection_after,
                    },
                    origin,
                );
            }
            self.request_caret_reveal(cx);
            cx.emit(DocumentEvent::SelectionChanged);
            cx.notify();
        }
        decision
    }

    fn restore_undo_record(
        &mut self,
        record: &UndoRecord<I>,
        undo: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.model.region_index(&record.region_id) else {
            return false;
        };
        let region = &self.model.regions.as_slice()[index];
        if region.policy() != EditPolicy::Editable {
            return false;
        }
        let (expected, replacement, selection) = if undo {
            (&record.after, &record.before, record.selection_before)
        } else {
            (&record.before, &record.after, record.selection_after)
        };
        if self.model.region_text(index) != *expected {
            return false;
        }
        let range = region.range();
        let transaction = EditTransaction::new(
            self.model.revision,
            if undo {
                EditOrigin::Undo
            } else {
                EditOrigin::Redo
            },
            vec![TextEdit::new(range.clone(), replacement)],
            self.model.text.len(),
        )
        .expect("an editable region replacement is a valid transaction");
        if self.apply_transaction(transaction, cx) != EditDecision::Apply {
            return false;
        }
        let Some(index) = self.model.region_index(&record.region_id) else {
            return false;
        };
        let start = self.model.regions.as_slice()[index].range().start;
        self.model.replace_selection(
            start + selection.anchor.min(replacement.len()),
            start + selection.head.min(replacement.len()),
        );
        self.model.set_marked_range(None);
        self.request_caret_reveal(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
        true
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo.finish_composition();
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
        self.undo.finish_composition();
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
        if extend {
            let (anchor, _) = self.model.selection_offsets();
            self.model.replace_selection(anchor, offset);
        } else {
            self.model.collapse_selection(offset);
        }
        self.request_caret_reveal(cx);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn vertical_offset(&self, down: bool, page: bool) -> usize {
        let range = self.model.selected_range();
        let cursor = if range.is_empty() {
            self.model.cursor()
        } else if down {
            range.end
        } else {
            range.start
        };
        let Some(display) = self.model.source_to_display(cursor, Affinity::After) else {
            return cursor;
        };
        let Some((position, line_height, _, _)) =
            self.text_position_for_display(display, Affinity::After)
        else {
            return cursor;
        };
        let distance = if page {
            self.list_state
                .viewport_bounds()
                .size
                .height
                .max(line_height)
        } else {
            line_height
        };
        self.offset_for_point(point(
            position.x,
            if down {
                position.y + distance
            } else {
                position.y - distance
            },
        ))
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
        self.move_selection_head(self.vertical_offset(false, false), false, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.vertical_offset(true, false), false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.vertical_offset(false, false), true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.vertical_offset(true, false), true, cx);
    }

    fn move_page_up(&mut self, _: &MovePageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.vertical_offset(false, true), false, cx);
    }

    fn move_page_down(&mut self, _: &MovePageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.vertical_offset(true, true), false, cx);
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
        self.move_selection_head(0, false, cx);
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.model.text.len(), false, cx);
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
        self.move_selection_head(0, true, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_head(self.model.text.len(), true, cx);
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
            self.replace(range, "", EditOrigin::User, cx);
        }
    }

    fn delete_to_start_of_line(
        &mut self,
        _: &DeleteToBeginningOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(self.start_of_line(cursor)..cursor, "", EditOrigin::User, cx);
    }

    fn delete_to_end_of_line(
        &mut self,
        _: &DeleteToEndOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.model.cursor();
        self.replace(cursor..self.end_of_line(cursor), "", EditOrigin::User, cx);
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
            self.replace(range, "", EditOrigin::User, cx);
        }
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        self.replace(self.model.selected_range(), "\n", EditOrigin::User, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
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
        if self.replace(range, "", EditOrigin::User, cx) == EditDecision::Apply {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.replace(self.model.selected_range(), &text, EditOrigin::Paste, cx);
    }

    fn offset_for_point(&self, point: Point<Pixels>) -> usize {
        if let Some(record) = self
            .text_layouts
            .iter()
            .find(|record| record.bounds.top() <= point.y && point.y <= record.bounds.bottom())
        {
            let local = record
                .layout
                .index_for_position(point)
                .unwrap_or_else(|_| record.display.len());
            return self
                .model
                .display_to_source(record.display.start + local, Affinity::After)
                .unwrap_or(self.model.text.len());
        }
        if let Some(record) = self
            .block_layouts
            .iter()
            .find(|record| record.bounds.contains(&point))
        {
            return if point.y < record.bounds.center().y {
                record.source.start
            } else {
                record.source.end
            };
        }
        if self
            .last_bounds
            .is_some_and(|bounds| point.y < bounds.top())
        {
            0
        } else {
            self.model.text.len()
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .block_layouts
            .iter()
            .any(|block| block.bounds.contains(&event.position))
        {
            self.drag_anchor = None;
            return;
        }
        self.focus_handle.focus(window, cx);
        let offset = self.offset_for_point(event.position);
        let anchor = if event.modifiers.shift {
            self.model.selection_offsets().0
        } else {
            offset
        };
        self.drag_anchor = Some(anchor);
        self.model.replace_selection(anchor, offset);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            return;
        }
        let Some(anchor) = self.drag_anchor else {
            return;
        };
        self.model
            .replace_selection(anchor, self.offset_for_point(event.position));
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag_anchor = None;
    }

    fn capture_viewport_anchor(&mut self) {
        if self.pending_viewport_anchor.is_some() {
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
        let offset = self.offset_for_point(probe);
        let viewport_y = self
            .screen_position_for_source(offset, Affinity::After)
            .map_or(probe.y, |position| position.y);
        self.pending_viewport_anchor = Some(PendingViewportAnchor {
            anchor: self.model.anchors.create(offset, AnchorBias::Right),
            viewport_y,
        });
    }

    fn screen_position_for_source(
        &self,
        source: usize,
        affinity: Affinity,
    ) -> Option<Point<Pixels>> {
        let display = self.model.source_to_display(source, affinity)?;
        self.text_position_for_display(display, affinity)
            .map(|(position, _, _, _)| position)
    }

    fn text_position_for_display(
        &self,
        display: usize,
        affinity: Affinity,
    ) -> Option<(Point<Pixels>, Pixels, usize, Bounds<Pixels>)> {
        let mut matches = self
            .text_layouts
            .iter()
            .filter(|record| record.display.start <= display && display <= record.display.end);
        let record = if affinity == Affinity::Before {
            matches.next()
        } else {
            matches.next_back()
        }?;
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

    fn restore_pending_viewport_anchor(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_viewport_anchor.as_ref() else {
            return;
        };
        let Some(offset) = self.model.anchors.resolve(pending.anchor) else {
            return;
        };
        let Some(display) = self.model.source_to_display(offset, Affinity::After) else {
            return;
        };
        let Some((position, _, item_ix, item_bounds)) =
            self.text_position_for_display(display, Affinity::After)
        else {
            return;
        };
        let pending = self
            .pending_viewport_anchor
            .take()
            .expect("pending viewport anchor was present");
        let viewport = self.list_state.viewport_bounds();
        let offset_in_item = (position.y - item_bounds.top() + viewport.top() - pending.viewport_y)
            .max(Pixels::ZERO);
        self.list_state.scroll_to(ListOffset {
            item_ix,
            offset_in_item,
        });
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
        self.caret_reveal_pending = true;
        let cursor = self.model.cursor();
        let item_ix = self.layout_item_for_source(cursor);
        let is_laid_out = self
            .model
            .source_to_display(cursor, Affinity::After)
            .is_some_and(|display| {
                self.text_layouts
                    .iter()
                    .any(|record| record.display.contains_inclusive(display))
            })
            || self
                .block_layouts
                .iter()
                .any(|record| record.source.contains_inclusive(cursor));
        let item_is_in_viewport = self.list_state.item_is_above_viewport(item_ix) == Some(false)
            && self.list_state.item_is_below_viewport(item_ix) == Some(false);
        if !is_laid_out && !item_is_in_viewport {
            let viewport_lines = (self.list_state.viewport_bounds().size.height / px(21.))
                .max(1.)
                .floor() as usize;
            let coarse_item_ix = self
                .model
                .editable_group_start_for_cursor()
                .map(|start| self.layout_item_for_source_after(start))
                .filter(|start| item_ix.saturating_sub(*start) + 2 <= viewport_lines)
                .unwrap_or(item_ix);
            self.list_state.scroll_to(ListOffset {
                item_ix: coarse_item_ix,
                offset_in_item: Pixels::ZERO,
            });
        }
        self.schedule_caret_reveal(cx);
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
        let Some(display) = self
            .model
            .source_to_display(self.model.cursor(), Affinity::After)
        else {
            return;
        };
        let Some((position, line_height, _, _)) =
            self.text_position_for_display(display, Affinity::After)
        else {
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
        let Some((anchor, viewport_fraction)) = self
            .scroll_pin
            .as_ref()
            .map(|pin| (pin.anchor, pin.viewport_fraction))
        else {
            return;
        };
        let Some(offset) = self.model.anchors.resolve(anchor) else {
            return;
        };
        let Some(display) = self.model.source_to_display(offset, Affinity::After) else {
            return;
        };
        let Some((position, _, _, _)) = self.text_position_for_display(display, Affinity::After)
        else {
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

    pub(super) fn update_root_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.apply_scrollbar_input(cx);
        self.last_bounds = Some(bounds);
        if self.dynamic_trailer
            && self.trailer_height != Some(bounds.size.height)
            && bounds.size.height > Pixels::ZERO
        {
            self.pending_trailer_height = Some(bounds.size.height);
            self.schedule_trailer_resize(cx);
        }
    }

    pub(super) fn update_trailer_layout(&mut self, item_ix: usize, bounds: Bounds<Pixels>) {
        self.trailer_layout = Some((item_ix, bounds));
    }

    fn schedule_trailer_resize(&mut self, cx: &mut Context<Self>) {
        if self.trailer_resize_scheduled {
            return;
        }
        self.trailer_resize_scheduled = true;
        let entity = cx.entity();
        cx.defer(move |cx| {
            entity.update(cx, |state, cx| {
                state.trailer_resize_scheduled = false;
                let Some(height) = state.pending_trailer_height.take() else {
                    return;
                };
                if state.trailer_height == Some(height) {
                    return;
                }
                state.trailer_height = Some(height);
                if let Some(index) = state
                    .layout_items
                    .iter()
                    .position(|item| matches!(item, DocumentLayoutItem::Trailer))
                {
                    state.list_state.remeasure_items(index..index + 1);
                }
                cx.notify();
            });
        });
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
            .source_to_display(self.model.cursor(), Affinity::After)
            .filter(|cursor| display.start <= *cursor && *cursor <= display.end)
            .map(|cursor| cursor - display.start);
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
                text,
                presentation,
            } => {
                let source_start = self
                    .model
                    .display_to_source(display.start, Affinity::Before)
                    .unwrap_or(0);
                let source_end = self
                    .model
                    .display_to_source(display.end, Affinity::After)
                    .unwrap_or(self.model.text.len());
                if let Ok(position) = self.position_for_offset(source_start, Affinity::After) {
                    let region = self.region(position.node_id()).map(DocumentRegion::range);
                    text_context = Some(DocumentTextContext {
                        node_id: position.node_id().clone(),
                        source: source_start..source_end,
                        first_line: region.is_some_and(|region| source_start <= region.start),
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
        self.model.set_marked_range(None);
        self.replace(
            range,
            text,
            if committing_composition {
                EditOrigin::Composition
            } else {
                EditOrigin::User
            },
            cx,
        );
        if committing_composition {
            self.undo.finish_composition();
        }
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
        self.model.set_marked_range(None);
        if self.replace(range.clone(), text, EditOrigin::Composition, cx) != EditDecision::Apply {
            return;
        }

        if text.is_empty() {
            return;
        }
        let marked = range.start..range.start + text.len();
        self.model.set_marked_range(Some(marked.clone()));
        let selected = if let Some(selected) = new_selected_range_utf16 {
            let replacement = Rope::from(text);
            range.start + replacement.offset_utf16_to_offset(selected.start)
                ..range.start + replacement.offset_utf16_to_offset(selected.end)
        } else {
            marked.end..marked.end
        };
        self.model.replace_selection(selected.start, selected.end);
        cx.emit(DocumentEvent::SelectionChanged);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let source = self.range_from_utf16(&range_utf16);
        let start_display = self
            .model
            .source_to_display(source.start, Affinity::Before)?;
        let end_display = self.model.source_to_display(source.end, Affinity::After)?;
        let (start, line_height, _, _) =
            self.text_position_for_display(start_display, Affinity::Before)?;
        let (mut end, _, _, _) = self.text_position_for_display(end_display, Affinity::After)?;
        end.y = start.y;
        Some(Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
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
        let range = self.range_from_utf16(&range_utf16);
        self.model.replace_selection(range.start, range.end);
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
        self.text_layouts.clear();
        self.block_layouts.clear();
        self.trailer_layout = None;
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
            .on_mouse_up(
                gpui::MouseButton::Left,
                window.listener_for(&interaction_entity, Self::mouse_up),
            )
            .on_mouse_move(window.listener_for(&interaction_entity, Self::mouse_move))
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

    use crate::Theme;

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
            model.route_and_apply(&transaction),
            RoutingOutcome::Applied
        ));
        assert_eq!(model.text(), "history draft");
        assert_eq!(model.regions.as_slice()[0].range(), 0..7);
        assert_eq!(model.regions.as_slice()[1].range(), 7..13);
        assert_eq!(model.revision.value(), 1);
    }

    #[test]
    fn editable_group_crosses_atomic_blocks_but_stops_at_readonly_history() {
        let mut model = DocumentModel::new(
            "history\ntext\u{fffc}",
            vec![
                DocumentRegion::new("history", 0..7, EditPolicy::Readonly),
                DocumentRegion::new("separator", 7..8, EditPolicy::Readonly),
                DocumentRegion::new("draft-1", 8..12, EditPolicy::Editable),
                DocumentRegion::new("terminal", 12..15, EditPolicy::Atomic),
                DocumentRegion::new("draft-2", 15..15, EditPolicy::Editable),
            ],
        )
        .unwrap();
        model.replace_selection(15, 15);

        assert_eq!(model.editable_group_start_for_cursor(), Some(8));
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
            model.route_and_apply(&transaction),
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
            model.route_and_apply(&transaction),
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

        cx.simulate_keystrokes("ctrl-end shift-left ctrl-shift-home");

        document.read_with(&cx, |document, _| {
            assert_eq!(document.selected_range(), 0..7);
            assert_eq!(document.model.selection_offsets(), (7, 0));
        });
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
                        0.,
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
                assert_eq!(document.segment_paint_snapshot(&(0..5)).2, Some(5));
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
