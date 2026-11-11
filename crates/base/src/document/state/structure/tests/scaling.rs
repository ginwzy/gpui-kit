use super::{WORK, Work};
use crate::{
    Theme,
    document::{
        Affinity, AnchoredBlock, DocumentInlineStyle, DocumentPosition, DocumentProjection,
        DocumentRegion, DocumentSnapshot, DocumentState, DocumentStyles, EditPolicy,
    },
};
use gpui::{AppContext, Entity, HighlightStyle, IntoElement, TestAppContext, div};

fn document(count: usize, cx: &mut TestAppContext) -> Entity<DocumentState<usize>> {
    cx.update(|cx| {
        cx.set_global(Theme::default());
        crate::init(cx);
        cx.new(|cx| {
            let text = format!("{}draft", "h\n".repeat(count));
            let mut regions = (0..count)
                .map(|ix| DocumentRegion::new(ix, ix * 2..ix * 2 + 2, EditPolicy::Readonly))
                .collect::<Vec<_>>();
            regions.push(DocumentRegion::new(
                count,
                count * 2..text.len(),
                EditPolicy::Editable,
            ));
            let mut document = DocumentState::new("", Vec::new(), cx).unwrap();
            document.set_block_renderer(|_, _, _| div().into_any_element(), cx);
            let projection = DocumentProjection::identity(text.len());
            document
                .reset(
                    DocumentSnapshot::new(
                        text,
                        regions,
                        projection,
                        Vec::new(),
                        DocumentStyles::new(
                            Vec::new(),
                            (0..count)
                                .map(|ix| {
                                    DocumentInlineStyle::new(
                                        ix * 2..ix * 2 + 1,
                                        HighlightStyle::default(),
                                    )
                                })
                                .collect(),
                        ),
                    )
                    .anchored_blocks(
                        (0..count)
                            .map(|ix| {
                                AnchoredBlock::new(
                                    count + ix + 1,
                                    DocumentPosition::new(ix, 2, Affinity::Before),
                                )
                            })
                            .collect(),
                    )
                    .selection(DocumentPosition::new(
                        count,
                        5,
                        Affinity::After,
                    )),
                    cx,
                )
                .unwrap();
            document
        })
    })
}

#[gpui::test]
fn fixed_draft_work_stays_local_with_many_styled_nodes_and_anchors(cx: &mut TestAppContext) {
    for count in [0, 128, 4096] {
        let document = document(count, cx);
        WORK.with_borrow_mut(|work| *work = Some(Work::default()));
        document.update(cx, |document, cx| {
            let sent = usize::MAX - 1;
            let next = usize::MAX;
            document
                .apply_structure_splice(
                    document.revision(),
                    &count,
                    DocumentSnapshot::new(
                        "draft",
                        vec![
                            DocumentRegion::new(sent, 0..5, EditPolicy::Editable),
                            DocumentRegion::new(next, 5..5, EditPolicy::Editable),
                        ],
                        DocumentProjection::identity(5),
                        Vec::new(),
                        DocumentStyles::default(),
                    )
                    .selection(DocumentPosition::new(next, 0, Affinity::After)),
                    cx,
                )
                .unwrap();
            document
                .set_node_policies(document.revision(), &[(sent, EditPolicy::Readonly)], cx)
                .unwrap();
            document
                .apply_host_node_splice(
                    document.revision(),
                    Some(&sent),
                    Some(&next),
                    DocumentSnapshot::new(
                        "",
                        Vec::new(),
                        DocumentProjection::identity(0),
                        Vec::new(),
                        DocumentStyles::default(),
                    ),
                    cx,
                )
                .unwrap();
        });
        let work = WORK.with_borrow_mut(|work| work.take().unwrap());
        assert!(
            work.source_bytes < 128
                && work.layout_bytes < 128
                && work.anchor_checks <= 4
                && work.compacted_anchors == 0,
            "{count} styled history nodes: {work:?}"
        );
        document.read_with(cx, |document, _| {
            assert_eq!(document.text(), "h\n".repeat(count));
            assert_eq!(document.model.anchored_blocks.len(), count);
            assert_eq!(
                document.model.layout_items(None),
                document.layout_items.iter().cloned().collect::<Vec<_>>()
            );
        });
    }
}

#[gpui::test]
fn removing_many_anchors_compacts_the_suffix_once_and_keeps_its_indices(cx: &mut TestAppContext) {
    let count = 512;
    let document = document(count, cx);
    WORK.with_borrow_mut(|work| *work = Some(Work::default()));
    document.update(cx, |document, cx| {
        document
            .apply_host_node_splice(
                document.revision(),
                Some(&64),
                Some(&256),
                DocumentSnapshot::new(
                    "",
                    Vec::new(),
                    DocumentProjection::identity(0),
                    Vec::new(),
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
    });
    let work = WORK.with_borrow_mut(|work| work.take().unwrap());
    assert_eq!(work.compacted_anchors, count - 64);
    document.update(cx, |document, cx| {
        let expected = (0..64)
            .chain(256..count)
            .map(|ix| count + ix + 1)
            .collect::<Vec<_>>();
        assert_eq!(
            document
                .model
                .anchored_blocks
                .iter()
                .map(|block| *block.id())
                .collect::<Vec<_>>(),
            expected
        );
        for (ix, id) in expected.iter().enumerate() {
            assert_eq!(document.model.anchored_indices.get(id), Some(&ix));
            let node = document.model.anchored_blocks[ix].position().node_id();
            assert_eq!(document.model.anchored_nodes.get(node), Some(&vec![ix]));
        }
        for node in 64..256 {
            assert!(!document.model.anchored_nodes.contains_key(&node));
            assert!(
                !document
                    .model
                    .anchored_indices
                    .contains_key(&(count + node + 1))
            );
        }
        // A second operation addresses the retained suffix through the updated indices.
        document
            .apply_host_node_splice(
                document.revision(),
                Some(&256),
                Some(&257),
                DocumentSnapshot::new(
                    "",
                    Vec::new(),
                    DocumentProjection::identity(0),
                    Vec::new(),
                    DocumentStyles::default(),
                ),
                cx,
            )
            .unwrap();
        assert_eq!(document.model.anchored_blocks.len(), count - 193);
        assert_eq!(
            document.model.layout_items(None),
            document.layout_items.iter().cloned().collect::<Vec<_>>()
        );
    });
}
