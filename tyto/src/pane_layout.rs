//! Where a workspace's leaves, dividers and panes sit, computed from its
//! split tree instead of left to nested flex boxes.
//!
//! Panes are built once and stay in one flat layer, each placed over
//! its leaf by these rects; only the chrome (tab bars, dividers) follows
//! the tree. Nesting panes inside the tree's boxes meant every split,
//! close or zoom rebuilt every pane of the workspace: a shell's
//! scrollback went back to the top, an explorer lost its list position.
//!
//! A rect's lengths are `pct`% of the pane area plus fixed pixels, what
//! the flex layout resolved them to, so CSS applies them directly
//! (`calc()`) and the browser keeps them right as the window resizes.

use leptos::*;

use crate::types::{PaneLeaf, PaneTree, SplitDir};

/// A leaf's tab bar, above its panes.
pub(crate) const TAB_BAR_PX: f64 = 28.0;
/// The draggable bar between a split's two sides.
pub(crate) const DIVIDER_PX: f64 = 6.0;

/// A length in the pane area: `pct` percent of the area's width (for a
/// left / width) or height (top / height), plus `px` pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Len {
    pub(crate) pct: f64,
    pub(crate) px:  f64,
}

impl Len {
    const ZERO: Len = Len { pct: 0.0, px: 0.0 };
    const FULL: Len = Len { pct: 100.0, px: 0.0 };

    const fn px(px: f64) -> Len { Len { pct: 0.0, px } }

    fn plus(self, other: Len) -> Len { Len { pct: self.pct + other.pct, px: self.px + other.px } }

    fn times(self, k: f64) -> Len { Len { pct: self.pct * k, px: self.px * k } }

    fn css(self) -> String { format!("calc({}% + {}px)", self.pct, self.px) }

    /// In pixels, for an area `area_px` wide (or high).
    pub(crate) fn resolve(self, area_px: f64) -> f64 { self.pct / 100.0 * area_px + self.px }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) left:   Len,
    pub(crate) top:    Len,
    pub(crate) width:  Len,
    pub(crate) height: Len,
}

impl Rect {
    /// The whole pane area.
    pub(crate) const FULL: Rect = Rect { left: Len::ZERO, top: Len::ZERO, width: Len::FULL, height: Len::FULL };

    /// The two sides of a split at `ratio` and the divider between them,
    /// centred on the ratio: `(a, divider, b)`.
    fn split(self, dir: SplitDir, ratio: f64) -> (Rect, Rect, Rect) {
        let half = Len::px(DIVIDER_PX / 2.0);
        let (start, size) = match dir {
            SplitDir::H => (self.left, self.width),
            SplitDir::V => (self.top, self.height),
        };
        let cut = start.plus(size.times(ratio));
        let sides = [
            (start, size.times(ratio).plus(half.times(-1.0))),
            (cut.plus(half.times(-1.0)), Len::px(DIVIDER_PX)),
            (cut.plus(half), size.times(1.0 - ratio).plus(half.times(-1.0))),
        ];
        let [a, divider, b] = sides.map(|(start, size)| match dir {
            SplitDir::H => Rect { left: start, width: size, ..self },
            SplitDir::V => Rect { top: start, height: size, ..self },
        });
        (a, divider, b)
    }

    /// A leaf's tab bar.
    pub(crate) fn tab_bar(self) -> Rect { Rect { height: Len::px(TAB_BAR_PX), ..self } }

    /// A leaf's panes, below its tab bar.
    fn below_tab_bar(self) -> Rect {
        Rect {
            top:    self.top.plus(Len::px(TAB_BAR_PX)),
            height: self.height.plus(Len::px(-TAB_BAR_PX)),
            ..self
        }
    }

    /// `position: absolute` placement, for an element of the pane area.
    pub(crate) fn style(self) -> String {
        format!(
            "position:absolute;left:{};top:{};width:{};height:{};",
            self.left.css(), self.top.css(), self.width.css(), self.height.css(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Divider {
    pub(crate) dir:   SplitDir,
    /// The split's ratio — also identifies the divider.
    pub(crate) ratio: RwSignal<f64>,
    /// The split's whole area, which a drag measures the ratio against.
    pub(crate) region: Rect,
    pub(crate) rect:   Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PanePlace {
    pub(crate) id:    String,
    pub(crate) rect:  Rect,
    /// On screen: its leaf's active tab, and not behind another leaf's zoom.
    pub(crate) shown: bool,
}

/// A workspace's layout at this moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Placement {
    /// The leaves on screen (the zoomed one alone, when zoomed).
    pub(crate) leaves:   Vec<(PaneLeaf, Rect)>,
    pub(crate) dividers: Vec<Divider>,
    /// Every pane of the workspace, in tree order.
    pub(crate) panes:    Vec<PanePlace>,
}

/// Lay out `tree`; with `zoomed` naming a pane in it, that pane's leaf
/// fills the area and the others' panes keep their places, hidden (so
/// their size, and what's laid out in them, is unchanged on unzoom).
/// Reads the split ratios and the leaves' tabs, so a memo over it
/// follows drags and tab switches.
pub(crate) fn place(tree: &PaneTree, zoomed: Option<&str>) -> Placement {
    let mut placement = Placement::default();
    let mut owners = Vec::new();
    walk(tree, Rect::FULL, &mut placement, &mut owners);
    let zoomed_leaf = zoomed.and_then(|z| {
        placement.panes.iter().zip(&owners).find(|(p, _)| p.id == z).map(|(_, leaf)| *leaf)
    });
    if let Some(leaf) = zoomed_leaf {
        for (pane, owner) in placement.panes.iter_mut().zip(&owners) {
            if *owner == leaf {
                pane.rect = Rect::FULL.below_tab_bar();
            } else {
                pane.shown = false;
            }
        }
        placement.leaves = vec![(leaf, Rect::FULL)];
        placement.dividers.clear();
    }
    placement
}

/// `owners[i]` is the leaf of `placement.panes[i]`.
fn walk(tree: &PaneTree, rect: Rect, placement: &mut Placement, owners: &mut Vec<PaneLeaf>) {
    match tree {
        PaneTree::Leaf(leaf) => {
            let tabs = leaf.tabs.get();
            let active = leaf.active.get();
            for (i, id) in tabs.into_iter().enumerate() {
                placement.panes.push(PanePlace { id, rect: rect.below_tab_bar(), shown: i == active });
                owners.push(*leaf);
            }
            placement.leaves.push((*leaf, rect));
        }
        PaneTree::Split { dir, ratio, a, b } => {
            let (rect_a, divider, rect_b) = rect.split(*dir, ratio.get());
            walk(a, rect_a, placement, owners);
            placement.dividers.push(Divider { dir: *dir, ratio: *ratio, region: rect, rect: divider });
            walk(b, rect_b, placement, owners);
        }
    }
}

/// `now`'s ids in a stable order: those already in `before` keep their
/// order, new ones go last. A keyed list in this order never moves an
/// existing pane in the DOM — moving one would reset its scroll.
pub(crate) fn stable_order(before: &[String], now: &[String]) -> Vec<String> {
    let mut order: Vec<String> = before.iter().filter(|id| now.contains(id)).cloned().collect();
    for id in now {
        if !order.contains(id) {
            order.push(id.clone());
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(pct: f64, px: f64) -> Len { Len { pct, px } }

    #[test]
    fn a_split_matches_the_flex_layout_it_replaces() {
        // `flex: 0 0 calc(r% - 3px)` on both sides of a 6px divider.
        let (a, divider, b) = Rect::FULL.split(SplitDir::H, 0.25);
        assert_eq!((a.left, a.width), (len(0.0, 0.0), len(25.0, -3.0)));
        assert_eq!((divider.left, divider.width), (len(25.0, -3.0), len(0.0, 6.0)));
        assert_eq!((b.left, b.width), (len(25.0, 3.0), len(75.0, -3.0)));
        assert_eq!((a.top, a.height), (Len::ZERO, Len::FULL));
        // The pieces tile the area: in a 1000px area, 0–247, 247–253, 253–1000.
        assert_eq!(b.left.plus(b.width).resolve(1000.0), 1000.0);

        // Nested: the bottom half of the right side.
        let (_, _, bottom) = b.split(SplitDir::V, 0.5);
        assert_eq!((bottom.left, bottom.width), (b.left, b.width));
        assert_eq!((bottom.top, bottom.height), (len(50.0, 3.0), len(50.0, -3.0)));
        assert_eq!(bottom.below_tab_bar().top, len(50.0, 31.0));
        assert_eq!(bottom.style(),
                   "position:absolute;left:calc(25% + 3px);top:calc(50% + 3px);\
                    width:calc(75% + -3px);height:calc(50% + -3px);");
    }

    #[test]
    fn panes_are_placed_in_their_leaf_and_a_zoom_hides_the_rest() {
        let _rt = create_runtime();
        let left = PaneLeaf::new("a".into());
        left.tabs.set(vec!["a".into(), "b".into()]);
        left.active.set(1);
        let right = PaneLeaf::new("c".into());
        let ratio = create_rw_signal(0.5);
        let tree = PaneTree::Split {
            dir: SplitDir::H, ratio,
            a: Box::new(PaneTree::Leaf(left)),
            b: Box::new(PaneTree::Leaf(right)),
        };

        let p = place(&tree, None);
        let shown: Vec<(&str, bool)> = p.panes.iter().map(|p| (p.id.as_str(), p.shown)).collect();
        assert_eq!(shown, [("a", false), ("b", true), ("c", true)]);
        let (rect_left, _, rect_right) = Rect::FULL.split(SplitDir::H, 0.5);
        assert_eq!(p.leaves, [(left, rect_left), (right, rect_right)]);
        assert_eq!(p.panes[0].rect, rect_left.below_tab_bar());
        assert_eq!(p.panes[2].rect, rect_right.below_tab_bar());
        assert_eq!(p.dividers.len(), 1);
        assert_eq!(p.dividers[0].region, Rect::FULL);

        // Zoomed on c: its leaf fills the area; a and b keep their place, hidden.
        let z = place(&tree, Some("c"));
        assert_eq!(z.leaves, [(right, Rect::FULL)]);
        assert!(z.dividers.is_empty());
        let shown: Vec<(&str, bool)> = z.panes.iter().map(|p| (p.id.as_str(), p.shown)).collect();
        assert_eq!(shown, [("a", false), ("b", false), ("c", true)]);
        assert_eq!(z.panes[1].rect, rect_left.below_tab_bar());
        assert_eq!(z.panes[2].rect, Rect::FULL.below_tab_bar());

        // A zoom on a pane that's gone is ignored, as before.
        assert_eq!(place(&tree, Some("gone")), p);
    }

    #[test]
    fn known_panes_keep_their_order() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A split put "n" between "a" and "c" in tree order: it goes last.
        assert_eq!(stable_order(&ids(&["a", "c"]), &ids(&["a", "n", "c"])), ids(&["a", "c", "n"]));
        // Closing drops it; moving to another leaf changes nothing.
        assert_eq!(stable_order(&ids(&["a", "c", "n"]), &ids(&["c", "a"])), ids(&["a", "c"]));
        assert_eq!(stable_order(&[], &ids(&["x", "y"])), ids(&["x", "y"]));
    }
}
