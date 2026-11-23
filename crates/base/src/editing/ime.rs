use gpui::{Bounds, Pixels, Window};

/// Last published geometry for one focused input, independent of caret visibility.
#[derive(Default)]
pub(crate) struct ImePosition {
    last: Option<(Bounds<Pixels>, f32)>,
    #[cfg(test)]
    pub(crate) invalidations: usize,
}

impl ImePosition {
    pub(crate) fn clear(&mut self) {
        self.last = None;
    }

    pub(crate) fn publish(&mut self, bounds: Option<Bounds<Pixels>>, window: &Window) {
        let position = bounds.map(|bounds| (bounds, window.scale_factor()));
        if self.last != position {
            self.last = position;
            if position.is_some() {
                window.invalidate_character_coordinates();
                #[cfg(test)]
                {
                    self.invalidations += 1;
                }
            }
        }
    }
}
