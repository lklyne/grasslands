//! Drags in the terminal area: dividers, panes by their header, and tabs
//! along the tab bar.
//!
//! The divider state ([`RatioOverrides`]), the tab drag's pitch steps
//! ([`TabDragTick`]) and the drop decisions are plain data so they can be
//! tested without a window. herdr owns split ratios: a local ratio only
//! covers the time between a drag and herdr's layout reflecting it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::herdr::{PaneId, PaneMoveDestination, SplitDirection, SplitTree, TabId};
use crate::overlay;

/// How long a local divider ratio waits for herdr's layout to catch up.
const OVERRIDE_TIMEOUT: Duration = Duration::from_secs(3);
/// How close herdr's ratio must come to the local one to count as caught up.
const CAUGHT_UP: f32 = 0.005;

/// A split in a tab's tree, addressed the way `layout.set_split_ratio`
/// wants: `false` for the first side at each level.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SplitKey {
    pub tab_id: TabId,
    pub path: Vec<bool>,
}

impl SplitKey {
    /// The divider's element id, e.g. `divider-t1-01`.
    pub fn element_id(&self) -> SharedString {
        let path: String = self
            .path
            .iter()
            .map(|step| if *step { '1' } else { '0' })
            .collect();
        format!("divider-{}-{path}", self.tab_id).into()
    }
}

pub struct RatioOverride {
    pub ratio: f32,
    since: Instant,
}

struct DividerDrag {
    key: SplitKey,
    direction: SplitDirection,
}

/// Divider ratios set here that herdr hasn't confirmed yet, and the divider
/// being dragged.
#[derive(Default)]
pub struct RatioOverrides {
    ratios: HashMap<SplitKey, RatioOverride>,
    drag: Option<DividerDrag>,
}

impl RatioOverrides {
    pub fn get(&self, key: &SplitKey) -> Option<&RatioOverride> {
        self.ratios.get(key)
    }

    /// Forgets everything; herdr ids mean nothing on another machine.
    pub fn clear(&mut self) {
        self.ratios.clear();
        self.drag = None;
    }

    pub fn is_dragging(&self, key: &SplitKey) -> bool {
        self.drag.as_ref().is_some_and(|drag| &drag.key == key)
    }

    pub fn dragging(&self) -> Option<&SplitKey> {
        self.drag.as_ref().map(|drag| &drag.key)
    }

    pub fn begin_drag(&mut self, key: SplitKey, direction: SplitDirection) {
        self.drag = Some(DividerDrag { key, direction });
    }

    /// Moves the dragged divider to `pointer` inside its split's `bounds`.
    /// False when nothing is being dragged or the split wasn't drawn.
    pub fn drag_to(
        &mut self,
        bounds: impl FnOnce(&SplitKey) -> Option<Bounds<Pixels>>,
        pointer: Point<Pixels>,
        now: Instant,
    ) -> bool {
        let Some(drag) = self.drag.as_ref() else {
            return false;
        };
        let Some(bounds) = bounds(&drag.key) else {
            return false;
        };
        let ratio = super::layout::ratio_at(drag.direction, bounds, pointer);
        let key = drag.key.clone();
        self.set(key, ratio, now);
        true
    }

    /// Ends the drag; returns the ratio to send to herdr, if it moved.
    pub fn end_drag(&mut self) -> Option<(SplitKey, f32)> {
        let drag = self.drag.take()?;
        let ratio = self.ratios.get(&drag.key)?.ratio;
        Some((drag.key, ratio))
    }

    pub fn set(&mut self, key: SplitKey, ratio: f32, now: Instant) {
        self.ratios.insert(key, RatioOverride { ratio, since: now });
    }

    /// Drops the ratios herdr has caught up with (`herdr_ratio` gives its
    /// current one; `None` when the split is gone), or never will. The
    /// dragged divider keeps its ratio.
    pub fn retain_unconfirmed(
        &mut self,
        herdr_ratio: impl Fn(&SplitKey) -> Option<f32>,
        now: Instant,
    ) {
        let dragging = self.drag.as_ref().map(|drag| &drag.key);
        self.ratios.retain(|key, local| {
            if dragging == Some(key) {
                return true;
            }
            let caught_up =
                herdr_ratio(key).is_none_or(|ratio| (ratio - local.ratio).abs() < CAUGHT_UP);
            !caught_up && now.duration_since(local.since) < OVERRIDE_TIMEOUT
        });
    }

    /// `root` with the local ratios of `tab_id` applied.
    pub fn apply(&self, tab_id: &TabId, root: &SplitTree) -> SplitTree {
        fn apply(
            node: &SplitTree,
            path: &mut Vec<bool>,
            lookup: &dyn Fn(&[bool]) -> Option<f32>,
        ) -> SplitTree {
            match node {
                SplitTree::Pane(id) => SplitTree::Pane(id.clone()),
                SplitTree::Split {
                    direction,
                    ratio,
                    first,
                    second,
                } => {
                    let ratio = lookup(path).unwrap_or(*ratio);
                    path.push(false);
                    let first = apply(first, path, lookup);
                    path.pop();
                    path.push(true);
                    let second = apply(second, path, lookup);
                    path.pop();
                    SplitTree::Split {
                        direction: *direction,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }
                }
            }
        }
        let lookup = |path: &[bool]| {
            self.ratios
                .get(&SplitKey {
                    tab_id: tab_id.to_owned(),
                    path: path.to_vec(),
                })
                .map(|local| local.ratio)
        };
        apply(root, &mut Vec::new(), &lookup)
    }
}

/// herdr's `tab.move` index for dropping the tab at `from` on the tab at
/// `to`, so it lands where `to` was. herdr counts the gaps of the bar before
/// the move, so a tab moving right skips past its target.
pub fn tab_insert_index(from: usize, to: usize) -> usize {
    if to > from { to + 1 } else { to }
}

/// The dragged tab and the tab the drag last ticked over, so each new tab
/// ticks once.
#[derive(Debug, Default)]
pub struct TabDragTick(Option<(usize, usize)>);

impl TabDragTick {
    pub fn reset(&mut self) {
        self.0 = None;
    }

    /// A drag from tab `from` entered tab `over`: the semitones to tick at
    /// (one higher for each tab further right than where it started, lower
    /// going left), or `None` when it is still over the same tab. The first
    /// move only notes where the drag is.
    pub fn over(&mut self, from: usize, over: usize) -> Option<f64> {
        let last = match self.0 {
            Some((origin, last)) if origin == from => last,
            _ => {
                self.0 = Some((from, over));
                return None;
            }
        };
        if last == over {
            return None;
        }
        self.0 = Some((from, over));
        Some(over as f64 - from as f64)
    }
}

/// What dropping pane `source` on pane `target` does, given the tab each is
/// in.
#[derive(Debug, PartialEq)]
pub enum PaneDrop {
    Swap,
    /// From another tab: move it next to the target instead.
    Move(PaneMoveDestination),
}

pub fn pane_drop(
    source: (&PaneId, Option<&TabId>),
    target: (&PaneId, Option<&TabId>),
) -> Option<PaneDrop> {
    let ((source, source_tab), (target, target_tab)) = (source, target);
    if source == target {
        return None;
    }
    let target_tab = target_tab?;
    if source_tab == Some(target_tab) {
        return Some(PaneDrop::Swap);
    }
    Some(PaneDrop::Move(PaneMoveDestination::Tab {
        tab_id: target_tab.clone(),
        split: SplitDirection::Right,
        target_pane_id: Some(target.to_owned()),
        ratio: None,
    }))
}

/// A pane being dragged by its header: drop on another pane to swap, on a
/// tab to move it there, on `+` to move it to a new tab.
#[derive(Clone)]
pub struct DraggedPane {
    pub pane_id: PaneId,
    pub label: SharedString,
}

/// A tab being dragged along the tab bar to a new place (`tab.move`).
#[derive(Clone)]
pub struct DraggedTab {
    pub tab_id: TabId,
    /// Its place in the bar, to show which side of a target it will land on.
    pub index: usize,
    pub label: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.label.clone())
    }
}

/// GPUI's own drag preview, left empty: the main window draws under the
/// terminals, so the visible preview is a [`DragChip`] in the overlay.
pub struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The dragged pane's label, following the pointer in the overlay. It does
/// not occlude, so the overlay keeps letting the drag's mouse events through
/// to the main window, where the drop targets are.
struct DragChip {
    label: SharedString,
    position: Point<Pixels>,
}

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(self.position.x + px(12.))
            .top(self.position.y + px(10.))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .child(self.label.clone())
    }
}

/// The dragged pane's chip, on its own overlay layer.
pub struct PaneDragPreview {
    overlay: Entity<overlay::Overlay>,
    layer: overlay::LayerId,
    chip: Entity<DragChip>,
}

impl PaneDragPreview {
    pub fn open(
        label: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Self> {
        let overlay = match overlay::get_or_open(window, cx) {
            Ok(overlay) => overlay,
            Err(error) => {
                tracing::error!("drag preview: {error:#}");
                return None;
            }
        };
        let chip = cx.new(|_| DragChip { label, position });
        let layer = overlay.update(cx, |overlay, cx| {
            overlay.push_layer(chip.clone(), overlay::Capture::Passive, cx)
        });
        Some(Self {
            overlay,
            layer,
            chip,
        })
    }

    pub fn move_to(&self, position: Point<Pixels>, cx: &mut App) {
        self.chip.update(cx, |chip, cx| {
            chip.position = position;
            cx.notify();
        });
    }

    pub fn close(self, cx: &mut App) {
        self.overlay
            .update(cx, |overlay, cx| overlay.remove_layer(self.layer, cx));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use gpui_kit::{Bounds, Pixels, point, px, size};

    use super::{
        OVERRIDE_TIMEOUT, PaneDrop, RatioOverrides, SplitKey, TabDragTick, pane_drop,
        tab_insert_index,
    };
    use crate::herdr::{PaneId, PaneMoveDestination, SplitDirection, SplitTree, TabId};

    fn key(path: &[bool]) -> SplitKey {
        SplitKey {
            tab_id: "t1".into(),
            path: path.to_vec(),
        }
    }

    fn split(ratio: f32, first: SplitTree, second: SplitTree) -> SplitTree {
        SplitTree::Split {
            direction: SplitDirection::Right,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn pane(id: &str) -> SplitTree {
        SplitTree::Pane(id.into())
    }

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(0.)), size(px(200.), px(100.)))
    }

    #[test]
    fn split_key_element_id_spells_the_path() {
        assert_eq!(key(&[false, true]).element_id().as_ref(), "divider-t1-01");
        assert_eq!(key(&[]).element_id().as_ref(), "divider-t1-");
    }

    #[test]
    fn drag_to_follows_the_pointer_inside_the_split() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        overrides.begin_drag(key(&[]), SplitDirection::Right);
        assert!(overrides.drag_to(|_| Some(bounds()), point(px(150.), px(50.)), now));
        assert_eq!(overrides.get(&key(&[])).map(|o| o.ratio), Some(0.25));
    }

    #[test]
    fn drag_to_without_a_drag_or_drawn_split_does_nothing() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        assert!(!overrides.drag_to(|_| Some(bounds()), point(px(150.), px(0.)), now));
        overrides.begin_drag(key(&[]), SplitDirection::Right);
        assert!(!overrides.drag_to(|_| None, point(px(150.), px(0.)), now));
        assert!(overrides.get(&key(&[])).is_none());
    }

    #[test]
    fn end_drag_returns_the_ratio_to_send() {
        let mut overrides = RatioOverrides::default();
        overrides.begin_drag(key(&[true]), SplitDirection::Right);
        overrides.drag_to(|_| Some(bounds()), point(px(250.), px(0.)), Instant::now());
        assert_eq!(overrides.end_drag(), Some((key(&[true]), 0.75)));
        assert!(overrides.dragging().is_none());
    }

    #[test]
    fn end_drag_without_a_move_sends_nothing() {
        let mut overrides = RatioOverrides::default();
        overrides.begin_drag(key(&[]), SplitDirection::Down);
        assert_eq!(overrides.end_drag(), None);
        assert!(overrides.dragging().is_none());
    }

    #[test]
    fn retain_unconfirmed_drops_ratios_herdr_caught_up_with() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        overrides.set(key(&[]), 0.3, now);
        overrides.set(key(&[true]), 0.6, now);
        overrides.retain_unconfirmed(|k| Some(if k.path.is_empty() { 0.302 } else { 0.5 }), now);
        assert!(overrides.get(&key(&[])).is_none());
        assert!(overrides.get(&key(&[true])).is_some());
    }

    #[test]
    fn retain_unconfirmed_drops_ratios_of_gone_splits() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        overrides.set(key(&[]), 0.3, now);
        overrides.retain_unconfirmed(|_| None, now);
        assert!(overrides.get(&key(&[])).is_none());
    }

    #[test]
    fn retain_unconfirmed_gives_up_after_the_timeout() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        overrides.set(key(&[]), 0.3, now);
        overrides.retain_unconfirmed(|_| Some(0.5), now + OVERRIDE_TIMEOUT);
        assert!(overrides.get(&key(&[])).is_none());
    }

    #[test]
    fn retain_unconfirmed_keeps_the_dragged_divider() {
        let mut overrides = RatioOverrides::default();
        let now = Instant::now();
        overrides.begin_drag(key(&[]), SplitDirection::Right);
        overrides.set(key(&[]), 0.3, now);
        overrides.retain_unconfirmed(|_| Some(0.3), now + OVERRIDE_TIMEOUT);
        assert!(overrides.get(&key(&[])).is_some());
    }

    #[test]
    fn apply_replaces_only_overridden_ratios() {
        let mut overrides = RatioOverrides::default();
        overrides.set(key(&[true]), 0.2, Instant::now());
        let root = split(0.5, pane("a"), split(0.5, pane("b"), pane("c")));
        let applied = overrides.apply(&"t1".into(), &root);
        assert_eq!(
            applied,
            split(0.5, pane("a"), split(0.2, pane("b"), pane("c")))
        );
    }

    #[test]
    fn apply_ignores_other_tabs() {
        let mut overrides = RatioOverrides::default();
        overrides.set(key(&[]), 0.2, Instant::now());
        let root = split(0.5, pane("a"), pane("b"));
        assert_eq!(overrides.apply(&"t2".into(), &root), root);
    }

    #[test]
    fn tab_insert_index_skips_past_a_target_to_the_right() {
        assert_eq!(tab_insert_index(0, 2), 3);
        assert_eq!(tab_insert_index(3, 1), 1);
        assert_eq!(tab_insert_index(1, 1), 1);
    }

    #[test]
    fn tab_drag_tick_first_move_is_silent() {
        let mut tick = TabDragTick::default();
        assert_eq!(tick.over(1, 1), None);
    }

    #[test]
    fn tab_drag_tick_steps_once_per_new_tab() {
        let mut tick = TabDragTick::default();
        tick.over(1, 1);
        assert_eq!(tick.over(1, 2), Some(1.0));
        assert_eq!(tick.over(1, 2), None);
        assert_eq!(tick.over(1, 0), Some(-1.0));
    }

    #[test]
    fn tab_drag_tick_restarts_for_another_drag() {
        let mut tick = TabDragTick::default();
        tick.over(1, 1);
        assert_eq!(tick.over(3, 2), None);
        assert_eq!(tick.over(3, 0), Some(-3.0));
        tick.reset();
        assert_eq!(tick.over(3, 1), None);
    }

    #[test]
    fn pane_drop_on_itself_does_nothing() {
        let (a, t1) = (PaneId::from("p1"), TabId::from("t1"));
        assert_eq!(pane_drop((&a, Some(&t1)), (&a, Some(&t1))), None);
    }

    #[test]
    fn pane_drop_in_the_same_tab_swaps() {
        let (a, b, t1) = (PaneId::from("p1"), PaneId::from("p2"), TabId::from("t1"));
        assert_eq!(
            pane_drop((&a, Some(&t1)), (&b, Some(&t1))),
            Some(PaneDrop::Swap)
        );
    }

    #[test]
    fn pane_drop_from_another_tab_moves_next_to_the_target() {
        let (a, b) = (PaneId::from("p1"), PaneId::from("p2"));
        let (t1, t2) = (TabId::from("t1"), TabId::from("t2"));
        assert_eq!(
            pane_drop((&a, Some(&t1)), (&b, Some(&t2))),
            Some(PaneDrop::Move(PaneMoveDestination::Tab {
                tab_id: t2,
                split: SplitDirection::Right,
                target_pane_id: Some(b.clone()),
                ratio: None,
            }))
        );
    }

    #[test]
    fn pane_drop_on_a_gone_pane_does_nothing() {
        let (a, b, t1) = (PaneId::from("p1"), PaneId::from("p2"), TabId::from("t1"));
        assert_eq!(pane_drop((&a, Some(&t1)), (&b, None)), None);
    }
}
