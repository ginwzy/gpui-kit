use super::*;
use crate::document::{Affinity, TextEdit};
use ropey::Rope;
use std::{ops::Range, sync::Arc};

#[test]
fn after_affinity_skips_all_hidden_objects_at_a_text_boundary() {
    let source = Rope::from("\u{fffc}\u{fffc}text");
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::hide(0..3), ProjectionSpan::hide(3..6)],
    )
    .unwrap();
    let map = ProjectionMap::new(&source, &projection);
    assert_eq!(map.display_to_source(0, Affinity::Before), Some(0));
    assert_eq!(map.display_to_source(0, Affinity::After), Some(6));
}

#[test]
fn replacement_maps_source_and_display_edges_with_affinity() {
    let source = Rope::from("a**bold**z");
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::hide(1..3), ProjectionSpan::hide(7..9)],
    )
    .unwrap();
    let map = ProjectionMap::new(&source, &projection);

    assert_eq!(map.display_text(), "aboldz");
    assert_eq!(map.source_to_display(1, Affinity::Before), Some(1));
    assert_eq!(map.source_to_display(2, Affinity::After), Some(1));
    assert_eq!(map.source_to_display(3, Affinity::After), Some(1));
    assert_eq!(map.display_to_source(1, Affinity::Before), Some(1));
    assert_eq!(map.display_to_source(1, Affinity::After), Some(3));
    assert_eq!(map.display_to_source(5, Affinity::After), Some(9));
}

#[test]
fn replacement_text_maps_interior_offsets_to_source_boundaries() {
    let source = Rope::from("aXYZz");
    let projection =
        DocumentProjection::new(source.len(), vec![ProjectionSpan::replace(1..4, "block")])
            .unwrap();
    let map = ProjectionMap::new(&source, &projection);

    assert_eq!(map.display_text(), "ablockz");
    assert_eq!(map.display_to_source(3, Affinity::Before), Some(1));
    assert_eq!(map.display_to_source(3, Affinity::After), Some(4));
    assert_eq!(map.source_to_display(2, Affinity::Before), Some(1));
    assert_eq!(map.source_to_display(2, Affinity::After), Some(6));
}

#[test]
fn spans_follow_an_edit_before_them() {
    let projection = DocumentProjection::new(6, vec![ProjectionSpan::replace(4..5, "X")]).unwrap();
    let transformed = projection.transformed(&[TextEdit::new(1..1, "++")], 8);
    assert_eq!(transformed.spans()[0].source(), 6..7);
}

#[test]
fn projection_boundaries_exclude_adjacent_edits() {
    for span in [
        ProjectionSpan::hide(2..4),
        ProjectionSpan::replace(2..4, "Q"),
        ProjectionSpan::mapped(2..4, "Q", vec![ProjectionMapping::new(0..1, 2..4)]),
    ] {
        let projection = DocumentProjection::new(6, vec![span]).unwrap();
        for (edit, source, expected) in [
            (TextEdit::new(2..2, "x"), "abxCDyz", 3..5),
            (TextEdit::new(4..4, "x"), "abCDxyz", 2..4),
            (TextEdit::new(0..2, "x"), "xCDyz", 1..3),
            (TextEdit::new(4..6, "x"), "abCDx", 2..4),
        ] {
            let transformed = projection.transformed(&[edit], source.len());
            let span = &transformed.spans()[0];
            assert_eq!(span.source(), expected);
            if let Some(mapping) = &span.mapping {
                assert_eq!(mapping[0].source(), expected);
            }
            assert_eq!(
                ProjectionMap::new(&Rope::from(source), &transformed).display_text(),
                format!(
                    "{}{}{}",
                    &source[..expected.start],
                    span.replacement(),
                    &source[expected.end..]
                ),
            );
        }
    }
}

#[test]
fn mapped_replacement_preserves_scalar_source_ranges() {
    let source = Rope::from("**重复 &amp;**");
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::mapped(
            0..source.len(),
            "重复 &",
            vec![
                ProjectionMapping::new(0..3, 2..5),
                ProjectionMapping::new(3..6, 5..8),
                ProjectionMapping::new(6..7, 8..9),
                ProjectionMapping::new(7..8, 9..14),
            ],
        )],
    )
    .unwrap();
    let map = ProjectionMap::new(&source, &projection);

    assert_eq!(map.display_text(), "重复 &");
    assert_eq!(map.source_to_display(2, Affinity::After), Some(0));
    assert_eq!(map.source_to_display(5, Affinity::After), Some(3));
    assert_eq!(map.display_to_source(3, Affinity::After), Some(5));
    assert_eq!(map.display_to_source(7, Affinity::Before), Some(9));
    assert_eq!(
        map.display_to_source(8, Affinity::After),
        Some(source.len())
    );
}

#[test]
fn one_verbatim_run_maps_like_its_scalars() {
    let source = Rope::from("x**重复 &amp;**y");
    let map = |mapping| {
        let projection = DocumentProjection::new(
            source.len(),
            vec![ProjectionSpan::mapped(
                1..source.len() - 1,
                "重复 &",
                mapping,
            )],
        )
        .unwrap();
        ProjectionMap::new(&source, &projection)
    };
    let scalars = map(vec![
        ProjectionMapping::new(0..3, 3..6),
        ProjectionMapping::new(3..6, 6..9),
        ProjectionMapping::new(6..7, 9..10),
        ProjectionMapping::new(7..8, 10..15),
    ]);
    let runs = map(vec![
        ProjectionMapping::new(0..7, 3..10),
        ProjectionMapping::new(7..8, 10..15),
    ]);
    for affinity in [Affinity::Before, Affinity::After] {
        for offset in 0..=source.len() {
            assert_eq!(
                runs.source_to_display(offset, affinity),
                scalars.source_to_display(offset, affinity),
                "source {offset} {affinity:?}"
            );
        }
        for offset in 0..=runs.display_text().len() {
            assert_eq!(
                runs.display_to_source(offset, affinity),
                scalars.display_to_source(offset, affinity),
                "display {offset} {affinity:?}"
            );
        }
    }
}

#[test]
fn partition_from_any_hint_matches_partition_point() {
    for len in 0..40 {
        let items: Vec<usize> = (0..len).collect();
        for split in 0..=len {
            for hint in 0..=len + 2 {
                assert_eq!(
                    partition_from(&items, hint, |item| *item < split),
                    split,
                    "len {len} split {split} hint {hint}"
                );
            }
        }
    }
}

#[test]
fn shared_display_text_is_reused_until_an_edit() {
    let source = Rope::from("one\ntwo");
    let mut map = ProjectionMap::new(&source, &DocumentProjection::identity(source.len()));
    let shared = map.shared_display_text();
    assert!(Arc::ptr_eq(&shared, &map.shared_display_text()));
    map.replace_identity(4..7, "three");
    assert_eq!(&*map.shared_display_text(), "one\nthree");
    assert_eq!(&*shared, "one\ntwo");
}

#[test]
fn resumed_display_lookups_match_fresh_ones() {
    let source = Rope::from("ab**中文 &amp;**cd\u{fffc}ef");
    let len = source.len();
    let projection = DocumentProjection::new(
        len,
        vec![
            ProjectionSpan::mapped(
                2..18,
                "中文 &",
                vec![
                    ProjectionMapping::new(0..7, 4..11),
                    ProjectionMapping::new(7..8, 11..16),
                ],
            ),
            ProjectionSpan::hide(20..23),
        ],
    )
    .unwrap();
    let map = ProjectionMap::new(&source, &projection);
    let mut hint = 0;
    for offset in 0..=map.display_text().len() + 1 {
        for affinity in [Affinity::After, Affinity::Before] {
            assert_eq!(
                map.display_to_source_from(offset, affinity, &mut hint),
                map.display_to_source(offset, affinity),
                "{offset} {affinity:?}"
            );
        }
    }
    // A hint past the answer falls back to a full search.
    let mut hint = map.runs.len();
    assert_eq!(
        map.display_to_source_from(0, Affinity::After, &mut hint),
        Some(0)
    );
}

#[test]
fn invalid_spans_are_rejected() {
    assert_eq!(
        DocumentProjection::new(4, vec![ProjectionSpan::hide(2..2)]),
        Err(ProjectionError::InvalidRange(2..2))
    );
    assert_eq!(
        DocumentProjection::new(
            4,
            vec![ProjectionSpan::hide(1..3), ProjectionSpan::hide(2..4)],
        ),
        Err(ProjectionError::Overlap(2..4))
    );
}

#[test]
fn style_coverage_excludes_shared_code_and_entity_endpoints() {
    let source = "`ab`后 &amp;末";
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::mapped(
            0..source.len(),
            "ab后 &末",
            vec![
                ProjectionMapping::new(0..1, 0..4),
                ProjectionMapping::new(1..2, 0..4),
                ProjectionMapping::new(2..5, 4..7),
                ProjectionMapping::new(5..6, 7..8),
                ProjectionMapping::new(6..7, 8..13),
                ProjectionMapping::new(7..10, 13..16),
            ],
        )],
    )
    .unwrap();
    let map = ProjectionMap::new(&Rope::from(source), &projection);
    for (source, expected) in [
        (0..4, vec![0..2]),
        (4..7, vec![2..5]),
        (8..13, vec![6..7]),
        (13..16, vec![7..10]),
        (4..4, vec![]),
        (1..3, vec![0..2]),
        (4..16, vec![2..10]),
    ] {
        let mut ranges = Vec::new();
        map.project_source_range(source, |range| ranges.push(range));
        assert_eq!(ranges, expected);
    }
}

#[test]
fn coverage_keeps_identity_clipping_hidden_text_and_whole_replacements() {
    let source = "abcDEfgh";
    let projection = DocumentProjection::new(
        source.len(),
        vec![
            ProjectionSpan::replace(1..3, "中🙂"),
            ProjectionSpan::hide(3..5),
        ],
    )
    .unwrap();
    let map = ProjectionMap::new(&Rope::from(source), &projection);
    for (source, expected) in [
        (0..1, vec![0..1]),
        (1..2, vec![1..8]),
        (2..3, vec![1..8]),
        (3..5, vec![]),
        (5..7, vec![8..10]),
        (0..8, vec![0..11]),
    ] {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        map.project_source_range(source, |range| ranges.push(range));
        assert_eq!(ranges, expected);
    }
}

#[test]
fn coverage_keeps_overlaps_with_non_monotonic_source_ends() {
    let source = "abcdef";
    let projection = DocumentProjection::new(
        source.len(),
        vec![ProjectionSpan::mapped(
            0..source.len(),
            "XYZ",
            vec![
                ProjectionMapping::new(0..1, 0..6),
                ProjectionMapping::new(1..2, 1..2),
                ProjectionMapping::new(2..3, 3..5),
            ],
        )],
    )
    .unwrap();
    let map = ProjectionMap::new(&Rope::from(source), &projection);
    for (source, expected) in [
        (2..3, vec![0..1]),
        (4..5, vec![0..1, 2..3]),
        (5..6, vec![0..1]),
    ] {
        let mut ranges = Vec::new();
        map.project_source_range(source, |range| ranges.push(range));
        assert_eq!(ranges, expected);
    }
}
