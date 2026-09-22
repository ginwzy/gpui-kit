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

Readonly and routed regions may be zero-width so an empty history node can
retain stable identity for reveal and follow. A zero-width editable region must
be the final region: the current input router resolves mutation targets from a
source selection, so it cannot disambiguate an empty editable node from a
following block at the same offset. Atomic blocks always occupy an object
replacement marker.

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
closes it. A rejected composition does not partially modify the document.

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
