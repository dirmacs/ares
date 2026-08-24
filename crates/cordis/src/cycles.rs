//! Load-time dependency-cycle detection (Cordis paper §6.5).
//!
//! A mutual inject dependency (`A` declares an inject on what `B` provides
//! and vice versa) makes both fibers permanently inactive: each waits for the
//! other's provider to become active, so neither ever satisfies
//! [`crate::FiberState::Active`]. The outcome is fully predictable from the
//! declarations alone, which means it can be reported at load time instead of
//! silently leaving a pair of inactive fibers behind.
//!
//! Detection runs over an explicitly built [`DependencyGraph`] whose nodes are
//! fiber ids and whose edges point from a consumer fiber to the provider fiber
//! of one of its declared injects. The loader reconstructs those edges after
//! applying a batch of entries — see [`CycleLedger`] and its caller in
//! [`crate::loader::Loader::apply`] — and reports any cycle without failing
//! the apply.
//!
//! The core search is a color-based depth-first walk ([`find_dependency_cycle`]
//! / [`find_dependency_cycles`]): white = unvisited, gray = on the current DFS
//! stack, black = fully explored. An edge into a gray node closes a cycle; the
//! gray stack from that node is the cycle itself.

use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{Context, FiberId};

/// Directed inject-dependency graph over registration fibers.
///
/// Nodes are fiber ids; an edge `a -> b` records "fiber `a` declares an
/// inject on the service type provided by fiber `b`". Neighbor lists keep
/// insertion order but duplicates are collapsed, and search visits nodes in
/// ascending order, so results are deterministic across runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    adjacency: BTreeMap<FiberId, Vec<FiberId>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node with no outgoing edges (idempotent).
    pub fn add_node(&mut self, id: FiberId) {
        self.adjacency.entry(id).or_default();
    }

    /// Add a directed edge `from -> to`; both endpoints become nodes.
    /// Duplicate edges are collapsed.
    pub fn add_edge(&mut self, from: FiberId, to: FiberId) {
        let targets = self.adjacency.entry(from).or_default();
        if !targets.contains(&to) {
            targets.push(to);
        }
    }

    /// All node ids in ascending order.
    pub fn node_ids(&self) -> impl Iterator<Item = FiberId> + '_ {
        self.adjacency.keys().copied()
    }

    /// Outgoing edges of `id` (empty when `id` has none recorded).
    pub fn neighbors(&self, id: FiberId) -> &[FiberId] {
        self.adjacency.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.adjacency.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adjacency.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Color {
    White,
    Gray,
    Black,
}

/// Find one dependency cycle in `graph`, or `None` when acyclic.
///
/// The returned path is the closed cycle in edge direction, starting and
/// ending at the same node (e.g. `[2, 3, 4, 2]`). Deterministic: nodes and
/// neighbors are visited in ascending order. See [`find_dependency_cycles`]
/// for the many-cycles variant.
pub fn find_dependency_cycle(graph: &DependencyGraph) -> Option<Vec<FiberId>> {
    find_dependency_cycles(graph).into_iter().next()
}

/// Find every dependency cycle in `graph`.
///
/// Iterative DFS with colors collects one cycle per back edge; results are
/// deduplicated up to rotation (the same ring entered at different points
/// counts once) and returned in ascending order of their smallest member.
pub fn find_dependency_cycles(graph: &DependencyGraph) -> Vec<Vec<FiberId>> {
    let mut color: HashMap<FiberId, Color> = graph.node_ids().map(|n| (n, Color::White)).collect();
    // Explicit stack frames: (node, number of neighbors already dispatched).
    let mut stack: Vec<(FiberId, usize)> = Vec::new();
    let mut found: BTreeSet<Vec<FiberId>> = BTreeSet::new();

    for root in graph.node_ids() {
        if color.get(&root) != Some(&Color::White) {
            continue;
        }
        color.insert(root, Color::Gray);
        stack.push((root, 0));
        while let Some(&mut (node, ref mut idx)) = stack.last_mut() {
            let neighbors = graph.neighbors(node).to_vec();
            if *idx >= neighbors.len() {
                // Fully explored: blacken, unwind one level, advance parent.
                color.insert(node, Color::Black);
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.1 += 1;
                }
                continue;
            }
            let next = neighbors[*idx];
            let back_edge = matches!(color.get(&next), Some(Color::Gray));
            if back_edge {
                found.insert(canonical_cycle(gray_stack_cycle(&stack, next)));
                stack.last_mut().expect("non-empty").1 += 1;
                continue;
            }
            match color.get(&next) {
                Some(Color::Black) => {
                    stack.last_mut().expect("non-empty").1 += 1;
                }
                _ => {
                    color.insert(next, Color::Gray);
                    stack.push((next, 0));
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Extract the cycle from the explicit gray DFS stack: everything from the
/// last occurrence of `back_edge_target` to the top, then the target again.
fn gray_stack_cycle(stack: &[(FiberId, usize)], back_edge_target: FiberId) -> Vec<FiberId> {
    let start = stack
        .iter()
        .rposition(|(id, _)| *id == back_edge_target)
        .unwrap_or(0);
    let mut cycle: Vec<FiberId> = stack[start..].iter().map(|(id, _)| *id).collect();
    cycle.push(back_edge_target);
    cycle
}

/// Normalize a closed cycle to its canonical rotation (smallest id first).
fn canonical_cycle(closed: Vec<FiberId>) -> Vec<FiberId> {
    let ring = closed.len().saturating_sub(1);
    let mut best: Option<Vec<FiberId>> = None;
    for shift in 0..ring.max(1) {
        let rotated: Vec<FiberId> = (0..ring)
            .map(|i| closed[(shift + i) % ring.max(1)])
            .chain(std::iter::once(closed[shift % ring.max(1)]))
            .collect();
        best = Some(match best {
            None => rotated,
            Some(b) if rotated < b => rotated,
            Some(b) => b,
        });
    }
    best.unwrap_or(closed)
}

/// Loader-side ledger reconstructing the inject graph after entries apply.
///
/// [`crate::loader::Loader::instantiate_entry`] diffs the context store before
/// and after a factory runs; every newly inserted TypeId is recorded here with
/// the entry's tracked fiber id, and the entry id is kept alongside so warning
/// reports can name entries rather than bare fiber numbers. Because the
/// registry's internal provided-map has no read accessor, this ledger is the
/// reconstruction source for the post-apply inject graph. It is provided
/// lazily by `instantiate_entry` itself, lives inside the crate, and is
/// intentionally not part of the public API surface.
#[derive(Default)]
pub(crate) struct CycleLedger {
    /// `(provided type, isolate realm) -> provider fiber` (last writer wins,
    /// matching re-registration semantics).
    providers: RwLock<HashMap<(TypeId, Option<String>), FiberId>>,
    /// `fiber -> owning entry id` (last writer wins).
    fibers: RwLock<BTreeMap<FiberId, String>>,
}

impl CycleLedger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record that `fid` provides `tid` at isolate realm `label`
    /// (`None` = root realm).
    pub(crate) fn record_provider(&self, tid: TypeId, label: Option<&str>, fid: FiberId) {
        self.providers
            .write()
            .insert((tid, label.map(str::to_string)), fid);
    }

    /// Record the entry id owning `fid`.
    pub(crate) fn note_entry(&self, fid: FiberId, entry_id: &str) {
        self.fibers.write().insert(fid, entry_id.to_string());
    }

    /// Provider fiber for `tid` at `label`, if known to the ledger.
    pub(crate) fn provider_of(&self, tid: TypeId, label: Option<&str>) -> Option<FiberId> {
        self.providers
            .read()
            .get(&(tid, label.map(str::to_string)))
            .copied()
    }

    /// Distinct tracked provider fibers, ascending.
    pub(crate) fn provider_fibers(&self) -> Vec<FiberId> {
        let mut ids: BTreeSet<FiberId> = self.providers.read().values().copied().collect();
        ids.extend(self.fibers.read().keys().copied());
        ids.into_iter().collect()
    }

    /// Entry id owning `fid`, if recorded.
    pub(crate) fn entry_id_of(&self, fid: FiberId) -> Option<String> {
        self.fibers.read().get(&fid).cloned()
    }

    /// Number of distinct `(type, realm)` keys tracked.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.providers.read().len()
    }
}

impl crate::Service for CycleLedger {}

/// Reconstruct the post-apply inject-dependency graph from the loader ledger
/// and registry lookups. `None` when the ledger or registry was never
/// provided (library deployments without the loader path).
///
/// Realm resolution: loader-managed fibers share the root context, so a
/// consumer's isolate namespace for an injected type equals
/// `ctx.isolate_label(tid)` — the same value the provider was recorded under.
/// Fibers the registry no longer tracks (verified rebuilds remove their old
/// fiber) contribute nothing; classic-retired fibers linger disposed but
/// their stale declarations may still contribute edges, which is conservative
/// (over-reporting beats silence for a load-time warning).
pub(crate) fn build_dependency_graph(ctx: &Arc<Context>) -> Option<DependencyGraph> {
    let ledger = ctx.get::<CycleLedger>()?;
    let registry = ctx.get::<crate::RegistryService>()?;
    let mut graph = DependencyGraph::new();
    for fid in ledger.provider_fibers() {
        graph.add_node(fid);
        let Some(fiber) = registry.get_fiber(fid) else {
            continue;
        };
        for tid in fiber.injected_type_ids() {
            let label = ctx.isolate_label(tid);
            if let Some(provider) = ledger.provider_of(tid, label.as_deref()) {
                // A fiber injecting its own provided type resolves against
                // itself and is satisfied — not a cycle.
                if provider != fid {
                    graph.add_edge(fid, provider);
                }
            }
        }
    }
    Some(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_from_edges(edges: &[(FiberId, FiberId)]) -> DependencyGraph {
        let mut g = DependencyGraph::new();
        for &(from, to) in edges {
            g.add_edge(from, to);
        }
        g
    }

    #[test]
    fn empty_and_single_node_graphs_have_no_cycle() {
        assert!(find_dependency_cycle(&DependencyGraph::new()).is_none());
        assert!(find_dependency_cycles(&DependencyGraph::new()).is_empty());
        let mut g = DependencyGraph::new();
        g.add_node(1);
        assert!(find_dependency_cycle(&g).is_none());
    }

    #[test]
    fn acyclic_chain_has_no_cycle() {
        let g = graph_from_edges(&[(1, 2), (2, 3), (3, 4)]);
        assert!(find_dependency_cycle(&g).is_none());
    }

    #[test]
    fn diamond_is_acyclic() {
        // Two paths converge; shared dependencies must not read as a cycle.
        let g = graph_from_edges(&[(1, 2), (1, 3), (2, 4), (3, 4)]);
        assert!(find_dependency_cycle(&g).is_none());
    }

    #[test]
    fn two_node_cycle_is_found_in_edge_direction() {
        // A -> B -> A: cycle reported [1, 2, 1].
        let g = graph_from_edges(&[(1, 2), (2, 1)]);
        assert_eq!(find_dependency_cycle(&g), Some(vec![1, 2, 1]));
    }

    #[test]
    fn three_node_cycle_is_found() {
        let g = graph_from_edges(&[(10, 20), (20, 30), (30, 10)]);
        assert_eq!(find_dependency_cycle(&g), Some(vec![10, 20, 30, 10]));
    }

    #[test]
    fn cycle_nested_behind_acyclic_prefix_is_found() {
        // 1 -> 2 -> {3,4} with 3 -> 4 -> 3; walk must descend into the cycle.
        let g = graph_from_edges(&[(1, 2), (2, 3), (2, 4), (3, 4), (4, 3)]);
        let found = find_dependency_cycle(&g).expect("cycle exists");
        assert_eq!(&found[..], &[3, 4, 3]);
    }

    #[test]
    fn self_loop_reports_two_node_path() {
        let g = graph_from_edges(&[(7, 7)]);
        assert_eq!(find_dependency_cycle(&g), Some(vec![7, 7]));
    }

    #[test]
    fn disconnected_components_only_yield_their_own_cycle() {
        // Acyclic component (1..=3) plus cyclic component (4 <-> 5).
        let g = graph_from_edges(&[(1, 2), (2, 3), (4, 5), (5, 4)]);
        assert_eq!(find_dependency_cycle(&g), Some(vec![4, 5, 4]));
    }

    #[test]
    fn multiple_disjoint_cycles_are_all_reported_once() {
        let g = graph_from_edges(&[(1, 2), (2, 1), (5, 4), (4, 5), (3, 3)]);
        let found = find_dependency_cycles(&g);
        assert_eq!(found, vec![vec![1, 2, 1], vec![3, 3], vec![4, 5, 4]]);
        // Single-shot variant reports the first deterministically.
        assert_eq!(find_dependency_cycle(&g), Some(vec![1, 2, 1]));
    }

    #[test]
    fn duplicate_edges_do_not_confuse_the_walk() {
        let mut g = DependencyGraph::new();
        g.add_edge(1, 2);
        g.add_edge(1, 2);
        g.add_edge(2, 1);
        assert_eq!(find_dependency_cycle(&g), Some(vec![1, 2, 1]));
        assert_eq!(g.neighbors(1), &[2]);
    }

    #[test]
    fn cross_edges_into_explored_subtrees_stay_acyclic() {
        // Deep chain revisited via cross edges must not be misreported.
        let g = graph_from_edges(&[(1, 2), (2, 3), (1, 3), (3, 4)]);
        assert!(find_dependency_cycle(&g).is_none());
    }

    #[test]
    fn ledger_round_trips_provider_lookup() {
        use crate::Service;
        #[derive(Debug)]
        struct Probe;
        impl Service for Probe {}

        let ledger = CycleLedger::new();
        let tid = TypeId::of::<Probe>();
        assert!(ledger.provider_of(tid, None).is_none());
        ledger.record_provider(tid, None, 11);
        ledger.record_provider(tid, Some("tenant:acme"), 12);
        ledger.note_entry(11, "svc:a");
        assert_eq!(ledger.provider_of(tid, None), Some(11));
        assert_eq!(ledger.provider_of(tid, Some("tenant:acme")), Some(12));
        assert_eq!(ledger.provider_of(tid, Some("tenant:other")), None);
        // Re-record wins (mirrors re-registration semantics).
        ledger.record_provider(tid, None, 13);
        assert_eq!(ledger.provider_of(tid, None), Some(13));
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.entry_id_of(11).as_deref(), Some("svc:a"));
        assert_eq!(ledger.entry_id_of(999), None);
        // provider_fibers unions provider-map ids (12, 13) with the entry
        // registry keys (11) — 11 is only an entry owner here, not a provider.
        assert_eq!(ledger.provider_fibers(), vec![11, 12, 13]);
    }

    #[test]
    fn sorted_node_enumeration_keeps_results_deterministic() {
        let mut g = DependencyGraph::new();
        g.add_node(9);
        g.add_node(2);
        g.add_node(5);
        let nodes: BTreeSet<_> = g.node_ids().collect();
        assert_eq!(nodes, BTreeSet::from([2, 5, 9]));
        assert_eq!(g.len(), 3);
        assert!(!g.is_empty());
    }
}
