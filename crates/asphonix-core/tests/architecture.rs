//! Architecture guard: the top-level modules of `asphonix-core` form an acyclic dependency graph, and a few
//! layering rules hold. This keeps the crate splittable into component crates (docs/PLAN-architecture-logging.md,
//! phases 2 and 4) and stops new cycles from creeping in.
//!
//! The graph is built from `crate::<module>` paths in non-test code (everything after the first `#[cfg(test)]`
//! line of a file, and files named `tests.rs` / `*_tests.rs`, are ignored). Grouped imports (`use crate::{..}`)
//! are rejected so every edge is visible to this scan.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type Graph = BTreeMap<String, BTreeSet<String>>;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn top_module(src: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(src).unwrap();
    let first = rel.components().next().unwrap().as_os_str().to_string_lossy().to_string();
    first.trim_end_matches(".rs").to_string()
}

fn is_test_file(file: &Path) -> bool {
    let name = file.file_name().unwrap().to_string_lossy();
    name == "tests.rs" || name.ends_with("_tests.rs")
}

fn module_graph() -> (Graph, Vec<String>) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut graph = Graph::new();
    let mut grouped = Vec::new();
    for file in files {
        let from = top_module(&src, &file);
        if from == "lib" || is_test_file(&file) {
            continue;
        }
        graph.entry(from.clone()).or_default();
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(i) = rest.find("crate::") {
                rest = &rest[i + "crate::".len()..];
                if rest.starts_with('{') {
                    grouped.push(format!("{}:{}", file.display(), n + 1));
                    continue;
                }
                let name: String = rest.chars().take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit()).collect();
                if !name.is_empty() && name != from {
                    graph.get_mut(&from).unwrap().insert(name);
                }
            }
        }
    }
    (graph, grouped)
}

fn find_cycle(graph: &Graph) -> Option<Vec<String>> {
    fn visit(n: &str, g: &Graph, state: &mut BTreeMap<String, u8>, stack: &mut Vec<String>) -> Option<Vec<String>> {
        state.insert(n.to_string(), 1);
        stack.push(n.to_string());
        for m in g.get(n).into_iter().flatten() {
            match state.get(m.as_str()).copied().unwrap_or(0) {
                1 => {
                    let start = stack.iter().position(|s| s == m).unwrap();
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(m.clone());
                    return Some(cycle);
                }
                0 => {
                    if let Some(c) = visit(m, g, state, stack) {
                        return Some(c);
                    }
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(n.to_string(), 2);
        None
    }
    let mut state = BTreeMap::new();
    for n in graph.keys() {
        if state.get(n.as_str()).copied().unwrap_or(0) == 0 {
            let mut stack = Vec::new();
            if let Some(c) = visit(n, graph, &mut state, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

fn deps<'a>(graph: &'a Graph, module: &str) -> &'a BTreeSet<String> {
    graph.get(module).unwrap_or_else(|| panic!("module `{module}` not found in asphonix-core/src"))
}

#[test]
fn no_grouped_crate_imports() {
    let (_, grouped) = module_graph();
    assert!(grouped.is_empty(), "write one `use crate::module::...` per module so the architecture scan sees it: {grouped:?}");
}

#[test]
fn top_level_modules_are_acyclic() {
    let (graph, _) = module_graph();
    if let Some(cycle) = find_cycle(&graph) {
        panic!("dependency cycle between asphonix-core modules: {}", cycle.join(" -> "));
    }
}

#[test]
fn layering_rules() {
    let (graph, _) = module_graph();
    let only = |module: &str, allowed: &[&str]| {
        let extra: Vec<_> = deps(&graph, module).iter().filter(|d| !allowed.contains(&d.as_str())).collect();
        assert!(extra.is_empty(), "`{module}` may only depend on {allowed:?}, but also uses {extra:?}");
    };
    // Leaves.
    only("config", &[]);
    only("ipc", &["config"]);
    only("prefs", &["config"]);
    only("sequence", &["ipc"]);
    only("redact", &[]);
    // Persistence of settings/credentials stays independent of the features that use them.
    only("secrets", &["config", "ipc"]);
    // The Subsonic client knows nothing about the library, audio or app features.
    only("subsonic", &["config", "ipc", "redact"]);
    // The audio engine never reaches up into app-level features.
    let audio = deps(&graph, "audio");
    for forbidden in ["scrobble", "mcp_bridge", "media", "secrets", "overlay", "updates", "smartlists", "continuity", "extras"] {
        assert!(!audio.contains(forbidden), "`audio` must not depend on `{forbidden}`");
    }
}
