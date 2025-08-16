use std::path::{Path, PathBuf};
use tokio::{fs, io::AsyncWriteExt};
use serde_json;

use crate::Event;
use super::HistoryStore;

const CAP: usize = 1024;

#[derive(Clone)]
pub struct FsHistoryStore { root: PathBuf }

impl FsHistoryStore {
    pub fn new(root: impl AsRef<Path>) -> Self { Self { root: root.as_ref().to_path_buf() } }
    fn inst_path(&self, instance: &str) -> PathBuf { self.root.join(format!("{}.jsonl", instance)) }
}

#[async_trait::async_trait]
impl HistoryStore for FsHistoryStore {
    async fn read(&self, instance: &str) -> Vec<Event> {
        let path = self.inst_path(instance);
        let data = fs::read_to_string(&path).await.unwrap_or_default();
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() { continue; }
            match serde_json::from_str::<Event>(line) { Ok(ev) => out.push(ev), Err(_) => {} }
        }
        out
    }

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

    async fn reset(&self) {
        let _ = fs::remove_dir_all(&self.root).await;
    }

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

    async fn dump_all_pretty(&self) -> String {
        let mut out = String::new();
        for inst in self.list_instances().await {
            out.push_str(&format!("instance={}\n", inst));
            for ev in self.read(&inst).await { out.push_str(&format!("  {ev:#?}\n")); }
        }
        out
    }
}


