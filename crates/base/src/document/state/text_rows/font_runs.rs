use std::{iter::from_fn, ops::Range};

use gpui::{Font, FontStyle, FontWeight, SharedString};

use super::super::DocumentTextPresentation;

/// Sparse overrides, not a resolved font: explicit NORMAL still overrides an
/// inherited bold paragraph. Paint attributes never enter this representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(in crate::document) struct FontOverride<'a> {
    weight: Option<FontWeight>,
    style: Option<FontStyle>,
    family: Option<&'a SharedString>,
    pub(in crate::document) scale: Option<u8>,
}

impl FontOverride<'_> {
    pub(in crate::document) fn apply(self, font: &mut Font) {
        if let Some(weight) = self.weight {
            font.weight = weight;
        }
        if let Some(style) = self.style {
            font.style = style;
        }
        if let Some(family) = self.family {
            font.family = family.clone();
        }
    }
}

/// Read the same resolved display intervals that StyledText paints. Adjacent
/// paint-only differences coalesce, keeping wrap identity independent of color.
pub(in crate::document) fn font_runs(
    presentation: &DocumentTextPresentation,
    len: usize,
) -> impl Iterator<Item = (Range<usize>, FontOverride<'_>)> {
    let mut highlights = presentation.highlights.iter();
    let mut families = presentation.font_family_overrides.iter().peekable();
    let mut scales = presentation.font_scales.iter().peekable();
    let mut position = 0;
    let mut segments = from_fn(move || {
        if position == len {
            return None;
        }
        while families
            .peek()
            .is_some_and(|(range, _)| range.end <= position)
        {
            families.next();
        }
        while scales
            .peek()
            .is_some_and(|(range, _)| range.end <= position)
        {
            scales.next();
        }
        let scale = scales
            .peek()
            .and_then(|(range, scale)| (range.start <= position).then_some(*scale));
        let family = families
            .peek()
            .and_then(|(range, family)| (range.start <= position).then_some(family));
        let (range, font) = match highlights.next() {
            Some((range, highlight)) => {
                debug_assert_eq!(range.start, position);
                (
                    range.clone(),
                    FontOverride {
                        weight: highlight.font_weight,
                        style: highlight.font_style,
                        family,
                        scale,
                    },
                )
            }
            None => (position..len, FontOverride::default()),
        };
        position = range.end;
        Some((range, font))
    })
    .peekable();
    from_fn(move || {
        let (mut range, font) = segments.next()?;
        while segments.peek().is_some_and(|(_, next)| *next == font) {
            range.end = segments.next().unwrap().0.end;
        }
        Some((range, font))
    })
}
