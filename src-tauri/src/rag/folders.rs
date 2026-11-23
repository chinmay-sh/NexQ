//! Folders the user picked for the knowledge base. Their files are indexed in
//! place (never copied), with the file's path as its id, and kept current: a
//! scan compares each file's modified time and size with those last indexed,
//! re-indexes new and changed files, and drops files that are gone.
//!
//! Scans run when a folder is added or removed, on "Rescan", and every minute
//! in the background (`run_rescans`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::config::RagConfig;
use super::embedder::Embedder;
use super::file_processor;
use super::RagManager;
use crate::db::rag as rag_db;
use crate::db::DatabaseManager;

/// `app_state` key holding the folder list (JSON array of paths).
pub const FOLDERS_KEY: &str = "rag_folders";
/// `app_state` key holding the saved RagConfig (JSON).
pub const CONFIG_KEY: &str = "rag_config";

/// Larger files are skipped (logs, dumps, datasets).
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Most files indexed across all folders.
const MAX_FILES: usize = 20_000;
/// Folders never entered (besides hidden ones such as .git).
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", "bin", "obj", "__pycache__", "venv"];
/// Time between background rescans.
// shortcut: polling instead of a file watcher; switch to `notify` if folders get large enough that a stat walk per minute costs too much.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub struct FoundFile {
    pub path: String,
    pub folder: String,
    pub file_type: String,
    pub modified_ms: i64,
    pub size: i64,
}

/// Every indexable file under `folders`.
pub fn scan(folders: &[String]) -> Vec<FoundFile> {
    let mut found = Vec::new();
    for folder in folders {
        let mut stack = vec![std::path::PathBuf::from(folder)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                log::warn!("RAG folders: can't read {}", dir.display());
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let Ok(file_type) = entry.file_type() else { continue };
                if file_type.is_dir() {
                    if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                    continue;
                }
                if !file_type.is_file() || name.starts_with("~$") {
                    continue; // symlinks, and Office lock files
                }
                let (Some(kind), Ok(meta)) = (file_processor::file_type_for(&path), entry.metadata()) else { continue };
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
                if found.len() >= MAX_FILES {
                    log::warn!("RAG folders: more than {} files; the rest are not indexed", MAX_FILES);
                    return found;
                }
                let modified_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                found.push(FoundFile {
                    path: path.to_string_lossy().to_string(),
                    folder: folder.clone(),
                    file_type: kind,
                    modified_ms,
                    size: meta.len() as i64,
                });
            }
        }
    }
    found
}

#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// New or changed files.
    pub index: Vec<FoundFile>,
    /// Indexed files that are gone (or whose folder was removed).
    pub remove: Vec<String>,
}

/// What to index and remove, given the (modified, size) last indexed per path.
pub fn diff(known: &HashMap<String, (i64, i64)>, found: &[FoundFile]) -> Changes {
    let index = found
        .iter()
        .filter(|f| known.get(&f.path) != Some(&(f.modified_ms, f.size)))
        .cloned()
        .collect();
    let present: std::collections::HashSet<&str> = found.iter().map(|f| f.path.as_str()).collect();
    let mut remove: Vec<String> = known.keys().filter(|p| !present.contains(p.as_str())).cloned().collect();
    remove.sort();
    Changes { index, remove }
}

/// A value from the `app_state` key/value table.
pub fn get_state(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM app_state WHERE key = ?1", params![key], |r| r.get::<_, Option<String>>(0))
        .optional()
        .ok()
        .flatten()
        .flatten()
}

pub fn set_state(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| format!("Failed to save {}: {}", key, e))
}

pub fn load_folders(conn: &Connection) -> Vec<String> {
    get_state(conn, FOLDERS_KEY)
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

pub fn save_folders(conn: &Connection, folders: &[String]) -> Result<(), String> {
    set_state(conn, FOLDERS_KEY, &serde_json::to_string(folders).unwrap_or_else(|_| "[]".into()))
}

fn known_files(conn: &Connection) -> Result<HashMap<String, (i64, i64)>, String> {
    let mut stmt = conn
        .prepare("SELECT path, modified_ms, size FROM rag_folder_files")
        .map_err(|e| format!("Failed to read indexed folder files: {}", e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))
        .map_err(|e| format!("Failed to read indexed folder files: {}", e))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("Failed to read indexed folder files: {}", e))
}

fn record_file(conn: &Connection, f: &FoundFile) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO rag_folder_files (path, folder, modified_ms, size) VALUES (?1, ?2, ?3, ?4)",
        params![f.path, f.folder, f.modified_ms, f.size],
    )
    .map(|_| ())
    .map_err(|e| format!("Failed to record {}: {}", f.path, e))
}

fn forget_file(conn: &Connection, path: &str) -> Result<(), String> {
    rag_db::delete_chunks_by_file(conn, path)?;
    conn.execute("DELETE FROM rag_folder_files WHERE path = ?1", params![path])
        .map(|_| ())
        .map_err(|e| format!("Failed to forget {}: {}", path, e))
}

/// Forget every indexed folder file (their chunks go with the index clear).
pub fn clear_files(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM rag_folder_files", [])
        .map(|_| ())
        .map_err(|e| format!("Failed to clear folder files: {}", e))
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderInfo {
    pub path: String,
    pub exists: bool,
    pub files: usize,
    pub chunks: usize,
}

pub fn folder_infos(conn: &Connection) -> Vec<FolderInfo> {
    load_folders(conn)
        .into_iter()
        .map(|path| {
            let files: i64 = conn
                .query_row("SELECT COUNT(*) FROM rag_folder_files WHERE folder = ?1", params![path], |r| r.get(0))
                .unwrap_or(0);
            let chunks: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM rag_chunks WHERE file_id IN (SELECT path FROM rag_folder_files WHERE folder = ?1)",
                    params![path],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            FolderInfo { exists: Path::new(&path).is_dir(), path, files: files as usize, chunks: chunks as usize }
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    /// Another scan was already running; this one did nothing.
    pub skipped: bool,
    pub indexed: usize,
    pub removed: usize,
    pub failed: usize,
    pub unchanged: usize,
}

static SYNCING: AtomicBool = AtomicBool::new(false);

struct SyncGuard;
impl Drop for SyncGuard {
    fn drop(&mut self) {
        SYNCING.store(false, Ordering::SeqCst);
    }
}

fn lock_db(db: &Arc<Mutex<DatabaseManager>>) -> Result<std::sync::MutexGuard<'_, DatabaseManager>, String> {
    db.lock().map_err(|e| format!("DB lock failed: {}", e))
}

/// Bring the index in line with the folders: index new and changed files,
/// drop removed ones. Emits `rag_folders_synced` with the report.
/// `current` gives the RAG settings and embedder to index with (see `rag::current`).
pub async fn sync(
    db: &Arc<Mutex<DatabaseManager>>,
    current: impl Fn() -> Result<(RagConfig, Embedder), String>,
    app: &AppHandle,
) -> Result<SyncReport, String> {
    if SYNCING.swap(true, Ordering::SeqCst) {
        return Ok(SyncReport { skipped: true, ..Default::default() });
    }
    let _guard = SyncGuard;

    let (folders, known) = {
        let guard = lock_db(db)?;
        (load_folders(guard.connection()), known_files(guard.connection())?)
    };
    if folders.is_empty() && known.is_empty() {
        return Ok(SyncReport::default());
    }
    let found = tokio::task::spawn_blocking(move || scan(&folders))
        .await
        .map_err(|e| format!("Folder scan failed: {}", e))?;
    let changes = diff(&known, &found);
    let mut report = SyncReport {
        removed: changes.remove.len(),
        unchanged: found.len() - changes.index.len(),
        ..Default::default()
    };

    {
        let guard = lock_db(db)?;
        for path in &changes.remove {
            if let Err(e) = forget_file(guard.connection(), path) {
                log::warn!("RAG folders: {}", e);
            }
        }
    }

    for file in &changes.index {
        let (path, kind) = (file.path.clone(), file.file_type.clone());
        let text = tokio::task::spawn_blocking(move || file_processor::extract_text(&path, &kind))
            .await
            .map_err(|e| format!("Text extraction failed: {}", e))?;
        rag_db::delete_chunks_by_file(lock_db(db)?.connection(), &file.path)?;
        match text {
            Ok(text) if !text.trim().is_empty() => {
                let (config, embedder) = current()?;
                let name = Path::new(&file.path).file_name().map_or(file.path.clone(), |n| n.to_string_lossy().to_string());
                match RagManager::index_file_async(db, &file.path, &text, &name, app, &config, &embedder).await {
                    Ok(_) => report.indexed += 1,
                    Err(e) => {
                        // The chunks are in, so keyword search still finds the file.
                        log::warn!("RAG folders: {} indexed without embeddings: {}", file.path, e);
                        report.failed += 1;
                    }
                }
            }
            Ok(_) => report.indexed += 1,
            Err(e) => {
                log::warn!("RAG folders: can't read {}: {}", file.path, e);
                report.failed += 1;
            }
        }
        // Recorded even when unreadable, so it isn't retried until it changes.
        record_file(lock_db(db)?.connection(), file)?;
    }

    log::info!(
        "RAG folders synced: {} indexed, {} removed, {} failed, {} unchanged",
        report.indexed, report.removed, report.failed, report.unchanged
    );
    let _ = app.emit("rag_folders_synced", &report);
    Ok(report)
}

/// `sync` with the app's database and RAG manager.
pub async fn sync_app(app: &AppHandle) -> Result<SyncReport, String> {
    let state = app.state::<crate::state::AppState>();
    let Some(db) = state.database.clone() else {
        return Err("Knowledge base not initialized".to_string());
    };
    sync(&db, || super::current(&state), app).await
}

/// Rescan the folders in the background, for as long as the app runs.
pub async fn run_rescans(app: AppHandle) {
    tokio::time::sleep(Duration::from_secs(10)).await;
    loop {
        if let Err(e) = sync_app(&app).await {
            log::warn!("RAG folders: rescan failed: {}", e);
        }
        tokio::time::sleep(RESCAN_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, modified_ms: i64, size: i64) -> FoundFile {
        FoundFile { path: path.into(), folder: "F".into(), file_type: "md".into(), modified_ms, size }
    }

    #[test]
    fn diff_finds_new_changed_and_removed_files() {
        let known: HashMap<String, (i64, i64)> =
            [("a.md", (1, 10)), ("b.md", (1, 10)), ("gone.md", (1, 10))].into_iter().map(|(p, v)| (p.to_string(), v)).collect();
        let found = vec![file("a.md", 1, 10), file("b.md", 2, 10), file("new.md", 1, 1)];
        let changes = diff(&known, &found);
        assert_eq!(changes.index, vec![file("b.md", 2, 10), file("new.md", 1, 1)]);
        assert_eq!(changes.remove, vec!["gone.md".to_string()]);
    }

    #[test]
    fn scan_skips_hidden_and_build_folders_and_unknown_types() {
        let root = std::env::temp_dir().join(format!("nexq-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["docs/sub", ".git", "node_modules/pkg"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (path, body) in [
            ("docs/plan.md", "plan"),
            ("docs/sub/main.rs", "fn main() {}"),
            ("docs/photo.png", "png"),
            ("docs/~$plan.docx", "lock"),
            (".git/config.toml", "x"),
            ("node_modules/pkg/index.js", "x"),
        ] {
            std::fs::write(root.join(path), body).unwrap();
        }
        let folder = root.to_string_lossy().to_string();
        let mut names: Vec<String> = scan(&[folder.clone()])
            .into_iter()
            .map(|f| {
                assert_eq!(f.folder, folder);
                Path::new(&f.path).strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/")
            })
            .collect();
        names.sort();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(names, ["docs/plan.md", "docs/sub/main.rs"]);
    }

    #[test]
    fn state_and_folder_files_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_state (key TEXT PRIMARY KEY NOT NULL, value TEXT);
             CREATE TABLE rag_folder_files (path TEXT PRIMARY KEY NOT NULL, folder TEXT NOT NULL, modified_ms INTEGER NOT NULL, size INTEGER NOT NULL);
             CREATE TABLE rag_chunks (chunk_id TEXT, file_id TEXT);
             CREATE TABLE rag_embeddings (chunk_id TEXT, embedding BLOB);",
        )
        .unwrap();
        assert!(load_folders(&conn).is_empty());
        save_folders(&conn, &["C:/a".into(), "C:/b".into()]).unwrap();
        save_folders(&conn, &["C:/b".into()]).unwrap();
        assert_eq!(load_folders(&conn), ["C:/b"]);

        let f = FoundFile { folder: "C:/b".into(), ..file("C:/b/x.md", 5, 7) };
        record_file(&conn, &f).unwrap();
        conn.execute("INSERT INTO rag_chunks VALUES ('c1', 'C:/b/x.md')", []).unwrap();
        assert_eq!(known_files(&conn).unwrap().get("C:/b/x.md"), Some(&(5, 7)));
        let info = folder_infos(&conn);
        assert_eq!((info[0].files, info[0].chunks), (1, 1));
        forget_file(&conn, "C:/b/x.md").unwrap();
        assert!(known_files(&conn).unwrap().is_empty());
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
