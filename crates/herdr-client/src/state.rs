//! The app-facing cache: workspaces → tabs → split tree → panes.
//!
//! Built whole from `session.snapshot` plus one `layout.export` per tab, and
//! replaced whole on every refresh. Never patched from events.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::*;

/// One immutable view of the herdr session. Shared as `Arc<State>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// Increments on every replacement; 0 means nothing has been read yet.
    pub generation: u64,
    pub version: String,
    pub protocol: u32,
    pub focused_workspace_id: Option<WorkspaceId>,
    pub focused_tab_id: Option<TabId>,
    pub focused_pane_id: Option<PaneId>,
    /// In herdr's order (by `number`).
    pub workspaces: Vec<Workspace>,
    pub panes: BTreeMap<PaneId, PaneInfo>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub info: WorkspaceInfo,
    /// In herdr's order (by `number`).
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub info: TabInfo,
    pub zoomed: bool,
    pub focused_pane_id: Option<PaneId>,
    /// `None` only if the tab's layout could not be read.
    pub root: Option<SplitTree>,
}

/// A tab's binary split tree. Leaves name panes; look them up in [`State::panes`].
#[derive(Debug, Clone, PartialEq)]
pub enum SplitTree {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        /// Share of the space given to `first`, 0..1.
        ratio: f32,
        first: Box<SplitTree>,
        second: Box<SplitTree>,
    },
}

/// A split inside a [`SplitTree`], addressed the way `layout.set_split_ratio` wants.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitRef {
    /// From the root: `false` = first child, `true` = second child.
    pub path: Vec<bool>,
    pub direction: SplitDirection,
    pub ratio: f32,
}

impl SplitTree {
    /// Converts herdr's exported node. Pane leaves without an id cannot occur in an
    /// export of a live tab; they are dropped by collapsing their parent split.
    pub fn from_layout(node: &LayoutNode) -> Option<Self> {
        match node {
            LayoutNode::Pane { pane_id, .. } => pane_id.clone().map(Self::Pane),
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match (Self::from_layout(first), Self::from_layout(second)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    direction: *direction,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (one, other) => one.or(other),
            },
        }
    }

    /// Pane ids in reading order (first before second).
    pub fn pane_ids(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Pane(id) => out.push(id),
            Self::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    /// Every split with its path, parents before children.
    pub fn splits(&self) -> Vec<SplitRef> {
        let mut out = Vec::new();
        self.collect_splits(&mut Vec::new(), &mut out);
        out
    }

    fn collect_splits(&self, path: &mut Vec<bool>, out: &mut Vec<SplitRef>) {
        if let Self::Split {
            direction,
            ratio,
            first,
            second,
        } = self
        {
            out.push(SplitRef {
                path: path.clone(),
                direction: *direction,
                ratio: *ratio,
            });
            path.push(false);
            first.collect_splits(path, out);
            path.pop();
            path.push(true);
            second.collect_splits(path, out);
            path.pop();
        }
    }

    /// The node at `path`, if any.
    pub fn at(&self, path: &[bool]) -> Option<&SplitTree> {
        let Some((&step, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Pane(_) => None,
            Self::Split { first, second, .. } => if step { second } else { first }.at(rest),
        }
    }

    /// Path from the root to the leaf holding `pane_id`.
    pub fn path_to(&self, pane_id: &str) -> Option<Vec<bool>> {
        match self {
            Self::Pane(id) => (id == pane_id).then(Vec::new),
            Self::Split { first, second, .. } => [(false, first), (true, second)]
                .into_iter()
                .find_map(|(step, child)| {
                    child.path_to(pane_id).map(|mut rest| {
                        rest.insert(0, step);
                        rest
                    })
                }),
        }
    }
}

impl State {
    /// Assembles a state from a snapshot and the exported tree of each tab.
    /// Tabs missing from `layouts` get `root: None`.
    pub fn build(
        generation: u64,
        snapshot: SessionSnapshot,
        layouts: &BTreeMap<TabId, LayoutDescription>,
    ) -> Self {
        let SessionSnapshot {
            version,
            protocol,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            mut workspaces,
            mut tabs,
            panes,
            agents,
            ..
        } = snapshot;
        workspaces.sort_by_key(|w| w.number);
        tabs.sort_by_key(|t| t.number);

        let mut tabs_by_workspace: BTreeMap<WorkspaceId, Vec<Tab>> = BTreeMap::new();
        for info in tabs {
            let layout = layouts.get(&info.tab_id);
            let tab = Tab {
                zoomed: layout.is_some_and(|l| l.zoomed),
                focused_pane_id: layout.map(|l| l.focused_pane_id.clone()),
                root: layout.and_then(|l| SplitTree::from_layout(&l.root)),
                info,
            };
            tabs_by_workspace
                .entry(tab.info.workspace_id.clone())
                .or_default()
                .push(tab);
        }

        let workspaces = workspaces
            .into_iter()
            .map(|info| Workspace {
                tabs: tabs_by_workspace
                    .remove(&info.workspace_id)
                    .unwrap_or_default(),
                info,
            })
            .collect();

        Self {
            generation,
            version,
            protocol,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            panes: panes.into_iter().map(|p| (p.pane_id.clone(), p)).collect(),
            agents,
        }
    }

    pub fn workspace(&self, workspace_id: &str) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|w| w.info.workspace_id == workspace_id)
    }

    pub fn tab(&self, tab_id: &str) -> Option<&Tab> {
        self.workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .find(|t| t.info.tab_id == tab_id)
    }

    pub fn pane(&self, pane_id: &str) -> Option<&PaneInfo> {
        self.panes.get(pane_id)
    }

    pub fn focused_pane(&self) -> Option<&PaneInfo> {
        self.focused_pane_id.as_deref().and_then(|id| self.pane(id))
    }

    pub fn pane_ids(&self) -> BTreeSet<&str> {
        self.panes.keys().map(String::as_str).collect()
    }

    /// Tabs whose split tree does not hold exactly the panes the snapshot puts in them,
    /// meaning the layout reads raced a change.
    pub(crate) fn inconsistent_tabs(&self) -> Vec<&str> {
        let mut by_tab: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for pane in self.panes.values() {
            by_tab
                .entry(&pane.tab_id)
                .or_default()
                .insert(&pane.pane_id);
        }
        self.workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .filter(|tab| {
                let in_tree: BTreeSet<&str> =
                    tab.root.iter().flat_map(SplitTree::pane_ids).collect();
                by_tab
                    .get(tab.info.tab_id.as_str())
                    .cloned()
                    .unwrap_or_default()
                    != in_tree
            })
            .map(|tab| tab.info.tab_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> Box<LayoutNode> {
        Box::new(LayoutNode::Pane {
            pane_id: Some(id.into()),
            cwd: None,
            label: None,
            command: None,
            env: BTreeMap::new(),
        })
    }

    fn sample_tree() -> SplitTree {
        let node = LayoutNode::Split {
            direction: SplitDirection::Right,
            ratio: 0.5,
            first: leaf("p1"),
            second: Box::new(LayoutNode::Split {
                direction: SplitDirection::Down,
                ratio: 0.3,
                first: leaf("p2"),
                second: leaf("p3"),
            }),
        };
        SplitTree::from_layout(&node).unwrap()
    }

    #[test]
    fn pane_ids_should_follow_reading_order() {
        assert_eq!(sample_tree().pane_ids(), vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn splits_should_carry_set_split_ratio_paths() {
        let paths: Vec<Vec<bool>> = sample_tree().splits().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, vec![vec![], vec![true]]);
    }

    #[test]
    fn path_to_should_find_nested_pane() {
        assert_eq!(sample_tree().path_to("p3"), Some(vec![true, true]));
    }

    #[test]
    fn at_should_return_node_at_path() {
        assert_eq!(
            sample_tree().at(&[true, false]),
            Some(&SplitTree::Pane("p2".into()))
        );
    }

    #[test]
    fn build_should_nest_tabs_under_workspaces() {
        let snapshot: SessionSnapshot = serde_json::from_str(SNAPSHOT).unwrap();
        let layout: LayoutDescription = serde_json::from_str(LAYOUT).unwrap();
        let state = State::build(1, snapshot, &BTreeMap::from([("w1:t1".to_owned(), layout)]));
        assert_eq!(state.workspace("w1").map(|w| w.tabs.len()), Some(1));
    }

    #[test]
    fn inconsistent_tabs_should_be_empty_when_tree_matches_snapshot() {
        let snapshot: SessionSnapshot = serde_json::from_str(SNAPSHOT).unwrap();
        let layout: LayoutDescription = serde_json::from_str(LAYOUT).unwrap();
        let state = State::build(1, snapshot, &BTreeMap::from([("w1:t1".to_owned(), layout)]));
        assert!(state.inconsistent_tabs().is_empty());
    }

    #[test]
    fn inconsistent_tabs_should_flag_missing_layout() {
        let snapshot: SessionSnapshot = serde_json::from_str(SNAPSHOT).unwrap();
        let state = State::build(1, snapshot, &BTreeMap::new());
        assert_eq!(state.inconsistent_tabs(), vec!["w1:t1"]);
    }

    // Captured from herdr 0.9.5.
    const SNAPSHOT: &str = r#"{"version":"0.9.5","protocol":22,"focused_workspace_id":"w1","focused_tab_id":"w1:t1","focused_pane_id":"w1:p1","workspaces":[{"workspace_id":"w1","number":1,"label":"spike","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"w1:t1","agent_status":"unknown"}],"tabs":[{"tab_id":"w1:t1","workspace_id":"w1","number":1,"label":"1","focused":true,"pane_count":1,"agent_status":"unknown"}],"panes":[{"pane_id":"w1:p1","terminal_id":"term_65dee54c760601","workspace_id":"w1","tab_id":"w1:t1","focused":true,"cwd":"/Users/lyleklyne","foreground_cwd":"/Users/lyleklyne","agent_status":"unknown","scroll":{"offset_from_bottom":0,"max_offset_from_bottom":0,"viewport_rows":40},"revision":0}],"layouts":[{"workspace_id":"w1","tab_id":"w1:t1","zoomed":false,"area":{"x":0,"y":0,"width":120,"height":40},"focused_pane_id":"w1:p1","panes":[{"pane_id":"w1:p1","focused":true,"rect":{"x":0,"y":0,"width":120,"height":40}}],"splits":[]}],"agents":[]}"#;
    const LAYOUT: &str = r#"{"workspace_id":"w1","tab_id":"w1:t1","zoomed":false,"focused_pane_id":"w1:p1","root":{"type":"pane","pane_id":"w1:p1","cwd":"/Users/lyleklyne"}}"#;
}
