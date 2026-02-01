use std::collections::{HashMap, HashSet};

/// Detects cycles in a dependency graph using DFS.
pub(crate) fn detect_cycle<S: AsRef<str>>(
    dependencies: &HashMap<String, Vec<S>>,
) -> Option<Vec<String>> {
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    for node in dependencies.keys() {
        if dfs_cycle(node, dependencies, &mut visited, &mut rec_stack, &mut path) {
            return Some(path);
        }
    }

    None
}

fn dfs_cycle<S: AsRef<str>>(
    node: &str,
    graph: &HashMap<String, Vec<S>>,
    visited: &mut HashSet<String>,
    rec_stack: &mut HashSet<String>,
    path: &mut Vec<String>,
) -> bool {
    let node_str = node.to_string();

    if rec_stack.contains(&node_str) {
        path.push(node_str);
        return true;
    }

    if visited.contains(&node_str) {
        return false;
    }

    visited.insert(node_str.clone());
    rec_stack.insert(node_str.clone());
    path.push(node_str.clone());

    if let Some(deps) = graph.get(node) {
        for dep in deps {
            if dfs_cycle(dep.as_ref(), graph, visited, rec_stack, path) {
                return true;
            }
        }
    }

    rec_stack.remove(&node_str);
    path.pop();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cycle() {
        let mut deps = HashMap::new();
        deps.insert("A".to_string(), vec!["B", "C"]);
        deps.insert("B".to_string(), vec!["D"]);
        deps.insert("C".to_string(), vec!["D"]);
        deps.insert("D".to_string(), vec![]);

        assert!(detect_cycle(&deps).is_none());
    }

    #[test]
    fn simple_cycle() {
        let mut deps = HashMap::new();
        deps.insert("A".to_string(), vec!["B"]);
        deps.insert("B".to_string(), vec!["C"]);
        deps.insert("C".to_string(), vec!["A"]);

        let cycle = detect_cycle(&deps);
        assert!(cycle.is_some());
    }

    #[test]
    fn self_cycle() {
        let mut deps = HashMap::new();
        deps.insert("A".to_string(), vec!["A"]);

        let cycle = detect_cycle(&deps);
        assert!(cycle.is_some());
    }
}
