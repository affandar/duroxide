use std::path::{Path, PathBuf};
use tokio::{fs, io::AsyncWriteExt};
use serde_json;

use crate::Event;
use super::{HistoryStore, WorkItem};

const CAP: usize = 1024;

/// Simple filesystem-backed history store writing JSONL per instance.
#[derive(Clone)]
pub struct FsHistoryStore { root: PathBuf, queue_file: PathBuf }

impl FsHistoryStore {
    /// Create a new store rooted at the given directory path.
    /// If `reset_on_create` is true, delete any existing data under the root first.
    pub fn new(root: impl AsRef<Path>, reset_on_create: bool) -> Self {
        let path = root.as_ref().to_path_buf();
        if reset_on_create {
            let _ = std::fs::remove_dir_all(&path);
        }
        let queue_file = path.join("work-queue.jsonl");
        // best-effort create
        let _ = std::fs::create_dir_all(&path);
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&queue_file);
        Self { root: path, queue_file }
    }
    fn inst_path(&self, instance: &str) -> PathBuf { self.root.join(format!("{instance}.jsonl")) }
}

#[async_trait::async_trait]
impl HistoryStore for FsHistoryStore {
    /// Read the entire JSONL file for the instance and deserialize each line.
    async fn read(&self, instance: &str) -> Vec<Event> {
        let path = self.inst_path(instance);
        let data = fs::read_to_string(&path).await.unwrap_or_default();
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() { continue; }
            if let Ok(ev) = serde_json::from_str::<Event>(line) { out.push(ev) }
        }
        out
    }

    /// Append events with a simple capacity guard by rewriting the file.
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String> {
        fs::create_dir_all(&self.root).await.ok();
        // Read current to enforce CAP
        let existing = self.read(instance).await;
        // If the instance file does not exist, treat as error (must call create_instance first)
        let path = self.inst_path(instance);
        if !fs::try_exists(&path).await.map_err(|e| e.to_string())? {
            return Err(format!("instance not found: {instance}"));
        }
        if existing.len() + new_events.len() > CAP {
            return Err(format!("history cap exceeded (cap={}, have={}, append={})", CAP, existing.len(), new_events.len()));
        }
        // Append only new events without truncation
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&path).await.unwrap();
        for ev in new_events {
            let line = serde_json::to_string(&ev).unwrap();
            file.write_all(line.as_bytes()).await.unwrap();
            file.write_all(b"\n").await.unwrap();
        }
        file.flush().await.ok();
        Ok(())
    }

    /// Remove the root directory and all contents.
    async fn reset(&self) {
        let _ = fs::remove_dir_all(&self.root).await;
    }

    /// List instances by scanning filenames with `.jsonl` suffix.
    async fn list_instances(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut rd) = fs::read_dir(&self.root).await {
            while let Ok(Some(ent)) = rd.next_entry().await {
                if let Some(name) = ent.file_name().to_str() {
                    if let Some(stem) = name.strip_suffix(".jsonl") { out.push(stem.to_string()); }
                }
            }
        }
        out
    }

    /// Produce a human-readable dump of all stored histories.
    async fn dump_all_pretty(&self) -> String {
        let mut out = String::new();
        for inst in self.list_instances().await {
            out.push_str(&format!("instance={inst}\n"));
            for ev in self.read(&inst).await { out.push_str(&format!("  {ev:#?}\n")); }
        }
        out
    }

    async fn create_instance(&self, instance: &str) -> Result<(), String> {
        fs::create_dir_all(&self.root).await.map_err(|e| e.to_string())?;
        let path = self.inst_path(instance);
        if fs::try_exists(&path).await.map_err(|e| e.to_string())? {
            return Err(format!("instance already exists: {instance}"));
        }
        let _ = fs::OpenOptions::new().create_new(true).write(true).open(&path).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn remove_instance(&self, instance: &str) -> Result<(), String> {
        let path = self.inst_path(instance);
        if !fs::try_exists(&path).await.map_err(|e| e.to_string())? {
            return Err(format!("instance not found: {instance}"));
        }
        fs::remove_file(&path).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn enqueue_work(&self, item: WorkItem) -> Result<(), String> {
        // sync file writes are fine here
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.queue_file).map_err(|e| e.to_string())?;
        let line = serde_json::to_string(&item).map_err(|e| e.to_string())?;
        use std::io::Write as _;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        f.write_all(b"\n").map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn dequeue_work(&self) -> Option<WorkItem> {
        // naive: read all, pop first, rewrite rest
        let content = std::fs::read_to_string(&self.queue_file).ok()?;
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.is_empty() { return None; }
        let first = items.remove(0);
        let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(&self.queue_file).ok()?;
        for it in items {
            let line = serde_json::to_string(&it).ok()?;
            use std::io::Write as _;
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
        Some(first)
    }

    async fn set_instance_orchestration(&self, instance: &str, orchestration: &str) -> Result<(), String> {
        let meta_path = self.root.join(format!("{instance}.meta"));
        std::fs::write(meta_path, orchestration).map_err(|e| e.to_string())
    }

    async fn get_instance_orchestration(&self, instance: &str) -> Option<String> {
        let meta_path = self.root.join(format!("{instance}.meta"));
        std::fs::read_to_string(meta_path).ok()
    }
}


