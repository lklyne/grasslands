//! Geometry for the split tree: divider ratios, keyboard focus moves and
//! keyboard resizes. Pure functions so they can be tested without a window.

use std::collections::HashMap;

use gpui_kit::{Bounds, Pixels};

use crate::herdr::{PaneDirection, PaneId, SplitDirection, SplitTree};

/// Smallest share a divider can give either side.
pub const MIN_RATIO: f32 = 0.05;
/// How far one keyboard resize moves a divider.
pub const RESIZE_STEP: f32 = 0.05;

pub fn clamp_ratio(ratio: f32) -> f32 {
    ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO)
}

/// The ratio a divider gets when dragged to `pointer` inside a split that
/// occupies `bounds`.
pub fn ratio_at(
    direction: SplitDirection,
    bounds: Bounds<Pixels>,
    pointer: gpui_kit::Point<Pixels>,
) -> f32 {
    let (offset, extent) = match direction {
        SplitDirection::Right => (pointer.x - bounds.origin.x, bounds.size.width),
        SplitDirection::Down => (pointer.y - bounds.origin.y, bounds.size.height),
    };
    let extent = f32::from(extent);
    if extent <= 0.0 {
        return 0.5;
    }
    clamp_ratio(f32::from(offset) / extent)
}

/// The pane next to `from` in `direction`, judged by where the panes were
/// last drawn: it must lie on that side, and among those the one whose facing
/// edge is closest and that overlaps `from` most on the other axis wins.
pub fn neighbor<'a>(
    from: &PaneId,
    direction: PaneDirection,
    panes: &'a HashMap<PaneId, Bounds<Pixels>>,
) -> Option<&'a PaneId> {
    let origin = panes.get(from)?;
    let edge = |b: &Bounds<Pixels>| -> (f32, f32, f32, f32) {
        (
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.origin.x + b.size.width),
            f32::from(b.origin.y + b.size.height),
        )
    };
    let (ol, ot, or, ob) = edge(origin);
    // Panes are separated by a divider a few pixels wide.
    const SLACK: f32 = 12.0;
    panes
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, bounds)| {
            let (l, t, r, b) = edge(bounds);
            let (gap, overlap) = match direction {
                PaneDirection::Left => (ol - r, ob.min(b) - ot.max(t)),
                PaneDirection::Right => (l - or, ob.min(b) - ot.max(t)),
                PaneDirection::Up => (ot - b, or.min(r) - ol.max(l)),
                PaneDirection::Down => (t - ob, or.min(r) - ol.max(l)),
            };
            ((-1.0..=SLACK).contains(&gap) && overlap > 0.0).then_some((id, overlap))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// The divider a keyboard resize of `pane_id` moves: the innermost split
/// around the pane along that axis. Returns its path and new ratio; moving
/// left/up shrinks the first side, right/down grows it.
pub fn resize(
    tree: &SplitTree,
    pane_id: &PaneId,
    direction: PaneDirection,
) -> Option<(Vec<bool>, f32)> {
    let axis = match direction {
        PaneDirection::Left | PaneDirection::Right => SplitDirection::Right,
        PaneDirection::Up | PaneDirection::Down => SplitDirection::Down,
    };
    let delta = match direction {
        PaneDirection::Left | PaneDirection::Up => -RESIZE_STEP,
        PaneDirection::Right | PaneDirection::Down => RESIZE_STEP,
    };
    let path = tree.path_to(pane_id)?;
    (0..path.len()).rev().find_map(|depth| {
        let prefix = &path[..depth];
        match tree.at(prefix)? {
            SplitTree::Split {
                direction, ratio, ..
            } if *direction == axis => Some((prefix.to_vec(), clamp_ratio(ratio + delta))),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, px, size};

    fn pane(id: &str) -> SplitTree {
        SplitTree::Pane(id.into())
    }

    fn split(direction: SplitDirection, ratio: f32, a: SplitTree, b: SplitTree) -> SplitTree {
        SplitTree::Split {
            direction,
            ratio,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    /// a | (b over c)
    fn three() -> HashMap<PaneId, Bounds<Pixels>> {
        HashMap::from([
            ("a".into(), rect(0., 0., 400., 600.)),
            ("b".into(), rect(404., 0., 400., 298.)),
            ("c".into(), rect(404., 302., 400., 298.)),
        ])
    }

    #[test]
    fn ratio_at_is_the_pointer_share_of_the_split() {
        let ratio = ratio_at(
            SplitDirection::Right,
            rect(100., 0., 400., 300.),
            point(px(200.), px(50.)),
        );
        assert!((ratio - 0.25).abs() < 1e-6);
    }

    #[test]
    fn ratio_at_keeps_both_sides_visible() {
        let ratio = ratio_at(
            SplitDirection::Down,
            rect(0., 0., 400., 300.),
            point(px(10.), px(-40.)),
        );
        assert_eq!(ratio, MIN_RATIO);
    }

    #[test]
    fn neighbor_right_of_a_picks_the_pane_overlapping_most() {
        let mut panes = three();
        panes.insert("b".into(), rect(404., 0., 400., 400.));
        panes.insert("c".into(), rect(404., 404., 400., 196.));
        assert_eq!(
            neighbor(&"a".into(), PaneDirection::Right, &panes).map(PaneId::as_str),
            Some("b")
        );
    }

    #[test]
    fn neighbor_down_from_b_is_c() {
        assert_eq!(
            neighbor(&"b".into(), PaneDirection::Down, &three()).map(PaneId::as_str),
            Some("c")
        );
    }

    #[test]
    fn neighbor_left_of_the_leftmost_pane_is_none() {
        assert_eq!(
            neighbor(&"a".into(), PaneDirection::Left, &three()).map(PaneId::as_str),
            None
        );
    }

    #[test]
    fn resize_moves_the_innermost_split_on_that_axis() {
        let tree = split(
            SplitDirection::Right,
            0.5,
            pane("a"),
            split(SplitDirection::Down, 0.5, pane("b"), pane("c")),
        );
        assert_eq!(
            resize(&tree, &"c".into(), PaneDirection::Up),
            Some((vec![true], 0.45))
        );
    }

    #[test]
    fn resize_skips_splits_on_the_other_axis() {
        let tree = split(
            SplitDirection::Right,
            0.5,
            pane("a"),
            split(SplitDirection::Down, 0.5, pane("b"), pane("c")),
        );
        assert_eq!(
            resize(&tree, &"c".into(), PaneDirection::Right),
            Some((vec![], 0.55))
        );
    }

    #[test]
    fn resize_without_a_split_on_that_axis_is_none() {
        let tree = split(SplitDirection::Right, 0.5, pane("a"), pane("b"));
        assert_eq!(resize(&tree, &"a".into(), PaneDirection::Down), None);
    }
}
