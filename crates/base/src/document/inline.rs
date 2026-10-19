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
    constrain_width: bool,
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
            && self.constrain_width == other.constrain_width
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
            constrain_width: false,
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

    /// Cap the preferred width at the paragraph's available row width. The
    /// object still wraps atomically; its renderer must fit the narrower box.
    /// Fixed-width objects keep the default `false` behavior.
    pub fn constrain_width(mut self, constrain: bool) -> Self {
        self.constrain_width = constrain;
        self
    }

    pub fn is_width_constrained(&self) -> bool {
        self.constrain_width
    }

    pub(in crate::document) fn constrained_to(mut self, rem: Pixels, width: Pixels) -> Self {
        if self.constrain_width {
            self.width = self.width.to_pixels(rem).min(width).into();
        }
        self
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
