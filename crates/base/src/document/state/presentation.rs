use crate::document::DocumentInlineStyle;
use gpui::HighlightStyle;

use super::DocumentTextPresentation;

impl DocumentTextPresentation {
    /// Resolve projected coverage once, before either wrapping or painting.
    /// Each field takes the last covering explicit value. Font-family edges
    /// also split highlights, so StyledText sees exactly the measured fonts.
    pub(super) fn resolve_inline(&mut self, len: usize) {
        let mut layers: Vec<_> = std::mem::take(&mut self.highlights)
            .into_iter()
            .map(|(range, highlight)| DocumentInlineStyle::new(range, highlight))
            .chain(
                std::mem::take(&mut self.font_family_overrides)
                    .into_iter()
                    .map(|(range, family)| {
                        DocumentInlineStyle::new(range, HighlightStyle::default())
                            .font_family(family)
                    }),
            )
            .chain(
                std::mem::take(&mut self.font_scales)
                    .into_iter()
                    .map(|(range, scale)| {
                        DocumentInlineStyle::new(range, HighlightStyle::default()).font_scale(scale)
                    }),
            )
            .filter(|style| !style.source().is_empty())
            .collect();
        if layers.is_empty() {
            return;
        }
        // Keep inherited-font gaps in the same partition as explicit styles.
        layers.push(DocumentInlineStyle::new(0..len, HighlightStyle::default()));
        let mut previous_family = None;
        let mut previous_scale = None;
        for style in DocumentInlineStyle::flatten(layers) {
            let range = style.source();
            let (start, end) = (range.start, range.end);
            let highlight = style.highlight();
            let family = style.font_family_override();
            let scale = style.font_scale_override();
            if let Some((range, previous)) = self.highlights.last_mut()
                && *previous == highlight
                && previous_family.as_ref() == family
                && previous_scale == scale
            {
                range.end = end;
                if family.is_some() {
                    self.font_family_overrides.last_mut().unwrap().0.end = end;
                }
                if scale.is_some() {
                    self.font_scales.last_mut().unwrap().0.end = end;
                }
            } else {
                self.highlights.push((start..end, highlight));
                if let Some(family) = family {
                    self.font_family_overrides
                        .push((start..end, family.clone()));
                }
                if let Some(scale) = scale {
                    self.font_scales.push((start..end, scale));
                }
                previous_scale = scale;
                previous_family = family.cloned();
            }
        }
    }
}

#[cfg(test)]
mod tests;
