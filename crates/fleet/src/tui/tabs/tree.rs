//! One tree model for every tab: parents fold/unfold their children, `j/k` walk every visible
//! node, and the selected node is what `Enter`, `o`, the detail panel, etc. act on.

use fleet_core::model::{
    Activity, AgentRow, MachineView, PaneRow, PaneView, ReviewRow, ReviewView, WorktreeRow,
    WorktreeView,
};
use std::collections::HashSet;
use std::path::PathBuf;

/// A row identity that survives refreshes and tab switches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Project(String),
    Dir(PathBuf),
    Agent(u32),
    Session(String),
    Pane(String),
    Review(String, u64),
    Provider(String),
    Machine(String),
}

#[derive(Debug, Clone)]
pub enum NodeData {
    /// A section: a configured project, or "outside projects".
    Project {
        name: String,
        summary: String,
    },
    Agent {
        agent: AgentRow,
        worktree: Option<WorktreeRow>,
        review: Option<ReviewRow>,
        pane: Option<PaneRow>,
        project: Option<String>,
    },
    Worktree(WorktreeView),
    Session {
        /// The machine it is on; `None` = this computer.
        machine: Option<String>,
        name: String,
        attached: bool,
        panes: Vec<PaneView>,
        worktree: Option<WorktreeRow>,
        agents: usize,
        urgent: Option<Activity>,
    },
    Pane(PaneView),
    Review(ReviewView),
    /// A machine provider section (`local`, `instabox`).
    Provider {
        name: String,
        summary: String,
    },
    Machine(MachineView),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub key: RowKey,
    pub depth: u8,
    pub has_children: bool,
    pub unfolded: bool,
    pub data: NodeData,
}

pub struct TreeNode {
    pub key: RowKey,
    pub data: NodeData,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn leaf(key: RowKey, data: NodeData) -> Self {
        Self {
            key,
            data,
            children: vec![],
        }
    }
    pub fn new(key: RowKey, data: NodeData, children: Vec<TreeNode>) -> Self {
        Self {
            key,
            data,
            children,
        }
    }
}

/// Text the `/` filter matches against.
pub fn hay(d: &NodeData) -> String {
    match d {
        NodeData::Project { name, summary } => format!("{name} {summary}"),
        NodeData::Agent {
            agent,
            worktree,
            review,
            pane,
            project,
        } => format!(
            "{} {} {} {} {} {} {}",
            agent.title.clone().unwrap_or_default(),
            agent.activity.label(),
            agent.mode.clone().unwrap_or_default(),
            worktree
                .as_ref()
                .map(|w| format!("{} {}", w.name, w.branch.clone().unwrap_or_default()))
                .unwrap_or_default(),
            review
                .as_ref()
                .map(|r| format!("{} {}", r.display_id, r.status.label()))
                .unwrap_or_default(),
            pane.as_ref().map(|p| p.target()).unwrap_or_default(),
            project.clone().unwrap_or_default(),
        ),
        NodeData::Worktree(v) => format!(
            "{} {} {} {} {} {} {}",
            v.worktree.name,
            v.worktree.repo,
            v.worktree.branch.clone().unwrap_or_default(),
            v.worktree.path.display(),
            v.overall.label(),
            v.review
                .as_ref()
                .map(|r| format!("{} {}", r.display_id, r.status.label()))
                .unwrap_or_default(),
            v.agents
                .iter()
                .map(|a| format!(
                    "{} {}",
                    a.title.clone().unwrap_or_default(),
                    a.activity.label()
                ))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        NodeData::Session {
            name,
            panes,
            worktree,
            ..
        } => format!(
            "{name} {} {}",
            worktree
                .as_ref()
                .map(|w| w.name.clone())
                .unwrap_or_default(),
            panes
                .iter()
                .map(|p| format!(
                    "{} {} {}",
                    p.pane.command,
                    p.pane.title,
                    p.agent
                        .as_ref()
                        .and_then(|a| a.title.clone())
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        NodeData::Pane(p) => format!(
            "{} {} {} {}",
            p.pane.target(),
            p.pane.command,
            p.pane.title,
            p.agent
                .as_ref()
                .and_then(|a| a.title.clone())
                .unwrap_or_default()
        ),
        NodeData::Provider { name, summary } => format!("{name} {summary}"),
        NodeData::Machine(m) => format!(
            "{} {} {} {}",
            m.row.name,
            m.row.provider,
            m.row.state.label(),
            m.tasks.join(" ")
        ),
        NodeData::Review(v) => format!(
            "{} {} {} {} {} {}",
            v.review.display_id,
            v.review.title,
            v.review.author,
            v.review.source_branch,
            v.review.status.label(),
            v.worktrees
                .iter()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

fn matches(tn: &TreeNode, filter: &str) -> bool {
    filter.is_empty()
        || super::super::util::fuzzy_match(&hay(&tn.data), filter)
        || tn.children.iter().any(|c| matches(c, filter))
}

/// Flatten the tree into the visible node list. A parent is kept when it or any descendant
/// matches `filter`; children appear only when the parent is in `unfolded`.
pub fn flatten(tree: Vec<TreeNode>, unfolded: &HashSet<RowKey>, filter: &str) -> Vec<Node> {
    let mut out = vec![];
    for tn in tree {
        push(tn, 0, unfolded, filter, &mut out);
    }
    out
}

fn push(tn: TreeNode, depth: u8, unfolded: &HashSet<RowKey>, filter: &str, out: &mut Vec<Node>) {
    if !matches(&tn, filter) {
        return;
    }
    let has_children = !tn.children.is_empty();
    let is_unfolded = has_children && unfolded.contains(&tn.key);
    out.push(Node {
        key: tn.key.clone(),
        depth,
        has_children,
        unfolded: is_unfolded,
        data: tn.data,
    });
    if is_unfolded {
        for c in tn.children {
            push(c, depth + 1, unfolded, filter, out);
        }
    }
}

/// Index of the parent of node `i` (the nearest earlier node with a smaller depth).
pub fn parent_index(nodes: &[Node], i: usize) -> Option<usize> {
    let d = nodes.get(i)?.depth;
    if d == 0 {
        return None;
    }
    (0..i).rev().find(|&j| nodes[j].depth < d)
}

impl Node {
    pub fn pane_target(&self) -> Option<String> {
        match &self.data {
            NodeData::Agent { agent, pane, .. } => agent
                .pane_target
                .clone()
                .or_else(|| pane.as_ref().map(|p| p.target())),
            NodeData::Worktree(v) => v
                .agents
                .first()
                .and_then(|a| a.pane_target.clone())
                .or_else(|| {
                    v.panes
                        .iter()
                        .find(|p| fleet_core::agents::KNOWN.contains(&p.command.as_str()))
                        .or(v.panes.first())
                        .map(|p| p.target())
                }),
            NodeData::Session { name, .. } => Some(name.clone()),
            NodeData::Pane(p) => Some(p.pane.target()),
            NodeData::Review(v) => v.agents.first().and_then(|a| a.pane_target.clone()),
            NodeData::Project { .. } | NodeData::Provider { .. } | NodeData::Machine(_) => None,
        }
    }

    /// The machine the row is on (`None` = this computer, or no machine).
    pub fn machine(&self) -> Option<String> {
        match &self.data {
            NodeData::Agent { agent, .. } => agent.machine.clone(),
            NodeData::Worktree(v) => v.worktree.machine.clone(),
            NodeData::Session { machine, .. } => machine.clone(),
            NodeData::Pane(p) => p.pane.machine.clone(),
            NodeData::Machine(m) if m.row.provider != "local" => Some(m.row.name.clone()),
            _ => None,
        }
    }

    pub fn plan_url(&self) -> Option<String> {
        self.agents().iter().find_map(|a| a.plan_url.clone())
    }

    pub fn agents(&self) -> Vec<&AgentRow> {
        match &self.data {
            NodeData::Agent { agent, .. } => vec![agent],
            NodeData::Worktree(v) => v.agents.iter().collect(),
            NodeData::Session { panes, .. } => {
                panes.iter().filter_map(|p| p.agent.as_ref()).collect()
            }
            NodeData::Pane(p) => p.agent.iter().collect(),
            NodeData::Review(v) => v.agents.iter().collect(),
            NodeData::Project { .. } | NodeData::Provider { .. } | NodeData::Machine(_) => vec![],
        }
    }

    pub fn review(&self) -> Option<&ReviewRow> {
        match &self.data {
            NodeData::Agent { review, .. } => review.as_ref(),
            NodeData::Worktree(v) => v.review.as_ref(),
            NodeData::Review(v) => Some(&v.review),
            _ => None,
        }
    }

    pub fn worktree(&self) -> Option<&WorktreeRow> {
        match &self.data {
            NodeData::Agent { worktree, .. } => worktree.as_ref(),
            NodeData::Worktree(v) => Some(&v.worktree),
            NodeData::Session { worktree, .. } => worktree.as_ref(),
            NodeData::Pane(p) => p.worktree.as_ref(),
            NodeData::Review(v) => v.worktrees.first(),
            NodeData::Project { .. } | NodeData::Provider { .. } | NodeData::Machine(_) => None,
        }
    }

    /// Name used by `fleet agent send/stop/rename` lookups and popup titles.
    pub fn name(&self) -> String {
        match &self.data {
            NodeData::Project { name, .. } => name.clone(),
            NodeData::Agent {
                worktree, agent, ..
            } => worktree
                .as_ref()
                .map(|w| w.name.clone())
                .unwrap_or_else(|| agent.cwd.display().to_string()),
            NodeData::Worktree(v) => v.worktree.name.clone(),
            NodeData::Session { name, .. } => name.clone(),
            NodeData::Pane(p) => p.pane.session.clone(),
            NodeData::Review(v) => v
                .worktrees
                .first()
                .map(|w| w.name.clone())
                .unwrap_or_else(|| v.review.display_id.clone()),
            NodeData::Provider { name, .. } => name.clone(),
            NodeData::Machine(m) => m.row.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(n: &str, children: Vec<TreeNode>) -> TreeNode {
        TreeNode::new(
            RowKey::Project(n.into()),
            NodeData::Project {
                name: n.into(),
                summary: String::new(),
            },
            children,
        )
    }

    #[test]
    fn flatten_and_parent() {
        let tree = vec![
            proj(
                "a",
                vec![proj("a1", vec![]), proj("a2", vec![proj("a2x", vec![])])],
            ),
            proj("b", vec![]),
        ];
        let mut unfolded = HashSet::new();
        let v = flatten_keys(&tree_clone(&tree), &unfolded);
        assert_eq!(v, vec!["a", "b"]);
        unfolded.insert(RowKey::Project("a".into()));
        let v = flatten_keys(&tree_clone(&tree), &unfolded);
        assert_eq!(v, vec!["a", "a1", "a2", "b"]);
        unfolded.insert(RowKey::Project("a2".into()));
        let nodes = flatten(tree_clone(&tree), &unfolded, "");
        assert_eq!(
            nodes.iter().map(|n| n.depth).collect::<Vec<_>>(),
            vec![0, 1, 1, 2, 0]
        );
        assert_eq!(parent_index(&nodes, 3), Some(2));
        assert_eq!(parent_index(&nodes, 1), Some(0));
        assert_eq!(parent_index(&nodes, 0), None);
        // filter keeps the parent chain of a matching leaf
        let nodes = flatten(tree_clone(&tree), &unfolded, "a2x");
        assert_eq!(
            nodes.iter().map(|n| n.name()).collect::<Vec<_>>(),
            vec!["a", "a2", "a2x"]
        );
    }

    fn tree_clone(t: &[TreeNode]) -> Vec<TreeNode> {
        t.iter()
            .map(|n| TreeNode {
                key: n.key.clone(),
                data: n.data.clone(),
                children: tree_clone(&n.children),
            })
            .collect()
    }
    fn flatten_keys(t: &[TreeNode], u: &HashSet<RowKey>) -> Vec<String> {
        flatten(tree_clone(t), u, "")
            .iter()
            .map(|n| n.name())
            .collect()
    }
}
