use super::DocumentInlineStyle;
use gpui::{FontStyle, FontWeight, HighlightStyle, StrikethroughStyle, UnderlineStyle, blue, red};

#[test]
fn inline_style_layers_are_flattened_without_losing_highlights() {
    let styles = DocumentInlineStyle::flatten(vec![
        DocumentInlineStyle::new(
            0..4,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        ),
        DocumentInlineStyle::new(
            2..6,
            HighlightStyle {
                background_color: Some(blue()),
                ..Default::default()
            },
        ),
    ]);

    assert_eq!(styles.len(), 3);
    assert_eq!(styles[1].source(), 2..4);
    assert_eq!(styles[1].highlight().font_weight, Some(FontWeight::BOLD));
    assert_eq!(styles[1].highlight().background_color, Some(blue()));
}

#[test]
fn inline_styles_match_ordered_overlays_at_every_byte_boundary() {
    let source = "a中🙂bc文Z".repeat(8);
    let offsets = source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(source.len()))
        .collect::<Vec<_>>();
    let mut seed = 7_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as usize
    };
    for count in [0, 1, 2, 8, 64, 256] {
        for _ in 0..8 {
            let layers = (0..count)
                .map(|ix| {
                    let mask = next();
                    let color = if ix % 2 == 0 { blue() } else { red() };
                    let start = offsets[next() % offsets.len()];
                    let end = offsets[next() % offsets.len()];
                    // Include empty/reversed ranges, equal boundaries, default-only
                    // layers and input orders unrelated to range order.
                    let range = if ix % 5 == 0 {
                        start..end
                    } else {
                        start.min(end)..start.max(end)
                    };
                    let mut layer = DocumentInlineStyle::new(
                        range,
                        HighlightStyle {
                            color: (mask & 1 != 0).then_some(color),
                            font_weight: (mask & 2 != 0).then_some(if ix % 2 == 0 {
                                FontWeight::BOLD
                            } else {
                                FontWeight::NORMAL
                            }),
                            font_style: (mask & 4 != 0).then_some(if ix % 2 == 0 {
                                FontStyle::Italic
                            } else {
                                FontStyle::Normal
                            }),
                            background_color: (mask & 8 != 0).then_some(color),
                            underline: (mask & 16 != 0).then_some(UnderlineStyle {
                                color: Some(color),
                                ..Default::default()
                            }),
                            strikethrough: (mask & 32 != 0).then_some(StrikethroughStyle {
                                color: Some(color),
                                ..Default::default()
                            }),
                            fade_out: (mask & 64 != 0).then_some(if ix % 2 == 0 {
                                0.
                            } else {
                                0.5
                            }),
                        },
                    );
                    if mask & 128 != 0 {
                        layer = layer.font_family(if ix % 2 == 0 { "mono" } else { "prose" });
                    }
                    if mask & 256 != 0 {
                        layer = layer.font_scale(if ix % 2 == 0 { 70 } else { 90 });
                    }
                    if mask & 512 != 0 {
                        layer = layer.with_link(if ix % 2 == 0 { "first" } else { "second" });
                    }
                    if mask & 1024 != 0 {
                        layer = layer.with_link_hover_color(color);
                    }
                    layer
                })
                .collect::<Vec<_>>();

            // Independent slow oracle: at each original boundary, the last
            // covering Some wins separately for each field, including font.
            let mut boundaries = layers
                .iter()
                .flat_map(|s| [s.source().start, s.source().end])
                .collect::<Vec<_>>();
            boundaries.sort_unstable();
            boundaries.dedup();
            let expected = boundaries
                .windows(2)
                .filter_map(|bounds| {
                    let range = bounds[0]..bounds[1];
                    let covering = layers
                        .iter()
                        .rev()
                        .filter(|s| s.source().start <= range.start && s.source().end >= range.end)
                        .collect::<Vec<_>>();
                    if covering.is_empty() {
                        return None;
                    }
                    let highlight = HighlightStyle {
                        color: covering.iter().find_map(|s| s.highlight().color),
                        font_weight: covering.iter().find_map(|s| s.highlight().font_weight),
                        font_style: covering.iter().find_map(|s| s.highlight().font_style),
                        background_color: covering
                            .iter()
                            .find_map(|s| s.highlight().background_color),
                        underline: covering.iter().find_map(|s| s.highlight().underline),
                        strikethrough: covering.iter().find_map(|s| s.highlight().strikethrough),
                        fade_out: covering.iter().find_map(|s| s.highlight().fade_out),
                    };
                    Some(DocumentInlineStyle {
                        source: range,
                        highlight,
                        font_family: covering
                            .iter()
                            .find_map(|s| s.font_family_override())
                            .cloned(),
                        font_scale: covering.iter().find_map(|s| s.font_scale_override()),
                        link: covering.iter().find_map(|s| s.link()).cloned(),
                        link_hover: covering.iter().find_map(|s| s.link_hover_color()),
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(DocumentInlineStyle::flatten(layers), expected);
        }
    }

    // An empty range contributes a boundary but no layer. A default-only
    // covering style still produces output on both sides of that boundary.
    assert_eq!(
        DocumentInlineStyle::flatten(vec![
            DocumentInlineStyle::new(0..8, HighlightStyle::default()),
            DocumentInlineStyle::new(
                4..4,
                HighlightStyle {
                    color: Some(blue()),
                    ..Default::default()
                }
            )
            .font_family("ignored"),
        ]),
        vec![
            DocumentInlineStyle::new(0..4, HighlightStyle::default()),
            DocumentInlineStyle::new(4..8, HighlightStyle::default()),
        ],
    );
}
