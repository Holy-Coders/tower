use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemInfo {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
}

fn lang_for(path: &str) -> Option<&'static str> {
    if path.ends_with(".rs") { Some("rust") }
    else if path.ends_with(".ts") || path.ends_with(".tsx") || path.ends_with(".mts") || path.ends_with(".cts") { Some("typescript") }
    else { None }
}

pub fn extract_items(file: &str, source: &str) -> Vec<ItemInfo> {
    let Some(lang) = lang_for(file) else { return vec![] };
    let mut parser = tree_sitter::Parser::new();
    let set = if lang == "rust" {
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())
    } else {
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
    };
    if set.is_err() { return vec![]; }
    let Some(tree) = parser.parse(source, None) else { return vec![] };
    let root = tree.root_node();
    let mut out = Vec::new();
    if lang == "rust" { walk_rust(root, source, file, &mut out) } else { walk_ts(root, source, file, &mut out) }
    out
}

fn node_text(n: tree_sitter::Node, src: &str) -> String {
    src.get(n.start_byte()..n.end_byte()).unwrap_or("").to_string()
}
fn name_child(n: tree_sitter::Node, src: &str) -> Option<String> {
    n.child_by_field_name("name").map(|c| node_text(c, src).trim().to_string()).filter(|s| !s.is_empty())
}
fn push(out: &mut Vec<ItemInfo>, n: tree_sitter::Node, kind: &str, name: String, file: &str) {
    out.push(ItemInfo { name, kind: kind.to_string(), file: file.to_string(), start_line: n.start_position().row + 1, end_line: n.end_position().row + 1 });
}

fn decl_list(n: tree_sitter::Node) -> Option<tree_sitter::Node> {
    for i in 0..n.child_count() {
        if let Some(ch) = n.child(i) {
            if ch.kind() == "declaration_list" { return Some(ch); }
        }
    }
    None
}

fn walk_rust(root: tree_sitter::Node, src: &str, file: &str, out: &mut Vec<ItemInfo>) {
    walk_rust_block(root, src, file, out);
}

fn walk_rust_block(parent: tree_sitter::Node, src: &str, file: &str, out: &mut Vec<ItemInfo>) {
    let mut cur = parent.walk();
    for child in parent.children(&mut cur) {
        let kind = child.kind();
        match kind {
            "function_item" => if let Some(n) = name_child(child, src) { push(out, child, "function", n, file) },
            "struct_item" => if let Some(n) = name_child(child, src) { push(out, child, "struct", n, file) },
            "enum_item" => if let Some(n) = name_child(child, src) { push(out, child, "enum", n, file) },
            "trait_item" => if let Some(n) = name_child(child, src) { push(out, child, "trait", n, file) },
            // Inline modules are containers, not items: index their contents
            // directly. In particular a `#[cfg(test)] mod tests` yields one
            // item per test function (named by the function) instead of a
            // single collapsed `tests` item (Phase 2.2 Fix B). Bodiless
            // declarations (`mod foo;`) keep a module item.
            "mod_item" => match decl_list(child) {
                Some(body) => walk_rust_block(body, src, file, out),
                None => if let Some(n) = name_child(child, src) { push(out, child, "module", n, file) },
            },
            "type_item" => if let Some(n) = name_child(child, src) { push(out, child, "type", n, file) },
            "const_item" => if let Some(n) = name_child(child, src) { push(out, child, "const", n, file) },
            "static_item" => if let Some(n) = name_child(child, src) { push(out, child, "static", n, file) },
            "macro_definition" => if let Some(n) = name_child(child, src) { push(out, child, "macro", n, file) },
            "impl_item" => {
                let tname = child.child_by_field_name("type").map(|c| node_text(c, src).trim().to_string()).unwrap_or_else(|| "impl".into());
                push(out, child, "impl", tname.clone(), file);
                // methods inside declaration list
                let mut c2 = child.walk();
                for sub in child.children(&mut c2) {
                    if sub.kind() == "declaration_list" {
                        let mut c3 = sub.walk();
                        for m in sub.children(&mut c3) {
                            if m.kind() == "function_item" { if let Some(n) = name_child(m, src) { push(out, m, "method", format!("{tname}::{n}"), file) } }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn ts_node_items(n: tree_sitter::Node, src: &str, file: &str) -> Vec<ItemInfo> {
    let mut out = Vec::new();
    match n.kind() {
        "function_declaration" | "generator_function_declaration" => if let Some(name) = name_child(n, src) { push(&mut out, n, "function", name, file) },
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = name_child(n, src) {
                push(&mut out, n, "class", name.clone(), file);
                let mut c = n.walk();
                for ch in n.children(&mut c) {
                    if ch.kind() == "class_body" {
                        let mut c2 = ch.walk();
                        for m in ch.children(&mut c2) {
                            if m.kind() == "method_definition" || m.kind() == "public_field_definition" {
                                if let Some(mn) = name_child(m, src) { push(&mut out, m, "method", format!("{name}.{mn}"), file) }
                            }
                        }
                    }
                }
            }
        }
        "interface_declaration" => if let Some(name) = name_child(n, src) { push(&mut out, n, "interface", name, file) },
        "type_alias_declaration" => if let Some(name) = name_child(n, src) { push(&mut out, n, "type", name, file) },
        "enum_declaration" => if let Some(name) = name_child(n, src) { push(&mut out, n, "enum", name, file) },
        "lexical_declaration" | "variable_declaration" => {
            let mut c = n.walk();
            for d in n.children(&mut c) {
                if d.kind() == "variable_declarator" {
                    if let Some(name) = name_child(d, src) {
                        let is_fn = d.child_by_field_name("value").map(|v| matches!(v.kind(), "arrow_function" | "function_expression")).unwrap_or(false);
                        push(&mut out, d, if is_fn { "function" } else { "variable" }, name, file);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn walk_ts(root: tree_sitter::Node, src: &str, file: &str, out: &mut Vec<ItemInfo>) {
    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        if child.kind() == "export_statement" {
            let mut c = child.walk();
            for ch in child.children(&mut c) {
                for mut it in ts_node_items(ch, src, file) { it.file = file.to_string(); out.push(it); }
            }
        } else {
            for mut it in ts_node_items(child, src, file) { it.file = file.to_string(); out.push(it); }
        }
    }
}

/// Items changed/added/removed between base source and new source of one file.
pub fn changed_items(file: &str, base_src: Option<&str>, new_src: Option<&str>) -> Vec<String> {
    let base_items = base_src.map(|s| extract_items(file, s)).unwrap_or_default();
    let new_items = new_src.map(|s| extract_items(file, s)).unwrap_or_default();
    let bmap: HashMap<(String, String), String> = base_items.iter().map(|i| ((i.kind.clone(), i.name.clone()), item_text(base_src.unwrap_or(""), i))).collect();
    let nmap: HashMap<(String, String), String> = new_items.iter().map(|i| ((i.kind.clone(), i.name.clone()), item_text(new_src.unwrap_or(""), i))).collect();
    let mut changed = Vec::new();
    for (k, v) in &nmap {
        match bmap.get(k) { Some(bv) if bv == v => {}, _ => changed.push(k.1.clone()) }
    }
    for k in bmap.keys() { if !nmap.contains_key(k) { changed.push(k.1.clone()); } }
    changed.sort(); changed.dedup();
    changed
}

fn item_text(src: &str, item: &ItemInfo) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let s = item.start_line.saturating_sub(1);
    let e = item.end_line.min(lines.len());
    if s >= e { String::new() } else { lines[s..e].join("\n") }
}
