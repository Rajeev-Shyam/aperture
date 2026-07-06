//! VS Code workspace-MRU path resolution — the Q56 spike decision (M4).
//!
//! VS Code's default `window.title` shows only the *filename* and the
//! *workspace name*. To resolve an absolute path without guessing, this module:
//!   1. reads the recently-opened folder list from VS Code's own
//!      `state.vscdb` (`%APPDATA%\Code\User\globalStorage\state.vscdb`, key
//!      `history.recentlyOpenedPathsList` in `ItemTable`) — a read-only SQLite
//!      open of a file VS Code updates in place [VERIFY concurrent-read
//!      behavior on-target; a lock failure just resolves nothing];
//!   2. matches the parsed workspace name against a recent folder's basename;
//!   3. walks that folder (bounded depth/breadth, junk dirs skipped) for the
//!      filename — accepting only a **unique** match (ambiguity ⇒ `None`;
//!      doc 10 §4's never-guess rule applies to the IDE ladder too).
//!
//! Results are cached per `(workspace, filename)` for the process lifetime —
//! focus events repeat far faster than workspaces change shape.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directories that are never worth walking for a user-facing source file.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    ".cache",
    "bin",
    "obj",
];

/// Walk bounds — keep worst-case latency far under the capture budget even on
/// a monorepo (the walk runs on a blocking thread in the pipeline).
const MAX_DIRS: usize = 4096;
const MAX_DEPTH: usize = 8;

#[derive(Default)]
pub(crate) struct VsCodeMru {
    /// Override for tests/spikes; `None` ⇒ the default per-user location.
    state_db: Option<PathBuf>,
    /// `(workspace, filename)` → resolved path (or a cached miss).
    cache: Mutex<HashMap<(String, String), Option<String>>>,
}

impl VsCodeMru {
    pub(crate) fn with_state_db(state_db: PathBuf) -> Self {
        Self {
            state_db: Some(state_db),
            ..Self::default()
        }
    }

    /// Resolve `filename` inside the recent workspace named `workspace`.
    /// `None` when the workspace is unknown, the file is absent, or the match
    /// is ambiguous — never a guess.
    pub(crate) fn resolve(&self, workspace: &str, filename: &str) -> Option<String> {
        if workspace.is_empty() || filename.is_empty() {
            return None;
        }
        let key = (workspace.to_string(), filename.to_string());
        if let Some(cached) = self.cache.lock().ok()?.get(&key) {
            return cached.clone();
        }
        let resolved = self
            .workspace_root(workspace)
            .and_then(|root| find_unique(&root, filename))
            .map(|p| p.display().to_string());
        self.cache
            .lock()
            .ok()?
            .insert(key, resolved.clone());
        resolved
    }

    fn state_db_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.state_db {
            return Some(p.clone());
        }
        let appdata = std::env::var_os("APPDATA")?;
        Some(
            Path::new(&appdata)
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        )
    }

    /// Read the recent-folder list and match `workspace` against basenames
    /// (case-insensitive). First match wins — the list is most-recent-first.
    fn workspace_root(&self, workspace: &str) -> Option<PathBuf> {
        let db_path = self.state_db_path()?;
        if !db_path.is_file() {
            return None;
        }
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        let raw: Vec<u8> = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'history.recentlyOpenedPathsList'",
                [],
                |row| row.get(0),
            )
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&raw).ok()?;
        let entries = json.get("entries")?.as_array()?;
        for entry in entries {
            // Folder workspaces only; `.code-workspace` files are a v1 non-goal
            // [ASSUMPTION — multi-root workspaces resolve nothing, never wrongly].
            let Some(folder_uri) = entry.get("folderUri").and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(url) = url::Url::parse(folder_uri) else {
                continue;
            };
            let Ok(path) = url.to_file_path() else {
                continue;
            };
            let matches = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(workspace));
            if matches && path.is_dir() {
                return Some(path);
            }
        }
        None
    }
}

/// Bounded BFS for exactly one file named `filename` under `root`.
/// Two hits ⇒ ambiguous ⇒ `None` (early exit).
fn find_unique(root: &Path, filename: &str) -> Option<PathBuf> {
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    let mut hit: Option<PathBuf> = None;
    while let Some((dir, depth)) = queue.pop() {
        visited += 1;
        if visited > MAX_DIRS {
            // Bound exceeded: treat as unresolved rather than half-searched-and-
            // wrong. (No silent cap on correctness — only on effort.)
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth + 1 <= MAX_DEPTH {
                    let skip = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| SKIP_DIRS.contains(&n.to_ascii_lowercase().as_str()));
                    if !skip {
                        queue.push((path, depth + 1));
                    }
                }
            } else if kind.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(filename))
            {
                if hit.is_some() {
                    return None; // ambiguous — never guess
                }
                hit = Some(path);
            }
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture `state.vscdb` whose recent list points at `workspace_dir`.
    fn fixture_state_db(dir: &Path, workspace_dir: &Path) -> PathBuf {
        let db_path = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value BLOB)")
            .unwrap();
        let uri = url::Url::from_file_path(workspace_dir).unwrap().to_string();
        let json = serde_json::json!({ "entries": [ { "folderUri": uri } ] }).to_string();
        conn.execute(
            "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('history.recentlyOpenedPathsList', ?1)",
            [json.as_bytes()],
        )
        .unwrap();
        db_path
    }

    #[test]
    fn resolves_a_unique_filename_in_a_recent_workspace() {
        let base = std::env::temp_dir().join("aperture-mru-test-unique");
        let ws = base.join("myproject");
        let src = ws.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("main.rs");
        std::fs::write(&file, b"fn main() {}").unwrap();
        let db = fixture_state_db(&base, &ws);

        let mru = VsCodeMru::with_state_db(db);
        assert_eq!(
            mru.resolve("myproject", "main.rs"),
            Some(file.display().to_string())
        );
        // Case-insensitive workspace match.
        assert_eq!(
            mru.resolve("MyProject", "main.rs"),
            Some(file.display().to_string())
        );
    }

    #[test]
    fn ambiguous_or_missing_matches_resolve_nothing() {
        let base = std::env::temp_dir().join("aperture-mru-test-ambig");
        let ws = base.join("proj");
        std::fs::create_dir_all(ws.join("a")).unwrap();
        std::fs::create_dir_all(ws.join("b")).unwrap();
        std::fs::write(ws.join("a").join("mod.rs"), b"").unwrap();
        std::fs::write(ws.join("b").join("mod.rs"), b"").unwrap();
        let db = fixture_state_db(&base, &ws);

        let mru = VsCodeMru::with_state_db(db);
        // Two mod.rs files ⇒ ambiguous ⇒ never guess.
        assert_eq!(mru.resolve("proj", "mod.rs"), None);
        assert_eq!(mru.resolve("proj", "not-here.rs"), None);
        assert_eq!(mru.resolve("unknown-ws", "mod.rs"), None);
    }

    #[test]
    fn junk_dirs_are_skipped() {
        let base = std::env::temp_dir().join("aperture-mru-test-junk");
        let ws = base.join("app");
        std::fs::create_dir_all(ws.join("node_modules").join("dep")).unwrap();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("node_modules").join("dep").join("index.js"), b"").unwrap();
        std::fs::write(ws.join("src").join("index.js"), b"").unwrap();
        let db = fixture_state_db(&base, &ws);

        let mru = VsCodeMru::with_state_db(db);
        // The node_modules copy is invisible; the src one is unique.
        assert_eq!(
            mru.resolve("app", "index.js"),
            Some(ws.join("src").join("index.js").display().to_string())
        );
    }
}
