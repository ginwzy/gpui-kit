use super::super::DocumentState;
use gpui::{AppContext as _, Context, PreparedLineImage, PreparedLinePaint, Task};
use std::{
    collections::HashMap,
    hash::Hash,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub(in crate::document) struct PaintPreparation {
    entries: HashMap<Range<usize>, Entry>,
    task: Option<Task<()>>,
    current: Option<Range<usize>>,
    cancel: Arc<AtomicBool>,
}

struct Entry {
    request: PreparedLinePaint,
    image: Option<Arc<PreparedLineImage>>,
    finished: bool,
    seen: bool,
}

impl Drop for PaintPreparation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl PaintPreparation {
    pub(super) fn invalidate(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.entries.clear();
    }
    pub(in crate::document) fn begin_frame(&mut self) {
        for entry in self.entries.values_mut() {
            entry.seen = false;
        }
    }
    pub(super) fn is_pending(&self) -> bool {
        // A completed image notifies observers before the runner releases its
        // slot. Readiness describes the visible results, not that bookkeeping.
        self.entries.values().any(|entry| !entry.finished)
    }
    fn publish(
        &mut self,
        range: &Range<usize>,
        request: &PreparedLinePaint,
        result: anyhow::Result<Option<Arc<PreparedLineImage>>>,
    ) -> bool {
        let Some(entry) = self
            .entries
            .get_mut(range)
            .filter(|entry| entry.request.same_request(request))
        else {
            return false;
        };
        entry.finished = true;
        match result {
            Ok(image) => entry.image = image,
            Err(error) => tracing::error!("prepare document text image: {error:#}"),
        }
        true
    }
}

impl<I: Clone + Eq + Hash + 'static> DocumentState<I> {
    pub(in crate::document) fn request_row_image(
        &mut self,
        range: Range<usize>,
        request: PreparedLinePaint,
    ) -> Option<Arc<PreparedLineImage>> {
        let paint = &mut self.text_rows.as_mut()?.paint;
        if let Some(entry) = paint
            .entries
            .get_mut(&range)
            .filter(|entry| entry.request.same_request(&request))
        {
            entry.seen = true;
            return entry.image.clone();
        }
        if paint.current.as_ref() == Some(&range) {
            paint.cancel.store(true, Ordering::Relaxed);
        }
        paint.entries.insert(
            range,
            Entry {
                request,
                image: None,
                finished: false,
                seen: true,
            },
        );
        None
    }

    pub(in crate::document) fn finish_row_images(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.text_rows.as_mut() else {
            return;
        };
        let paint = &mut rows.paint;
        paint.entries.retain(|_, entry| entry.seen);
        if paint
            .current
            .as_ref()
            .is_some_and(|range| !paint.entries.contains_key(range))
        {
            paint.cancel.store(true, Ordering::Relaxed);
        }
        if paint.task.is_some() || !paint.entries.values().any(|entry| !entry.finished) {
            return;
        }
        let text_system = cx.text_system().clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let next = this
                    .update(cx, |this, _| {
                        let paint = &mut this.text_rows.as_mut()?.paint;
                        let next = paint
                            .entries
                            .iter()
                            .filter(|(_, entry)| !entry.finished)
                            .min_by_key(|(range, _)| range.start)
                            .map(|(range, entry)| (range.clone(), entry.request.clone()));
                        if let Some((range, request)) = next {
                            paint.current = Some(range.clone());
                            paint.cancel = Arc::new(AtomicBool::new(false));
                            Some((range, request, paint.cancel.clone()))
                        } else {
                            paint.current = None;
                            if let Some(task) = paint.task.take() {
                                task.detach();
                            }
                            None
                        }
                    })
                    .ok()
                    .flatten();
                let Some((range, request, cancel)) = next else {
                    break;
                };
                let source = text_system.clone();
                let work = request.clone();
                let cancelled = cancel.clone();
                let result = cx
                    .background_spawn(async move { work.prepare(&source, &cancelled) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let Some(rows) = this.text_rows.as_mut() else {
                            return;
                        };
                        let paint = &mut rows.paint;
                        paint.current = None;
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        if paint.publish(&range, &request, result.map(|image| image.map(Arc::new)))
                        {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.text_rows.as_mut().unwrap().paint.task = Some(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::state::{tests::document_view, text_rows::tests::settle_rows};
    use crate::document::{
        DocumentProjection, DocumentRegion, DocumentSnapshot, DocumentStyles, EditPolicy, TextRows,
    };
    use gpui::TestAppContext;

    #[gpui::test]
    fn image_of_replaced_text_cannot_replace_current_image(cx: &mut TestAppContext) {
        let (document, mut cx) = document_view(cx);
        let snapshot = |prefix| {
            let text = format!("{prefix}{}", "\u{301}".repeat(5000));
            let len = text.len();
            DocumentSnapshot::new(
                text,
                vec![DocumentRegion::new("source", 0..len, EditPolicy::Readonly)],
                DocumentProjection::identity(len),
                vec![],
                DocumentStyles::default(),
            )
        };
        document.update(&mut cx, |doc, cx| {
            doc.reset(snapshot('a'), cx).unwrap();
            doc.set_text_rows(Some(TextRows::new()), cx);
        });
        settle_rows(&document, &mut cx);
        let (range, old_request, old_image) = document.read_with(&cx, |doc, _| {
            let (range, entry) = doc
                .text_rows
                .as_ref()
                .unwrap()
                .paint
                .entries
                .iter()
                .next()
                .expect("extreme row uses background composition");
            (
                range.clone(),
                entry.request.clone(),
                entry.image.clone().unwrap(),
            )
        });
        document.update(&mut cx, |doc, cx| {
            doc.reset(snapshot('b'), cx).unwrap();
        });
        settle_rows(&document, &mut cx);
        document.update(&mut cx, |doc, _| {
            let paint = &mut doc.text_rows.as_mut().unwrap().paint;
            let current = paint.entries[&range].image.clone().unwrap();
            assert!(!paint.publish(&range, &old_request, Ok(Some(old_image))));
            assert!(Arc::ptr_eq(
                paint.entries[&range].image.as_ref().unwrap(),
                &current
            ));
        });
    }
}
