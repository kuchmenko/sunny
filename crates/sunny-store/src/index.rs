//! Tree-sitter-based Rust symbol extraction and SQLite-backed index.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::StoreError;

/// Kind of code symbol extracted from a Rust source file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Const,
    Static,
    TypeAlias,
    Macro,
    Module,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Const => "const",
            SymbolKind::Static => "static",
            SymbolKind::TypeAlias => "type_alias",
            SymbolKind::Macro => "macro",
            SymbolKind::Module => "module",
        }
    }

    pub fn from_kind_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolKind::Function),
            "struct" => Some(SymbolKind::Struct),
            "enum" => Some(SymbolKind::Enum),
            "trait" => Some(SymbolKind::Trait),
            "impl" => Some(SymbolKind::Impl),
            "const" => Some(SymbolKind::Const),
            "static" => Some(SymbolKind::Static),
            "type_alias" => Some(SymbolKind::TypeAlias),
            "macro" => Some(SymbolKind::Macro),
            "module" => Some(SymbolKind::Module),
            _ => None,
        }
    }
}

/// A code symbol extracted from a Rust source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub file_path: String,
    pub line: u32,
    pub end_line: u32,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    /// For methods: the name of the enclosing impl type.
    pub parent: Option<String>,
}

/// Extract all symbols from a Rust source string.
pub fn extract_symbols(source: &str, file_path: &str) -> Result<Vec<Symbol>, StoreError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| StoreError::Grammar(format!("failed to load Rust grammar: {e}")))?;

    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut symbols = Vec::new();
    let source_bytes = source.as_bytes();
    walk_node(
        tree.root_node(),
        source_bytes,
        file_path,
        None,
        &mut symbols,
    );
    Ok(symbols)
}

/// Extract all symbols from a Rust source file on disk.
pub fn extract_symbols_from_file(path: &Path) -> Result<Vec<Symbol>, StoreError> {
    let source = std::fs::read_to_string(path)?;
    let file_path = path.to_string_lossy().to_string();
    extract_symbols(&source, &file_path)
}

fn walk_node(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    file_path: &str,
    parent: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let kind = node.kind();

    let symbol_kind = match kind {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "impl_item" => Some(SymbolKind::Impl),
        "const_item" => Some(SymbolKind::Const),
        "static_item" => Some(SymbolKind::Static),
        "type_alias" => Some(SymbolKind::TypeAlias),
        "macro_definition" => Some(SymbolKind::Macro),
        "mod_item" => Some(SymbolKind::Module),
        _ => None,
    };

    if let Some(sym_kind) = symbol_kind {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("<unnamed>")
            .to_string();

        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;

        // For impl blocks, extract the impl target type to use as parent for methods.
        let impl_type_name: Option<String> = if sym_kind == SymbolKind::Impl {
            node.child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .map(String::from)
        } else {
            None
        };

        let signature = if sym_kind == SymbolKind::Function {
            node.utf8_text(source).ok().and_then(|text| {
                let sig_end = text
                    .find('{')
                    .or_else(|| text.find(';'))
                    .unwrap_or(text.len());
                let normalized = text[..sig_end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if normalized.is_empty() {
                    None
                } else {
                    Some(normalized.chars().take(200).collect::<String>())
                }
            })
        } else {
            None
        };

        symbols.push(Symbol {
            name: name.clone(),
            file_path: file_path.to_string(),
            line: start_line,
            end_line,
            kind: sym_kind,
            signature,
            parent: parent.map(String::from),
        });

        // For impl blocks, walk children with the impl target type as parent.
        if sym_kind == SymbolKind::Impl {
            let child_parent = impl_type_name.as_deref().unwrap_or(&name);
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    walk_node(child, source, file_path, Some(child_parent), symbols);
                }
            }
            return;
        }
    }

    // Recurse into children.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            walk_node(child, source, file_path, parent, symbols);
        }
    }
}

// ─── SymbolIndex: SQLite-backed storage and search ────────────────────────────

/// Statistics about the symbol index.
#[derive(Debug)]
pub struct IndexStats {
    pub total_symbols: u32,
    pub files_indexed: u32,
}

/// SQLite-backed index for codebase symbol search.
///
/// Uses tree-sitter for extraction and SQLite for persistent storage
/// with case-insensitive name search.
pub struct SymbolIndex {
    db: crate::Database,
}

impl SymbolIndex {
    pub fn new(db: crate::Database) -> Self {
        Self { db }
    }

    /// Index a single file: extract symbols and persist in DB.
    /// Returns the number of symbols stored.
    pub fn index_file(&self, file_path: &Path) -> Result<usize, StoreError> {
        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.to_path_buf());
        let path_str = canonical.to_string_lossy().to_string();
        let content = std::fs::read_to_string(&canonical)?;
        let hash = content_hash(&content);
        self.clear_file(&path_str)?;
        let symbols = extract_symbols(&content, &path_str)?;
        let conn = self.db.connection();
        for symbol in &symbols {
            conn.execute(
                "INSERT INTO symbols \
                 (name, file_path, line, end_line, kind, signature, parent, content_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    symbol.name,
                    symbol.file_path,
                    symbol.line as i64,
                    symbol.end_line as i64,
                    symbol.kind.as_str(),
                    symbol.signature,
                    symbol.parent,
                    hash,
                ],
            )?;
        }
        Ok(symbols.len())
    }

    /// Walk a directory respecting .gitignore and index all `.rs` files.
    pub fn index_directory(&self, root: &Path) -> Result<usize, StoreError> {
        let mut total = 0;
        for result in ignore::WalkBuilder::new(root).build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                && entry.path().extension().and_then(|e| e.to_str()) == Some("rs")
            {
                match self.index_file(entry.path()) {
                    Ok(n) => total += n,
                    Err(e) => tracing::warn!(
                        path = %entry.path().display(),
                        error = %e,
                        "failed to index file"
                    ),
                }
            }
        }
        Ok(total)
    }

    /// Search symbols by name substring (case-insensitive).
    pub fn search(&self, query: &str) -> Result<Vec<Symbol>, StoreError> {
        let conn = self.db.connection();
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT name, file_path, line, end_line, kind, signature, parent \
             FROM symbols WHERE name LIKE ?1 COLLATE NOCASE",
        )?;
        let rows: Result<Vec<_>, rusqlite::Error> = stmt
            .query_map(rusqlite::params![pattern], row_to_symbol)?
            .collect();
        rows.map(|items| items.into_iter().flatten().collect())
            .map_err(StoreError::Db)
    }

    /// Search symbols by name and kind.
    pub fn search_by_kind(&self, query: &str, kind: SymbolKind) -> Result<Vec<Symbol>, StoreError> {
        let conn = self.db.connection();
        let pattern = format!("%{query}%");
        let kind_str = kind.as_str();
        let mut stmt = conn.prepare(
            "SELECT name, file_path, line, end_line, kind, signature, parent \
             FROM symbols WHERE name LIKE ?1 COLLATE NOCASE AND kind = ?2",
        )?;
        let rows: Result<Vec<_>, rusqlite::Error> = stmt
            .query_map(rusqlite::params![pattern, kind_str], row_to_symbol)?
            .collect();
        rows.map(|items| items.into_iter().flatten().collect())
            .map_err(StoreError::Db)
    }

    /// Returns true if the file has changed since last index (or was never indexed).
    pub fn needs_reindex(&self, file_path: &Path) -> Result<bool, StoreError> {
        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.to_path_buf());
        let path_str = canonical.to_string_lossy().to_string();
        let content = std::fs::read_to_string(&canonical)?;
        let current_hash = content_hash(&content);
        let conn = self.db.connection();
        let mut stmt =
            conn.prepare("SELECT content_hash FROM symbols WHERE file_path = ?1 LIMIT 1")?;
        let stored = stmt.query_row(rusqlite::params![path_str], |r| r.get::<_, String>(0));
        match stored {
            Ok(h) => Ok(h != current_hash),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    /// Delete all symbols for a file (call before re-indexing).
    pub fn clear_file(&self, file_path: &str) -> Result<(), StoreError> {
        self.db.connection().execute(
            "DELETE FROM symbols WHERE file_path = ?1",
            rusqlite::params![file_path],
        )?;
        Ok(())
    }

    /// Return index statistics.
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        let conn = self.db.connection();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let files: i64 =
            conn.query_row("SELECT COUNT(DISTINCT file_path) FROM symbols", [], |r| {
                r.get(0)
            })?;
        Ok(IndexStats {
            total_symbols: total as u32,
            files_indexed: files as u32,
        })
    }
}

fn row_to_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Symbol>> {
    let kind_str: String = row.get(4)?;
    let kind = match SymbolKind::from_kind_str(&kind_str) {
        Some(k) => k,
        None => {
            tracing::warn!(kind = %kind_str, "skipping symbol with unknown kind");
            return Ok(None);
        }
    };
    Ok(Some(Symbol {
        name: row.get(0)?,
        file_path: row.get(1)?,
        line: row.get::<_, i64>(2)? as u32,
        end_line: row.get::<_, i64>(3)? as u32,
        kind,
        signature: row.get(5)?,
        parent: row.get(6)?,
    }))
}

/// Content hash for file change detection (not cryptographic).
///
/// Uses `DefaultHasher`, which is suitable for stable comparisons between
/// indexing runs of the same binary build.
fn content_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── T7: extract_symbols tests ─────────────────────────────────────────────

    #[test]
    fn test_extract_function_symbol() {
        let source = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let symbols = extract_symbols(source, "test.rs").expect("should extract");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "add");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
        assert_eq!(symbols[0].line, 1);
    }

    #[test]
    fn test_extract_struct_symbol() {
        let source = "pub struct Foo { x: i32 }";
        let symbols = extract_symbols(source, "test.rs").expect("should extract");
        assert!(symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == SymbolKind::Struct));
    }

    #[test]
    fn test_extract_rust_symbols() {
        let source = r#"
fn hello() {}
fn world() {}
struct MyStruct { x: i32 }
enum MyEnum { A, B }
trait MyTrait { fn method(&self); }
const MY_CONST: u32 = 42;
"#;
        let symbols = extract_symbols(source, "multi.rs").expect("should extract");
        let functions: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        let structs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Struct)
            .collect();
        let enums: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Enum)
            .collect();
        let traits: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Trait)
            .collect();
        assert!(
            functions.len() >= 2,
            "expected at least 2 functions, got {}",
            functions.len()
        );
        assert_eq!(structs.len(), 1);
        assert_eq!(enums.len(), 1);
        assert_eq!(traits.len(), 1);
    }

    #[test]
    fn test_extract_impl_methods() {
        let source = r#"
struct Foo;
impl Foo {
    fn bar(&self) {}
    fn baz(&self) {}
}
"#;
        let symbols = extract_symbols(source, "impl.rs").expect("should extract");
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function && s.parent.is_some())
            .collect();
        assert_eq!(
            methods.len(),
            2,
            "expected 2 methods, got {}",
            methods.len()
        );
        for method in &methods {
            assert_eq!(
                method.parent.as_deref(),
                Some("Foo"),
                "method {} should have parent Foo",
                method.name
            );
        }
        let names: Vec<_> = methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn test_extract_symbols_from_file() {
        let dir = tempdir().expect("should create temp dir");
        let file_path = dir.path().join("test.rs");
        std::fs::write(
            &file_path,
            "fn greet(name: &str) -> String { name.to_string() }",
        )
        .expect("should write file");
        let symbols = extract_symbols_from_file(&file_path).expect("should extract symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_empty_source_returns_empty() {
        let symbols = extract_symbols("", "empty.rs").expect("should extract");
        assert!(symbols.is_empty());
    }

    // ── T12: SymbolIndex tests ────────────────────────────────────────────────

    fn make_index() -> (SymbolIndex, tempfile::TempDir) {
        let dir = tempdir().expect("should create temp dir");
        let db = crate::Database::open(dir.path().join("test.db").as_path())
            .expect("should open database");
        (SymbolIndex::new(db), dir)
    }

    #[test]
    fn test_index_and_search() {
        let (idx, dir) = make_index();
        let file = dir.path().join("foo.rs");
        std::fs::write(&file, "pub fn process_data(x: i32) -> i32 { x * 2 }").expect("write file");
        let n = idx.index_file(&file).expect("index file");
        assert_eq!(n, 1);
        let results = idx.search("process").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "process_data");
        assert_eq!(results[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_search_by_kind() {
        let (idx, dir) = make_index();
        let file = dir.path().join("bar.rs");
        std::fs::write(&file, "fn foo() {} struct Bar;").expect("write file");
        idx.index_file(&file).expect("index file");
        let fns = idx
            .search_by_kind("foo", SymbolKind::Function)
            .expect("search");
        assert_eq!(fns.len(), 1);
        let structs = idx
            .search_by_kind("bar", SymbolKind::Struct)
            .expect("search");
        assert_eq!(structs.len(), 1);
    }

    #[test]
    fn test_index_directory() {
        let (idx, dir) = make_index();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").expect("write a");
        std::fs::write(dir.path().join("b.rs"), "struct B;").expect("write b");
        let total = idx.index_directory(dir.path()).expect("index dir");
        assert!(total >= 2, "expected at least 2 symbols, got {total}");
    }

    #[test]
    fn test_incremental_reindex() {
        let (idx, dir) = make_index();
        let file = dir.path().join("c.rs");
        std::fs::write(&file, "fn original() {}").expect("write file");
        idx.index_file(&file).expect("index original");
        assert!(
            !idx.needs_reindex(&file).expect("check"),
            "should not need reindex after fresh index"
        );
        std::fs::write(&file, "fn modified() {}").expect("modify file");
        assert!(
            idx.needs_reindex(&file).expect("check"),
            "should need reindex after modification"
        );
        idx.index_file(&file).expect("re-index");
        assert!(
            !idx.needs_reindex(&file).expect("check"),
            "should not need reindex after re-index"
        );
    }

    #[test]
    fn test_clear_file_removes_symbols() {
        let (idx, dir) = make_index();
        let file = dir.path().join("d.rs");
        std::fs::write(&file, "fn clear_me() {}").expect("write file");
        idx.index_file(&file).expect("index");
        assert_eq!(idx.search("clear_me").expect("search").len(), 1);
        let path_str = file.canonicalize().unwrap().to_string_lossy().to_string();
        idx.clear_file(&path_str).expect("clear");
        assert_eq!(idx.search("clear_me").expect("search after clear").len(), 0);
    }

    #[test]
    fn test_stats_after_indexing() {
        let (idx, dir) = make_index();
        std::fs::write(dir.path().join("e.rs"), "fn e1() {} fn e2() {}").expect("write");
        idx.index_directory(dir.path()).expect("index");
        let stats = idx.stats().expect("stats");
        assert!(stats.total_symbols >= 2);
        assert!(stats.files_indexed >= 1);
    }

    #[test]
    fn test_search_case_insensitive() {
        let (idx, dir) = make_index();
        let file = dir.path().join("f.rs");
        std::fs::write(&file, "fn MyHandler() {}").expect("write file");
        idx.index_file(&file).expect("index");
        let results = idx.search("myhandler").expect("search");
        assert_eq!(
            results.len(),
            1,
            "case-insensitive search should find MyHandler"
        );
    }

    #[test]
    fn test_needs_reindex_new_file() {
        let (idx, dir) = make_index();
        let file = dir.path().join("new.rs");
        std::fs::write(&file, "fn new_fn() {}").expect("write");
        assert!(
            idx.needs_reindex(&file).expect("check"),
            "new file should need reindex"
        );
    }
}
