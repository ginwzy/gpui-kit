# Document Editing Foundation RFC

## Status

Implementation contract for the `feat/document-editing-foundation` branch,
based on GPUI Kit `v0.6.4`. This document describes a staged extension of
`gpui-base`.

Stages 1 through 8 are implemented on this branch: the shared selection core,
public coordinate/region/transaction contracts, `DocumentState` and
`DocumentElement`, host transactions with draft-local undo, rich source/display
projection, variable-height inline blocks, a dynamic trailer, and anchored
follow pins. Jian is the first real consumer: it uses one document for readonly
history, the editable draft, annotations, and terminal input handoff, and no
longer retains a parallel textarea/timeline interaction owner.

The first consumer is a continuous session document containing immutable
history, an editable trailing draft, projected rich text, and embedded blocks.
The API remains domain-neutral: `gpui-base` does not know about messages,
comments, tools, terminals, or model turns.

### Inline atomic objects in text rows

`DocumentBlock::new` keeps its existing independent block-row behavior. With
`TextRows` enabled, a host can opt an atomic region into text flow:

```rust,ignore
DocumentBlock::new(id, source.clone())
    .with_inline_size(InlineSize::new(rems(5.), rems(1.5), rems(1.1)))
    .replaceable(true)
// The complete atomic source range projects to exactly one display marker:
ProjectionSpan::replace(source, "\u{fffc}")
```

`InlineSize` declares width, height and baseline from the top, in pixels or
rems. Its finite, nonnegative lengths are validated with the presentation.
The existing block renderer supplies the object's appearance inside the
allocated box. Base does not inspect labels, paths, domain types or theme
choices. Measured pixel sizes must be reinstalled when their typography
changes; rem sizes follow the document's rem size. Remove inline objects
before disabling `TextRows`.

`DocumentState` remains the sole owner of source, selection, anchors, undo,
wrapping tasks and derived layout. `ListState` retains item heights and scroll
geometry. Object advances join the existing resumable grapheme wrapping;
objects cannot split, and their before/after boundaries also separate text
graphemes. Each mixed row uses ordinary `StyledText` runs and flex-positioned
object boxes, aligned on a shared baseline. Their actual prepaint bounds form
one row layout used by painting, hit testing, selection, input-method bounds,
vertical navigation and reveal. Plain rows retain their existing renderer.
An oversized object stays whole and is clipped by the document viewport.

Keep editable text regions, including zero-length regions where insertion or
composition is allowed before, between or after objects. Those regions own
text input; an object never becomes an editor or a marked-text owner. Normal
left/right movement skips the entire atomic source range. Shift selection and
pointer selection include whole objects, preserving source coordinates.

Inline appearance does not itself permit deletion. Only blocks explicitly
marked `replaceable(true)` participate in cross-object range replacement:

1. A User or Paste transaction covering editable text and complete opted-in
   objects emits `DocumentEvent::ReplaceRequested(StructureReplacement<I>)`
   and returns `EditDecision::StructureRequested`, without mutating the source.
2. The host builds the requested replacement's snapshot, preserving domain
   identities outside the replacement and supplying the resulting caret, then
   calls `apply_structure_replacement`. The request exposes its transaction
   and the removed object IDs.
3. Base rejects a stale revision, a response with different text, a protected
   region or active composition. A successful response enters the existing
   structure undo stack as one edit. The host handles
   `StructureReplayRequested` through `apply_structure_replay` as before.

Readonly, routed, partially covered atomic and ordinary block regions keep
their previous protection. Local Host splices still require display-line
seams. No application history, persistence or submission representation is
introduced by this API.

`crates/base/examples/inline_document.rs` is the runnable host example:

```sh
cargo run -p gpui-base --example inline_document
cargo test -p gpui-base --example inline_document
```

It includes three generic objects, adjacent objects with an empty editable
boundary, explicit newlines, Unicode, a readonly prefix, resize and a text-size
toggle. Its test exercises typing, clipboard paste, deletion and one-step
undo/redo through the real event/host transaction path. Kernel tests exercise
shared baselines, hit testing, dragging, IME rectangles, narrow wrapping,
reveal, composition boundaries, opt-in protection and superseding pending
long-line layout. Platform IME candidate windows and high-DPI behavior still
require native-platform acceptance.

Step 1 acceptance (Jian `6382ced`, Kit `60847eb2` plus this working change):
Base's 1,096 unit tests, one integration test and the example's host test
passed (one existing ignored test). Jian workspace passed 247 tests, including
the recorded replay, with 12 ignored; Harness integration, both formatting
checks, strict Clippy and full builds passed. X11/Xvfb at scale 1 verified
760/300-pixel windows and 16/22-pixel text, exact source copy after pointer
selection, Chinese/emoji replacement, one undo/redo and insertion between
adjacent objects. The rebuilt ordinary Jian binary also passed Unicode draft
editing, SourceView copy including final LF, real bash exit and subsequent
input, existing block references, metadata v6 save and normal exit.

An exit-time leaked-handle assertion also reproduced in an unmodified Kit
baseline built with all targets; no exit-flow changes were made here. Native
acceptance uses the ordinary application build, separately from test artifacts.
The original full-check logs were in temporary storage removed by an environment
restart; final native evidence and the baseline comparison are retained at
`~/.cache/jian-inline-step1/`.

No Jian consumer opts into this API yet. Step 2 must supply reference dimensions
and rendering, project each reference to its marker, and connect the typed
replacement/replay requests to existing draft domain transactions. History
policy, metadata v6 and Harness/Pi submission formats remain outside this step.

## Problem

`InputState`, `TextareaState`, and `EditorState` share `InputBaseState`. That
engine already owns the difficult mechanics of plain-text editing:

- Rope-backed text;
- selection and cursor movement;
- IME composition;
- edit transactions and undo/redo;
- wrapping, hit testing, scrolling, and caret reveal;
- tracked editor decorations and code-editor display mapping.

It intentionally assumes one homogeneous text buffer. Its `readonly` policy
applies to the complete input, `TextElement` lays out text lines of one editor,
and edits are applied before observers receive `InputEvent::Change`. These
contracts cannot safely implement a document with immutable and editable
ranges, routed edits, variable-height blocks, or a materializing trailer.

Adding callbacks to `InputBaseState` would make ordinary inputs carry document
semantics and would still leave layout constrained by `TextElement`. The new
document system therefore sits beside the existing input facades while sharing
an extracted editing foundation.

## Ownership

The retained behavior owner is `Entity<DocumentState<D>>`. It exclusively owns:

- the logical text buffer and object placeholders;
- anchors, selections, cursor affinity, and IME composition;
- edit regions and transaction routing;
- user undo/redo and host projection transactions;
- source-to-display projection and retained block layout;
- document scroll position, reveal requests, and follow pins.

`DocumentElement` is a frame-local renderer and input adapter. It reads and
updates `DocumentState`; it does not retain a second model.

The application delegate `D` owns domain meaning and presentation callbacks.
It interprets opaque region and block IDs, handles routed commands, and renders
application blocks. Base owns editing behavior and the geometry required for
that behavior, but no product styling.

## Non-goals

The first implementation does not provide:

- collaborative or replicated editing;
- an application history store;
- a general WYSIWYG Markdown authoring model;
- product-specific comment, resend, terminal, or tool semantics;
- compatibility through an outer wrapper around `EditorState`;
- changes to the behavior or public API of existing input facades.

## Architecture

```text
gpui-base
|- editing
|  |- EditBuffer
|  |- EditSelectionSet
|  |- CompositionState
|  |- EditTransaction
|  `- UndoManager
|- input
|  `- InputBaseState<M> + TextElement<M>
`- document
   |- DocumentState<D>
   |- DocumentElement<D>
   |- DocumentMap
   |- DocumentLayout
   |- DocumentDelegate
   `- DocumentScrollState
```

The extraction rule is strict: every editing type moved out of `input` must be
used immediately by `InputBaseState`, and existing input behavior must remain
covered. No parallel unused editing implementation is introduced.

## Coordinates And Identity

The system uses three coordinate spaces and never substitutes one for another:

1. **source offsets** are UTF-8 byte offsets in the current Rope;
2. **document positions** use stable application node identity plus an offset
   and affinity;
3. **display positions** are transient projection and layout coordinates.

```rust,ignore
pub struct DocumentPosition<N> {
    node_id: N,
    offset: usize,
    affinity: Affinity,
}

pub struct DocumentAnchor {
    id: AnchorId,
}
```

`DocumentPosition` is suitable for application commands and persistence only
while its node still exists. Resolving a stale position returns an error.
`DocumentAnchor` is runtime identity that follows edits according to its bias.
Selections and scroll pins use anchors, not byte offsets or screen points.

Application node IDs and block IDs must be stable domain IDs. Array indices,
display text, and frame-local element IDs are not document identity.

Atomic non-text objects occupy one source position using an object replacement
marker. They expose before/after boundaries for caret movement and selection;
their rendered height is not encoded into the Rope.

## Regions And Edit Routing

Regions are ordered, non-overlapping source ranges with opaque IDs and one base
policy:

```rust,ignore
pub enum EditPolicy {
    Editable,
    Readonly,
    Routed,
    Atomic,
}
```

- `Editable` applies an ordinary text edit.
- `Readonly` permits cursor placement and selection but rejects mutation.
- `Routed` asks the application delegate to interpret the edit.
- `Atomic` permits selection around an object but not partial mutation through
  it.

Domain concepts do not become variants in Base. A consumer may interpret one
routed region as resend, another as comment reply, and an atomic block as an
input handoff.

Text regions may be zero-width, including between atomic blocks. Clearing text
preserves the region ID, selection ownership and undo target. Selection anchors
retain node identity and affinity alongside their tracked source offsets, so an
empty text node and an adjacent object can share an offset without sharing an
input target. `set_selection_positions` preserves that identity for application
navigation and restoration. Atomic blocks occupy an object replacement marker.

Empty editable nodes before other content have a text layout row. Hit testing,
caret paint and platform input geometry resolve through that row's source extent
and node identity. User transactions validate their resulting region ranges and
UTF-8 edit boundaries before changing text, anchors or presentation.

An edit crossing incompatible policies is rejected as one operation. Base does
not silently apply the editable subset, because that changes deletion, paste,
IME, and undo semantics depending on selection direction.

## Edit Pipeline

All user and platform edits enter one pre-mutation pipeline:

```text
UTF-16 platform range
-> resolve and normalize source ranges
-> inspect regions and object boundaries
-> Apply | Reject | Route | MaterializeAndApply
-> one atomic buffer mutation
-> update anchors, regions, composition, and undo
-> invalidate projection and layout
-> update selection
-> emit one semantic event and notify
```

```rust,ignore
pub enum EditOrigin {
    User,
    Composition,
    Paste,
    Undo,
    Redo,
    Host,
}

pub struct EditRequest {
    // private fields; constructors and readers form the public seam
}

pub enum EditDecision<R> {
    Apply,
    Reject,
    Route(R),
    MaterializeAndApply,
}
```

Routing happens before mutation. Observing `Change` and rolling text back is
not valid because the platform may already have advanced IME composition,
selection, and undo state.

`EntityInputHandler::text_input_editable_range` reports the active editable
region in UTF-16 coordinates. It supplements rather than replaces pre-edit
routing: paste, action-driven deletion, undo, and multi-range edits must follow
the same policy.

## Transactions And Undo

An `EditTransaction` contains normalized replacements against one pre-edit
revision and an origin. Ranges are disjoint and applied from highest offset to
lowest. `DocumentState` captures selection before and after an applied user
transaction; application metadata remains in opaque region IDs rather than in
the base transaction.

User edits and IME composition participate in user undo. A composition opens
one transaction, refinements replace its marked range, and commit or cancel
closes it. Rejected and routed requests preserve the marked range, selection,
revision and undo grouping. The public `apply_transaction` accepts only User,
Paste and Composition; all successful input paths finish selection and undo
before emitting change events. Host, Undo and Redo use dedicated paths.

Undo stores local removed/inserted text relative to a stable region ID and
node-relative selections. Adjacent typing and deletion coalesce until navigation,
an explicit selection, a different edit intent or a resource limit ends the
group. Composition refinements coalesce into one record. Replay validates a
tentative model before replacing live state, so a failed replay is atomic.
History retains at most 1,000 records, 1,000 changes per record and 16 MiB of
allocated text buffers across undo and redo, including unused string capacity.
Oldest records are evicted first; a single
edit exceeding the limits is applied but starts a history boundary and is not
retained. No keystroke captures a complete editable region.

Model entry points validate UTF-8 boundaries against the actual Rope for
regions, positions, user/host edits and all projection spans. Edits transform
styles, projection spans and blocks together and validate the next presentation
before writing text or anchors. An edit that destroys a required paragraph
style boundary is rejected without changing input state.

Host projection updates use a separate entry point:

```rust,ignore
document.apply_host_transaction(transaction, next_regions, cx)
```

A host transaction:

- does not enter user undo history;
- atomically moves anchors, regions, selections, and marked ranges;
- keeps outstanding undo records relative to stable editable region IDs, so a
  later user undo still addresses the same editable content;
- preserves or deliberately updates the current scroll pin;
- emits one projection-change event.

Host edits may rebuild immutable history. They must not overwrite the canonical
editable region owned by the document. The current API enforces that every
editable region keeps the same ID and source contents across a host update.
It captures a source anchor at the viewport top before mutation and restores
that anchor's screen position after the next layout.

## Projection And Layout

The document projection is separate from the code editor's `DisplayMap`. It
maps rich projected spans and variable-height objects, not only folded and
wrapped text:

```text
source Rope + object markers
-> ProjectionMap
-> shaped and wrapped text runs
-> text lines and variable-height block rows
-> viewport coordinates
```

Every projected text span retains its source range. Paragraph-level
`TextStyleRefinement` and inline `HighlightStyle`/font-family runs are declared
against source ranges and resolved through the same projection. Pointer hit testing,
selection painting, copying, reveal, and application commands resolve through
that mapping. Immutable regions may hide Markdown syntax; the editable draft
initially uses identity projection.

Blocks are keyed by stable IDs. GPUI's SumTree-backed `ListState` retains
variable measurements and splices changed item ranges. Document layout measures
all unmeasured items at the current width before exposing the scroll extent;
only the viewport plus bounded overdraw is instantiated again during scrolling,
and only visible items are painted. Scrolling unchanged content must not discover
additional document height. Host insertion constrains the splice to the first
edited item so repeated rows cannot make a changed prefix look unchanged.

Splices re-arm complete measurement without invalidating retained item sizes.
Block changes invalidate the affected item; width or inherited text style/rem
changes invalidate layout globally. The trailer receives the current viewport
height before measurement. `DocumentState` and its `ListState` remain the sole
owners of layout and scroll geometry; the application does not maintain a second
height estimate.

Pointer selection uses the nearest shaped caret boundary, including the left
and right edges of soft-wrapped rows. Vertical gaps introduced by paragraph
styles or host wrappers resolve to the nearest text/object boundary. Only the
actual trailing area resolves to EOF; a virtualized viewport edge must not
jump to an unseen document endpoint. Atomic blocks retain input ownership on
press, while a drag started in text can extend across their boundaries.
Selection drags use the existing tracked selection anchor through host edits,
receive moves/releases outside the viewport, and reuse `AutoScroll` at viewport
edges. Hit testing after scrolling runs after the new visible layout is ready.
Applications may scope the existing SelectAll action to a domain text region
by setting the one document selection; Base's default remains document-wide.

Consecutive vertical moves retain their preferred x in `DocumentState`, even
across a shorter line. Horizontal moves, explicit selection, pointer input,
editing, reset and changes to layout width/style end this sequence. Platform
IME geometry preserves the single-caret rectangle; ranges spanning visual
lines return a valid rectangle at the first caret instead of a negative width.
Hit testing compares the shaped glyph boundaries and the final caret on the
target visual line. Small floating-point errors near the last glyph must not
advance navigation or pointer selection directly to the line end.

Application render callbacks are side-effect-free. Model changes and block
measurement feedback occur through explicit update paths, never by mutating
the document during paint.

Interactive blocks opt out of ancestor text keybindings with the
`InputHandoff` key context. Base input bindings use the selector
`Input && !InputHandoff`, and component Root focus traversal uses
`Root && !InputHandoff`; ordinary Input, Textarea, Editor, and focus traversal
behavior is unchanged, while a focused embedded terminal receives Enter, Tab,
navigation, and deletion keys before the surrounding document. The application
remains responsible for block-focus navigation and deletion semantics.

## Trailer And Scrolling

The trailer is a document layout item attached to the end anchor. It can fill
the remaining viewport, participate in hit testing, and resolve blank-space
clicks to a document position. It is not equivalent to
`scroll_beyond_last_line`, which only adds plain-text scroll extent.

The first consumer may keep a real zero-width editable range at EOF. This gives
blank-space clicks and first input ordinary IME and undo semantics without
requiring virtual materialization. `MaterializeAndApply` remains in the core
contract for consumers that truly have no source range before first input.

Document scrolling has one owner and exposes intent rather than raw ad hoc
offset writes:

```rust,ignore
document.reveal(position, strategy, cx);
document.pin_scroll(pin_id, anchor, viewport_fraction, cx);
document.unpin_scroll(pin_id, cx);
```

Wheel input, scrollbar drag, or an explicit user reveal stops following and
emits one event. A pin token prevents an unrelated stream or turn from
releasing the active pin.

## Public Seam

Public seam types keep private fields, constructors or builders, and readers.
The initial public module is `gpui_base::document`; shared editing internals
remain crate-private until a second real consumer proves a smaller stable API.

The document delegate supplies domain behavior through opaque identifiers and
callbacks. The delegate does not receive internal mutable references to the
Rope, anchor store, undo manager, or layout cache.

## Delivery Stages

1. **Complete:** extract one behavior-preserving editing primitive from
   `InputBaseState` and make all existing input modes consume it.
2. **Complete:** add stable anchors, selections, region policies, and
   transaction contracts with pure invariant tests.
3. **Complete:** add a plain-text `DocumentState` and `DocumentElement` with
   readonly ranges, one editable EOF range, selection, pointer input, and IME.
4. **Complete:** add host transactions and verify insertion before the draft
   preserves its cursor, composition, undo, and viewport.
5. **Complete:** add source/display projection, source-attached rich text, and
   block-level document positions.
6. **Complete:** add retained variable-height inline blocks and viewport-only
   SumTree layout.
7. **Complete:** add the dynamic trailer, anchor pins, matching-token unpin,
   and stop-follow event.
8. **Complete:** Jian uses one `SessionDocument` for history, draft, selection,
   scroll, annotations, and terminal handoff; its previous timeline selection,
   timeline scroll, and trailing Textarea interaction owners have been removed.

Each stage must compile and test independently. Existing Input, Textarea, and
Editor APIs remain behaviorally compatible throughout.

## Verification

The lowest layer proves each contract:

- pure tests: anchor bias, stale positions, region overlap, routed crossing
  edits, transaction ordering, undo transformation, projection mapping;
- GPUI context tests: focus, `EntityInputHandler`, IME, events, and stale
  revision rejection;
- UI integration tests: pointer selection, blank trailer clicks, keyboard
  navigation, inline-block boundaries, and scroll follow cancellation;
- consumer boundary tests: stream insertion before an active draft, submission
  sealing, restore, terminal handoff, and full build.

Manual validation remains required for platform IME candidate placement,
selection handles, scrollbar dragging, and embedded interactive content.

## Compatibility Rule

The Jian integration branch remains on GPUI Kit `v0.6.4` and `gpui-pre 0.3.5`.
It overrides only `gpui-base`; the published consumer pins this branch by an
immutable Git revision. Upgrading GPUI Kit or `gpui-pre` is a separate change
and is not a prerequisite for this RFC.
