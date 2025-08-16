use std::path::{Path, PathBuf};
use tokio::{fs, io::AsyncWriteExt};
use serde_json;

use crate::Event;
use super::HistoryStore;

const CAP: usize = 1024;

/// Simple filesystem-backed history store writing JSONL per instance.
#[derive(Clone)]
pub struct FsHistoryStore { root: PathBuf }

impl FsHistoryStore {
    /// Create a new store rooted at the given directory path.
    pub fn new(root: impl AsRef<Path>) -> Self { Self { root: root.as_ref().to_path_buf() } }
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
        let path = self.inst_path(instance);
        // Read current to enforce CAP
        let mut existing = self.read(instance).await;
        if existing.len() + new_events.len() > CAP {
            return Err(format!("history cap exceeded (cap={}, have={}, append={})", CAP, existing.len(), new_events.len()));
        }
        existing.extend(new_events.into_iter());
        // Rewrite file with bounded history
        let mut file = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&path).await.unwrap();
        for ev in existing {
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
}


