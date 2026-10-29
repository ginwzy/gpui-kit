use super::frame_radii;
use gpui::{Corners, Tiling, px};

#[test]
fn tiled_edges_remove_only_their_adjacent_corners() {
    let radius = px(12.);
    for flags in 0..16 {
        let tiling = Tiling {
            top: flags & 1 != 0,
            right: flags & 2 != 0,
            bottom: flags & 4 != 0,
            left: flags & 8 != 0,
        };
        let radii = frame_radii(&tiling, radius);
        assert_eq!(radii.top_left > px(0.), !tiling.top && !tiling.left);
        assert_eq!(radii.top_right > px(0.), !tiling.top && !tiling.right);
        assert_eq!(radii.bottom_right > px(0.), !tiling.bottom && !tiling.right);
        assert_eq!(radii.bottom_left > px(0.), !tiling.bottom && !tiling.left);
    }
    assert_eq!(frame_radii(&Tiling::tiled(), radius), Corners::default());
    assert_eq!(frame_radii(&Tiling::default(), px(0.)), Corners::default());
}
