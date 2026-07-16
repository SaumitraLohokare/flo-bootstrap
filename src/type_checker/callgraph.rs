use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, Module};

pub(super) struct CallGraph;

impl CallGraph {
    pub(super) fn build<'a>(module: &'a Module) -> Vec<Vec<&'a str>> {
        let mut adj = HashMap::new();

        // A name depends on every callee named in *any* of its overloads' bodies.
        // Grouping by name (rather than per-overload) keeps SCCs a safe
        // over-approximation for the bottom-up fixpoint.
        for (name, overloads) in &module.funcs {
            let calls = overloads
                .iter()
                .flat_map(|func| Self::collect_calls(&func.body))
                .collect();
            adj.insert(name.as_str(), calls);
        }

        Self::tarjans(adj)
    }

    fn collect_calls<'a>(expr: &'a Expr) -> Vec<&'a str> {
        use ExprKind::*;
        let mut calls = Vec::new();

        match &expr.kind {
            Num(_) | Bool(_) | Var(_) | Intrinsic => {}
            Call(name, args) => {
                calls.push(name.as_str());
                for arg in args {
                    calls.extend(Self::collect_calls(&arg));
                }
            }
        }

        calls
    }

    /// Returns SCCs in reverse topological order w.r.t. the call edges.
    /// That means: if A calls B (edge A -> B), then B's SCC appears
    /// *before* A's SCC in the returned Vec. This is the natural order
    /// for e.g. bottom-up type inference or bottom-up analysis, since
    /// callees are processed before callers.
    fn tarjans<'a>(graph: HashMap<&'a str, Vec<&'a str>>) -> Vec<Vec<&'a str>> {
        struct TarjanState<'a> {
            index: HashMap<&'a str, usize>,
            lowlink: HashMap<&'a str, usize>,
            on_stack: HashMap<&'a str, bool>,
            stack: Vec<&'a str>,
            next_index: usize,
            sccs: Vec<Vec<&'a str>>,
        }

        fn strongconnect<'a>(
            v: &'a str,
            graph: &HashMap<&'a str, Vec<&'a str>>,
            st: &mut TarjanState<'a>,
        ) {
            st.index.insert(v, st.next_index);
            st.lowlink.insert(v, st.next_index);
            st.next_index += 1;
            st.stack.push(v);
            st.on_stack.insert(v, true);

            if let Some(succs) = graph.get(v) {
                for &w in succs {
                    if !st.index.contains_key(w) {
                        // w not yet visited: recurse
                        strongconnect(w, graph, st);
                        let w_low = st.lowlink[w];
                        let v_low = st.lowlink[v];
                        st.lowlink.insert(v, v_low.min(w_low));
                    } else if *st.on_stack.get(w).unwrap_or(&false) {
                        // w is on stack: back edge, use its index
                        let w_idx = st.index[w];
                        let v_low = st.lowlink[v];
                        st.lowlink.insert(v, v_low.min(w_idx));
                    }
                    // else: w visited, not on stack -> already in a
                    // completed SCC, ignore (cross edge).
                }
            }

            // If v is a root node, pop the stack and produce an SCC.
            if st.lowlink[v] == st.index[v] {
                let mut component = Vec::new();
                loop {
                    let w = st.stack.pop().expect("stack not empty");
                    st.on_stack.insert(w, false);
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                st.sccs.push(component);
            }
        }

        let mut st = TarjanState {
            index: HashMap::new(),
            lowlink: HashMap::new(),
            on_stack: HashMap::new(),
            stack: Vec::new(),
            next_index: 0,
            sccs: Vec::new(),
        };

        // Iterate over all known nodes (both callers and any callees
        // that might not be keys in `adj`, e.g. external/undefined funcs).
        let mut all_nodes: Vec<&'a str> = graph.keys().copied().collect();
        for callees in graph.values() {
            for &c in callees {
                if !graph.contains_key(c) {
                    all_nodes.push(c);
                }
            }
        }
        all_nodes.sort();
        all_nodes.dedup();

        for node in all_nodes {
            if !st.index.contains_key(node) {
                strongconnect(node, &graph, &mut st);
            }
        }

        st.sccs
    }
}
