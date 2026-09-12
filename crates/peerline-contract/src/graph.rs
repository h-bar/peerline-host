//! The dependency graph a set of contracts induces, and the cycles in it.
//!
//! # Why this lives here
//!
//! Two things in the cluster layer build the same relation from different
//! inputs: a supervisor validating a config before it starts anything (from
//! probed manifests) and a coordinator explaining a readiness timeout (from
//! live registrations). They are looking at one cluster, and if they disagree
//! about its shape then an operator who sees a cycle at preflight and no cycle
//! in a timeout — or the reverse — learns nothing from either.
//!
//! They used to be kept in step by a pair of reciprocal comments that said
//! "if this definition changes, that one changes with it". That works exactly
//! until someone changes one of them, which is the only time it matters. So
//! the definition is here, in the crate both of them already depend on, and
//! there is only one of it.
//!
//! This module is deliberately dependency-free — `std` collections and
//! [`ContractId`], nothing else. It holds no wire types and serializes
//! nothing: a graph is derived from contracts on demand, never sent.
//!
//! # The definition
//!
//! **An edge is `produced -> consumed` for every pair declared by one
//! service**, read as "this contract cannot be served until that one is".
//!
//! Two properties of that sentence are load-bearing and were each once
//! written the other way:
//!
//! - **Optional dependencies are included.** A cycle through an optional edge
//!   is no deadlock, since an optional contract never blocks anyone — true,
//!   and not worth two answers. Every report built from this graph is
//!   advisory and says "possible cycle"; the caller separately names exactly
//!   which *required* contracts are outstanding; and an operator tracing a
//!   hang is better served by the whole declared shape than by a version of it
//!   filtered on a distinction that only holds at runtime.
//! - **Pairs are per service, not per process.** Joining every contract an
//!   instance produces to everything any of its services consumes is a
//!   coarser over-approximation, and it invents cycles inside a process that
//!   merely mounts two unrelated services.
//!
//! # Every cycle here is advisory, and that is not a limitation to fix
//!
//! Collapsing instances into contracts invents cycles no real deployment has.
//! With `storage-sqlite` producing `storage/1`, `storage-cache` also producing
//! `storage/1` while consuming `index/1`, and `indexer` producing `index/1`
//! while consuming `storage/1`, this graph has a cycle — and the deployment
//! resolves fine: `indexer` binds `storage-sqlite`, then `storage-cache` binds
//! `indexer`. The over-approximation gets *worse* exactly as redundancy is
//! added, which is the thing a selection policy exists to support.
//!
//! Detecting a real deadlock is a search over producer assignments, not a
//! graph walk, because bindings are chosen at runtime. So nothing built from
//! this graph may reject a registration, fail a resolve or refuse a config.
//! It may enrich an error, and that is all.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ContractId;

/// The contract-level dependency graph: which contracts must be served before
/// which others can be.
///
/// Build one with [`from_services`](Self::from_services), then ask it for
/// cycles. Vertices are only the contracts that something *produces*: a
/// contract nobody produces has no outgoing edges and cannot be part of a
/// cycle, so it is not a vertex.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractGraph {
    edges: BTreeMap<ContractId, BTreeSet<ContractId>>,
}

impl ContractGraph {
    /// Build the graph from one `(produces, consumes)` pair per service.
    ///
    /// The iterator yields one item per **service**, because per-service is
    /// the grouping the edge definition is stated in (see the module docs);
    /// handing it one item per process would silently coarsen the result.
    ///
    /// The service's own name is deliberately not taken. Nothing derived from
    /// this graph reports which service closes a cycle — a cycle is a claim
    /// about contracts, and the services behind it are whichever ones happen
    /// to declare those pairs — so a name parameter would be a field this type
    /// accepts and then throws away, which is worse than not asking for it.
    ///
    /// ```
    /// use peerline_contract::{ContractId, graph::ContractGraph};
    ///
    /// let storage = ContractId::new("storage", 1);
    /// let index = ContractId::new("index", 1);
    /// // One service produces `index/1` and consumes `storage/1`; another
    /// // produces `storage/1` and consumes `index/1`.
    /// let graph = ContractGraph::from_services([
    ///     (vec![&index], vec![&storage]),
    ///     (vec![&storage], vec![&index]),
    /// ]);
    /// assert_eq!(
    ///     graph.cycle_through(&index),
    ///     Some(vec![index.clone(), storage, index]),
    /// );
    /// ```
    pub fn from_services<'a, S, P, C>(services: S) -> Self
    where
        S: IntoIterator<Item = (P, C)>,
        P: IntoIterator<Item = &'a ContractId>,
        C: IntoIterator<Item = &'a ContractId>,
    {
        let mut edges: BTreeMap<ContractId, BTreeSet<ContractId>> = BTreeMap::new();
        for (produces, consumes) in services {
            // Collected because it is walked once per produced contract and a
            // caller's `consumes` is an iterator, which can only be walked
            // once. A service declares a handful of each.
            let consumed: Vec<&ContractId> = consumes.into_iter().collect();
            for produced in produces {
                // `or_default` even when nothing is consumed: a producer with
                // no dependencies is still a vertex, and omitting it would
                // make "is this contract produced at all" unanswerable here.
                let out = edges.entry(produced.clone()).or_default();
                out.extend(consumed.iter().map(|c| (*c).clone()));
            }
        }
        Self { edges }
    }

    /// The adjacency map, `produced -> everything producing it depends on`.
    pub fn edges(&self) -> &BTreeMap<ContractId, BTreeSet<ContractId>> {
        &self.edges
    }

    /// No produced contracts at all — so nothing to walk.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// The shortest cycle passing through `start`, as a path with the head
    /// repeated at the end (`index/1 -> storage/1 -> index/1`), or `None`.
    ///
    /// For the caller that already knows which contract it is stuck on and
    /// wants to say why. Breadth-first rather than depth-first so the reported
    /// cycle is the *shortest* one through `start`, which is the one a human
    /// can actually read in an error message; a depth-first walk would just as
    /// happily return a twelve-hop tour of the whole component.
    ///
    /// `start` need not be a vertex. A contract nothing produces trivially has
    /// no cycle through it, and answering `None` is better than making every
    /// caller check first.
    pub fn cycle_through<'s>(&'s self, start: &'s ContractId) -> Option<Vec<ContractId>> {
        let mut parent: BTreeMap<&ContractId, &ContractId> = BTreeMap::new();
        let mut queue: VecDeque<&ContractId> = VecDeque::new();
        queue.push_back(start);
        let mut closed: Option<&ContractId> = None;
        'search: while let Some(node) = queue.pop_front() {
            for next in self.edges.get(node).into_iter().flatten() {
                if next == start {
                    // The edge back to `start` closes it; `node` is the last
                    // hop before the head.
                    closed = Some(node);
                    break 'search;
                }
                if !parent.contains_key(next) {
                    parent.insert(next, node);
                    queue.push_back(next);
                }
            }
        }

        let last = closed?;
        // Walk the parent links back from the closing hop to the head, then
        // reverse: BFS records how it arrived at each vertex, not where it
        // went.
        let mut back = Vec::new();
        let mut node = last;
        while node != start {
            back.push(node.clone());
            match parent.get(node) {
                Some(p) => node = p,
                None => break,
            }
        }
        back.reverse();
        let mut path = vec![start.clone()];
        path.extend(back);
        path.push(start.clone());
        Some(path)
    }

    /// One cycle per tangle, as a path **without** the head repeated
    /// (`index/1 -> storage/1`, understood to close).
    ///
    /// For the caller with no particular contract in mind that wants to report
    /// everything suspicious in a config. One cycle per strongly connected
    /// component rather than every elementary cycle: enumerating them all is
    /// exponential in the worst case, and the report is a hint for a human
    /// reading a validation summary, not an input to a decision — one concrete
    /// example per tangle is what makes it readable.
    ///
    /// Deterministic: components come out of a depth-first walk in sorted
    /// vertex order, and each cycle is found from its component's smallest
    /// vertex.
    pub fn cycles(&self) -> Vec<Vec<ContractId>> {
        Scc::of(&self.edges)
            .into_iter()
            .filter_map(|component| self.cycle_within(&component))
            .collect()
    }

    /// One concrete cycle inside a strongly connected component, as a path.
    ///
    /// Any cycle will do — the point is to show a human something specific —
    /// so this walks from the component's smallest vertex and returns the
    /// first cycle it closes, which makes the choice deterministic.
    fn cycle_within(&self, component: &BTreeSet<ContractId>) -> Option<Vec<ContractId>> {
        let start = component.iter().next()?;
        let mut path: Vec<ContractId> = vec![start.clone()];
        loop {
            let current = path.last().expect("non-empty");
            let next = self
                .edges
                .get(current)?
                .iter()
                .find(|c| component.contains(*c))?
                .clone();
            if let Some(at) = path.iter().position(|c| *c == next) {
                return Some(path.split_off(at));
            }
            path.push(next);
            // A simple path cannot be longer than the component.
            if path.len() > component.len() {
                return None;
            }
        }
    }
}

/// Tarjan's strongly connected components, iterative so that a deep graph
/// cannot blow the stack.
struct Scc<'a> {
    edges: &'a BTreeMap<ContractId, BTreeSet<ContractId>>,
    index: BTreeMap<&'a ContractId, usize>,
    low: BTreeMap<&'a ContractId, usize>,
    on_stack: BTreeSet<&'a ContractId>,
    stack: Vec<&'a ContractId>,
    next_index: usize,
    components: Vec<BTreeSet<ContractId>>,
}

impl<'a> Scc<'a> {
    /// The components with a cycle in them: size > 1, or a single vertex with
    /// an edge to itself.
    fn of(edges: &'a BTreeMap<ContractId, BTreeSet<ContractId>>) -> Vec<BTreeSet<ContractId>> {
        let mut scc = Self {
            edges,
            index: BTreeMap::new(),
            low: BTreeMap::new(),
            on_stack: BTreeSet::new(),
            stack: Vec::new(),
            next_index: 0,
            components: Vec::new(),
        };
        for vertex in edges.keys() {
            if !scc.index.contains_key(vertex) {
                scc.walk(vertex);
            }
        }
        scc.components
            .into_iter()
            .filter(|c| {
                c.len() > 1
                    || c.iter()
                        .next()
                        .is_some_and(|v| edges.get(v).is_some_and(|out| out.contains(v)))
            })
            .collect()
    }

    fn walk(&mut self, root: &'a ContractId) {
        // (vertex, index of the next successor to visit)
        let mut frames: Vec<(&'a ContractId, usize)> = vec![(root, 0)];
        self.enter(root);
        while let Some((vertex, cursor)) = frames.pop() {
            let successors: Vec<&'a ContractId> = self
                .edges
                .get(vertex)
                .map(|s| s.iter().collect())
                .unwrap_or_default();
            if cursor < successors.len() {
                let next = successors[cursor];
                frames.push((vertex, cursor + 1));
                if !self.index.contains_key(next) {
                    self.enter(next);
                    frames.push((next, 0));
                } else if self.on_stack.contains(next) {
                    let candidate = self.index[next];
                    let low = self.low.get_mut(vertex).expect("visited");
                    *low = (*low).min(candidate);
                }
                continue;
            }
            // All successors done: close the vertex.
            if self.low[vertex] == self.index[vertex] {
                let mut component = BTreeSet::new();
                while let Some(top) = self.stack.pop() {
                    self.on_stack.remove(top);
                    component.insert(top.clone());
                    if top == vertex {
                        break;
                    }
                }
                self.components.push(component);
            }
            if let Some((parent, _)) = frames.last() {
                let child_low = self.low[vertex];
                let low = self.low.get_mut(*parent).expect("visited");
                *low = (*low).min(child_low);
            }
        }
    }

    fn enter(&mut self, vertex: &'a ContractId) {
        self.index.insert(vertex, self.next_index);
        self.low.insert(vertex, self.next_index);
        self.next_index += 1;
        self.stack.push(vertex);
        self.on_stack.insert(vertex);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> ContractId {
        ContractId::new(name, 1)
    }

    /// The definition, stated as a test: per service, produced to consumed,
    /// and a producer with no dependencies is still a vertex.
    #[test]
    fn an_edge_joins_every_produced_contract_to_every_consumed_one() {
        let (a, b, c) = (id("a"), id("b"), id("c"));
        let graph = ContractGraph::from_services([(vec![&a, &b], vec![&c]), (vec![&c], vec![])]);
        assert_eq!(graph.edges()[&a], BTreeSet::from([c.clone()]));
        assert_eq!(graph.edges()[&b], BTreeSet::from([c.clone()]));
        assert_eq!(graph.edges()[&c], BTreeSet::new());
        // `c` is consumed by the first service but that does not make it
        // depend on anything; only producing does.
        assert_eq!(graph.edges().len(), 3);
    }

    /// Two services whose contracts are unrelated must not be joined just
    /// because one caller happened to hand them over together. Grouping is the
    /// caller's statement about the cluster, and this is the assertion that it
    /// is honoured.
    #[test]
    fn services_are_not_joined_to_each_other() {
        let (a, b, c, d) = (id("a"), id("b"), id("c"), id("d"));
        let graph = ContractGraph::from_services([(vec![&a], vec![&b]), (vec![&c], vec![&d])]);
        assert_eq!(graph.edges()[&a], BTreeSet::from([b]));
        assert_eq!(graph.edges()[&c], BTreeSet::from([d]));
        assert!(graph.cycles().is_empty());
    }

    #[test]
    fn a_graph_with_no_cycle_reports_none() {
        let (a, b) = (id("a"), id("b"));
        let graph = ContractGraph::from_services([(vec![&a], vec![&b])]);
        assert_eq!(graph.cycle_through(&a), None);
        assert_eq!(graph.cycle_through(&b), None);
        assert!(graph.cycles().is_empty());
        assert!(!graph.is_empty(), "`a` is a vertex even with no cycle");
        assert!(ContractGraph::default().is_empty());
    }

    /// The two walks answer the same question in the two shapes their callers
    /// need: one repeats the head so the path reads as a loop in a message,
    /// the other does not because its renderer closes it.
    #[test]
    fn the_two_walks_agree_on_a_cycle_and_differ_only_in_shape() {
        let (a, b) = (id("a"), id("b"));
        let graph = ContractGraph::from_services([(vec![&a], vec![&b]), (vec![&b], vec![&a])]);
        assert_eq!(
            graph.cycle_through(&a),
            Some(vec![a.clone(), b.clone(), a.clone()]),
        );
        assert_eq!(graph.cycles(), vec![vec![a, b]]);
    }

    /// A contract that depends on itself is a cycle of one, and both walks say
    /// so — the single-vertex component is the case Tarjan's filter has to
    /// special-case, and the BFS one is the case where the first edge examined
    /// closes the loop.
    #[test]
    fn a_self_edge_is_a_cycle() {
        let a = id("a");
        let graph = ContractGraph::from_services([(vec![&a], vec![&a])]);
        assert_eq!(graph.cycle_through(&a), Some(vec![a.clone(), a.clone()]));
        assert_eq!(graph.cycles(), vec![vec![a]]);
    }

    /// Breadth-first, so the reported path is the shortest way home rather
    /// than whichever one a depth-first walk stumbled into first. Here `a`
    /// closes through `b` in two hops and through `c -> d` in three.
    #[test]
    fn the_reported_cycle_is_the_shortest_one_through_the_start() {
        let (a, b, c, d) = (id("a"), id("b"), id("c"), id("d"));
        let graph = ContractGraph::from_services([
            (vec![&a], vec![&b, &c]),
            (vec![&b], vec![&a]),
            (vec![&c], vec![&d]),
            (vec![&d], vec![&a]),
        ]);
        assert_eq!(
            graph.cycle_through(&a),
            Some(vec![a.clone(), b.clone(), a.clone()]),
        );
    }

    /// Asking about a contract nothing produces is legal and answers `None`,
    /// so a caller holding a list of outstanding contracts need not filter it
    /// first.
    #[test]
    fn a_contract_that_is_not_a_vertex_has_no_cycle() {
        let (a, b) = (id("a"), id("b"));
        let graph = ContractGraph::from_services([(vec![&a], vec![&b])]);
        assert_eq!(graph.cycle_through(&id("absent")), None);
    }

    /// Two independent tangles are reported once each, not once per
    /// elementary cycle, and in a deterministic order.
    #[test]
    fn one_cycle_is_reported_per_tangle() {
        let (a, b, c, d, e) = (id("a"), id("b"), id("c"), id("d"), id("e"));
        let graph = ContractGraph::from_services([
            (vec![&a], vec![&b]),
            (vec![&b], vec![&a]),
            (vec![&c], vec![&d]),
            (vec![&d], vec![&c]),
            (vec![&e], vec![]),
        ]);
        let cycles = graph.cycles();
        assert_eq!(cycles, vec![vec![a, b], vec![c, d]]);
        // Rebuilding must not reorder anything: this report is compared
        // across runs by humans reading two validation summaries.
        assert_eq!(graph.cycles(), cycles);
    }
}
