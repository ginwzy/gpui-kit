---
title: Resizable
description: Panel groups and resize handles for user-adjustable split layouts.
order: 23
---

# Resizable

Panel groups and resize handles for user-adjustable split layouts.

Like every `gpui-base` primitive, Resizable supplies behavior and semantic structure without imposing a product visual language. Apply GPUI styles and compose the exported parts to match your design system.

## Example

The [single native Cargo entrypoint](https://github.com/longbridge/gpui-kit/blob/main/crates/base/examples/native/src/bin/components.rs) selects this primitive from the [shared showcase implementation](https://github.com/longbridge/gpui-kit/blob/main/crates/base/examples/showcase/mod.rs). The same showcase is compiled once for the WASM preview above.

```bash
cargo run -p gpui-base-examples -- resizable
```

## Import

```rust
use gpui_kit::base::{ResizablePanel, ResizablePanelGroup, ResizableState, h_resizable, resizable_panel};
```

## Anatomy and API

The example composes `ResizablePanel`, `ResizablePanelGroup`, `ResizableState`. GPUI's standard styling and event traits provide presentation; these base types provide the interaction structure.

The authoritative module is [`components/resizable.rs`](https://github.com/longbridge/gpui-kit/blob/main/crates/base/examples/showcase/components/resizable.rs). Native and browser previews compile this same file.

## State and events

Panel sizes live in resizable state; dragging handles updates adjacent panels subject to minimums.

Keep controlled state on the parent render type or in a GPUI entity. Update it in callbacks and call `cx.notify()`; do not recreate persistent entities during every render.

For a sidebar whose preferred width must survive container resizing, opt into
`ResizablePanelGroup::controlled()`. In this mode, each panel's `.size(...)`
is the current caller-owned preference; panels without a size flex into the
remaining space. `size_range` clamps only the rendered size. Dragging previews a
new split, then `on_resize` reports the measured sizes on release. Adopt the
relevant sizes in the owner and notify it; automatic layout and external value
changes do not invoke this callback. While dragging, `ResizableState::is_resizing()`
lets the owner distinguish the preview from its saved preference. Without
`controlled()`, the existing retained, proportional split behavior is unchanged.

## Complete Rust example

The complete implementation used by the runnable showcase is embedded directly from Rust source:

<<< ../../../crates/base/examples/showcase/components/resizable.rs{rust}

The command above supplies application initialization, window creation, and shared `BaseShowcase` state.

## Accessibility

Provide keyboard alternatives for handles and preserve usable minimum panel sizes.

## Notes

Use stable element IDs where accepted. Verify focus, hover, active, selected, disabled, reduced-motion, and high-contrast appearances in the consuming design system.
