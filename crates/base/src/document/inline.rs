use gpui::{AbsoluteLength, Pixels, Size, px, size};

/// The space an inline atomic object occupies, including its baseline measured
/// from the top. The host supplies its appearance through the block renderer.
/// Use rem lengths for objects that scale with the document; measured pixel
/// lengths must be reinstalled when their typography changes.
#[derive(Clone, Copy, Debug)]
pub struct InlineSize {
    width: AbsoluteLength,
    height: AbsoluteLength,
    baseline: AbsoluteLength,
}

// Descriptor identity is exact, including the declared unit. Compare bits so
// even an invalid, not-yet-validated descriptor has reflexive equality; this
// keeps DocumentBlock's existing Eq contract.
impl PartialEq for InlineSize {
    fn eq(&self, other: &Self) -> bool {
        let key = |length: AbsoluteLength| match length {
            AbsoluteLength::Pixels(value) => (0u8, f32::from(value).to_bits()),
            AbsoluteLength::Rems(value) => (1u8, value.0.to_bits()),
        };
        [self.width, self.height, self.baseline].map(key)
            == [other.width, other.height, other.baseline].map(key)
    }
}
impl Eq for InlineSize {}

impl InlineSize {
    pub fn new(
        width: impl Into<AbsoluteLength>,
        height: impl Into<AbsoluteLength>,
        baseline: impl Into<AbsoluteLength>,
    ) -> Self {
        Self {
            width: width.into(),
            height: height.into(),
            baseline: baseline.into(),
        }
    }

    pub fn width(&self) -> AbsoluteLength {
        self.width
    }
    pub fn height(&self) -> AbsoluteLength {
        self.height
    }
    pub fn baseline(&self) -> AbsoluteLength {
        self.baseline
    }

    pub(super) fn resolve(self, rem: Pixels) -> (Size<Pixels>, Pixels) {
        (
            size(self.width.to_pixels(rem), self.height.to_pixels(rem)),
            self.baseline.to_pixels(rem),
        )
    }

    pub(super) fn is_valid(self) -> bool {
        // Validate host-provided lengths at the presentation boundary. A
        // baseline outside the box is permitted; the row still contains both.
        [self.width, self.height, self.baseline]
            .into_iter()
            .all(|length| {
                let value = f32::from(length.to_pixels(px(1.)));
                value.is_finite() && value >= 0.
            })
    }
}

pub(super) const OBJECT: &str = "\u{fffc}";
