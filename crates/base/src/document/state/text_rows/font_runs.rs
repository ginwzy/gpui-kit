use std::{iter::from_fn, ops::Range};

use gpui::{Font, FontStyle, FontWeight, SharedString};

use super::super::DocumentTextPresentation;

/// Sparse overrides, not a resolved font: explicit NORMAL still overrides an
/// inherited bold paragraph. Paint attributes never enter this representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct FontOverride<'a> {
    weight: Option<FontWeight>,
    style: Option<FontStyle>,
    family: Option<&'a SharedString>,
}

impl FontOverride<'_> {
    pub(super) fn apply(self, font: &mut Font) {
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

/// The maximal ranges with the same font overrides, including inherited gaps.
///
/// DocumentModel validates nonoverlapping source styles. Projection and clipping
/// preserve the order of both endpoints, although display ranges can overlap.
/// Each field therefore needs only its latest covering value: when that value
/// ends, every earlier value of the same field has ended too. This is NOT an
/// arbitrary-overlap sweep (nested ranges would need a stack).
///
/// Merge the two ordered start streams with the three current field ends,
/// consuming all events at a boundary before emitting its half-open interval.
/// Each input is consumed once; adjacent equal intervals are coalesced without
/// sorting, allocation, or rescanning. O(N + R) time, O(1) auxiliary space.
pub(super) fn font_runs(
    presentation: &DocumentTextPresentation,
    len: usize,
) -> impl Iterator<Item = (Range<usize>, FontOverride<'_>)> {
    let mut highlights = presentation
        .highlights
        .iter()
        .filter(|(range, style)| {
            !range.is_empty() && (style.font_weight.is_some() || style.font_style.is_some())
        })
        .peekable();
    let mut families = presentation
        .font_family_overrides
        .iter()
        .filter(|(range, _)| !range.is_empty())
        .peekable();
    let mut weight = None;
    let mut style = None;
    let mut family = None;
    let mut position = 0;
    let mut segments = from_fn(move || {
        if position == len {
            return None;
        }
        // Expire first, then apply every start here in input priority order.
        if weight.is_some_and(|(end, _)| end <= position) {
            weight = None;
        }
        if style.is_some_and(|(end, _)| end <= position) {
            style = None;
        }
        if family.is_some_and(|(end, _)| end <= position) {
            family = None;
        }
        while highlights
            .peek()
            .is_some_and(|(range, _)| range.start <= position)
        {
            let (range, highlight) = highlights.next().unwrap();
            if let Some(value) = highlight.font_weight {
                weight = Some((range.end, value));
            }
            if let Some(value) = highlight.font_style {
                style = Some((range.end, value));
            }
        }
        while families
            .peek()
            .is_some_and(|(range, _)| range.start <= position)
        {
            let (range, value) = families.next().unwrap();
            family = Some((range.end, value));
        }
        let end = [
            highlights.peek().map(|(range, _)| range.start),
            families.peek().map(|(range, _)| range.start),
            weight.map(|(end, _)| end),
            style.map(|(end, _)| end),
            family.map(|(end, _)| end),
        ]
        .into_iter()
        .flatten()
        .fold(len, usize::min);
        let run = (
            position..end,
            FontOverride {
                weight: weight.map(|(_, value)| value),
                style: style.map(|(_, value)| value),
                family: family.map(|(_, value)| value),
            },
        );
        position = end;
        Some(run)
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
