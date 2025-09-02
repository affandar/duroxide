use serde_json;
use std::path::{Path, PathBuf};
use tokio::{fs, io::AsyncWriteExt};

use super::{HistoryStore, WorkItem};
use crate::Event;

/// Simple filesystem-backed history store writing JSONL per instance.
#[derive(Clone)]
pub struct FsHistoryStore {
    root: PathBuf,
    orch_queue_file: PathBuf,
    work_queue_file: PathBuf,
    timer_queue_file: PathBuf,
    cap: usize,
}

impl FsHistoryStore {
    /// Create a new store rooted at the given directory path.
    /// If `reset_on_create` is true, delete any existing data under the root first.
    pub fn new(root: impl AsRef<Path>, reset_on_create: bool) -> Self {
        let path = root.as_ref().to_path_buf();
        if reset_on_create {
            let _ = std::fs::remove_dir_all(&path);
        }
        let orch_q = path.join("orch-queue.jsonl");
        let work_q = path.join("work-queue.jsonl");
        let timer_q = path.join("timer-queue.jsonl");
        // best-effort create
        let _ = std::fs::create_dir_all(&path);
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&orch_q);
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&work_q);
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&timer_q);
        Self {
            root: path,
            orch_queue_file: orch_q,
            work_queue_file: work_q,
            timer_queue_file: timer_q,
            cap: 1024,
        }
    }
    /// Create a new store with a custom history cap (useful for tests).
    pub fn new_with_cap(root: impl AsRef<Path>, reset_on_create: bool, cap: usize) -> Self {
        let mut s = Self::new(root, reset_on_create);
        s.cap = cap;
        s
    }
    fn inst_root(&self, instance: &str) -> PathBuf {
        self.root.join(instance)
    }
    fn exec_path(&self, instance: &str, execution_id: u64) -> PathBuf {
        self.inst_root(instance).join(format!("{}.jsonl", execution_id))
    }
    fn orch_lock_dir(&self) -> PathBuf {
        self.root.join(".locks/orch")
    }
    fn work_lock_dir(&self) -> PathBuf {
        self.root.join(".locks/work")
    }
    fn timer_lock_dir(&self) -> PathBuf {
        self.root.join(".locks/timer")
    }
    fn orch_lock_path(&self, token: &str) -> PathBuf {
        self.orch_lock_dir().join(format!("{token}.lock"))
    }
    fn work_lock_path(&self, token: &str) -> PathBuf {
        self.work_lock_dir().join(format!("{token}.lock"))
    }
    fn timer_lock_path(&self, token: &str) -> PathBuf {
        self.timer_lock_dir().join(format!("{token}.lock"))
    }
}

#[async_trait::async_trait]
impl HistoryStore for FsHistoryStore {
    /// Read the entire JSONL file for the instance and deserialize each line.
    async fn read(&self, instance: &str) -> Vec<Event> {
        let latest = self.latest_execution_id(instance).await.unwrap_or(1);
        let path = self.exec_path(instance, latest);
        let data = fs::read_to_string(&path).await.unwrap_or_default();
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Event>(line) {
                out.push(ev)
            }
        }
        out
    }

    /// Append events with a simple capacity guard by rewriting the file.
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String> {
        fs::create_dir_all(&self.root).await.ok();
        // Read current latest to enforce CAP
        let latest = self.latest_execution_id(instance).await.unwrap_or(1);
        let existing = self.read_with_execution(instance, latest).await;
        // If the exec file does not exist, treat as error (must call create_instance first)
        let path = self.exec_path(instance, latest);
        if !fs::try_exists(&path).await.map_err(|e| e.to_string())? {
            return Err(format!("instance not found: {instance}"));
        }
        if existing.len() + new_events.len() > self.cap {
            return Err(format!(
                "history cap exceeded (cap={}, have={}, append={})",
                self.cap,
                existing.len(),
                new_events.len()
            ));
        }
        // Build a seen set for idempotent completion-like events
        use std::collections::HashSet;
        let mut seen: HashSet<(u64, &'static str)> = HashSet::new();
        for ev in existing.iter() {
            match ev {
                Event::ActivityCompleted { id, .. } => {
                    seen.insert((*id, "ac"));
                }
                Event::ActivityFailed { id, .. } => {
                    seen.insert((*id, "af"));
                }
                Event::TimerFired { id, .. } => {
                    seen.insert((*id, "tf"));
                }
                Event::ExternalEvent { id, .. } => {
                    seen.insert((*id, "xe"));
                }
                Event::SubOrchestrationCompleted { id, .. } => {
                    seen.insert((*id, "sc"));
                }
                Event::SubOrchestrationFailed { id, .. } => {
                    seen.insert((*id, "sf"));
                }
                // Use synthetic id=0 slots to dedupe terminal events
                Event::OrchestrationCompleted { .. } => {
                    seen.insert((0, "oc"));
                }
                Event::OrchestrationFailed { .. } => {
                    seen.insert((0, "of"));
                }
                _ => {}
            }
        }
        // Append only not-yet-seen completion-like events; always append schedule-like ones
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .unwrap();
        for ev in new_events {
            let dup = match &ev {
                Event::ActivityCompleted { id, .. } => seen.contains(&(*id, "ac")),
                Event::ActivityFailed { id, .. } => seen.contains(&(*id, "af")),
                Event::TimerFired { id, .. } => seen.contains(&(*id, "tf")),
                Event::ExternalEvent { id, .. } => seen.contains(&(*id, "xe")),
                Event::SubOrchestrationCompleted { id, .. } => seen.contains(&(*id, "sc")),
                Event::SubOrchestrationFailed { id, .. } => seen.contains(&(*id, "sf")),
                Event::OrchestrationCompleted { .. } => seen.contains(&(0, "oc")),
                Event::OrchestrationFailed { .. } => seen.contains(&(0, "of")),
                _ => false,
            };
            if dup {
                continue;
            }
            let line = serde_json::to_string(&ev).unwrap();
            file.write_all(line.as_bytes()).await.unwrap();
            file.write_all(b"\n").await.unwrap();
            match &ev {
                Event::ActivityCompleted { id, .. } => {
                    seen.insert((*id, "ac"));
                }
                Event::ActivityFailed { id, .. } => {
                    seen.insert((*id, "af"));
                }
                Event::TimerFired { id, .. } => {
                    seen.insert((*id, "tf"));
                }
                Event::ExternalEvent { id, .. } => {
                    seen.insert((*id, "xe"));
                }
                Event::SubOrchestrationCompleted { id, .. } => {
                    seen.insert((*id, "sc"));
                }
                Event::SubOrchestrationFailed { id, .. } => {
                    seen.insert((*id, "sf"));
                }
                Event::OrchestrationCompleted { .. } => {
                    seen.insert((0, "oc"));
                }
                Event::OrchestrationFailed { .. } => {
                    seen.insert((0, "of"));
                }
                _ => {}
            }
        }
        file.flush().await.ok();
        Ok(())
    }

    /// Remove the root directory and all contents.
    async fn reset(&self) {
        let _ = fs::remove_dir_all(&self.root).await;
    }

    /// List instances by scanning instance directories (multi-execution) and legacy `.jsonl` files.
    async fn list_instances(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut rd) = fs::read_dir(&self.root).await {
            while let Ok(Some(ent)) = rd.next_entry().await {
                let path = ent.path();
                if let Some(name) = ent.file_name().to_str() {
                    if path.is_dir() {
                        out.push(name.to_string());
                    } else if let Some(stem) = name.strip_suffix(".jsonl") {
                        out.push(stem.to_string());
                    }
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
            if let Some(lat) = self.latest_execution_id(&inst).await {
                for eid in 1..=lat {
                    for ev in self.read_with_execution(&inst, eid).await {
                        out.push_str(&format!("  exec#{eid} {ev:#?}\n"));
                    }
                }
            }
        }
        out
    }

    async fn create_instance(&self, instance: &str) -> Result<(), String> {
        fs::create_dir_all(&self.root).await.map_err(|e| e.to_string())?;
        let inst_dir = self.inst_root(instance);
        if fs::try_exists(&inst_dir).await.map_err(|e| e.to_string())? {
            return Err(format!("instance already exists: {instance}"));
        }
        fs::create_dir_all(&inst_dir).await.map_err(|e| e.to_string())?;
        let _ = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.exec_path(instance, 1))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn remove_instance(&self, instance: &str) -> Result<(), String> {
        let inst_dir = self.inst_root(instance);
        if !fs::try_exists(&inst_dir).await.map_err(|e| e.to_string())? {
            return Err(format!("instance not found: {instance}"));
        }
        fs::remove_dir_all(&inst_dir).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    // ===== Orchestrator Queue Methods =====
    
    async fn enqueue_orchestrator_work(&self, item: WorkItem) -> Result<(), String> {
        // Idempotent enqueue: load current items and only append if not present
        let qf = &self.orch_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.contains(&item) {
            return Ok(());
        }
        items.push(item);
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, qf).map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn dequeue_orchestrator_peek_lock(&self) -> Option<(Vec<WorkItem>, String)> {
        // Group items by instance and return all items for the first instance
        let qf = &self.orch_queue_file;
        let content = std::fs::read_to_string(qf).ok()?;
        let all_items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if all_items.is_empty() {
            return None;
        }
        
        // Group by instance
        let mut instance_map = std::collections::HashMap::<String, Vec<WorkItem>>::new();
        for item in all_items.iter() {
            let instance = match item {
                WorkItem::StartOrchestration { instance, .. }
                | WorkItem::ActivityCompleted { instance, .. }
                | WorkItem::ActivityFailed { instance, .. }
                | WorkItem::TimerFired { instance, .. }
                | WorkItem::ExternalRaised { instance, .. }
                | WorkItem::CancelInstance { instance, .. }
                | WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
                WorkItem::SubOrchCompleted { parent_instance, .. }
                | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
                _ => continue,
            };
            instance_map.entry(instance).or_default().push(item.clone());
        }
        
        // Take the first instance's items
        let (target_instance, batch_items) = instance_map.into_iter().next()?;
        
        // Remove batch items from all_items
        let remaining_items: Vec<WorkItem> = all_items.into_iter()
            .filter(|item| {
                let item_instance = match item {
                    WorkItem::StartOrchestration { instance, .. }
                    | WorkItem::ActivityCompleted { instance, .. }
                    | WorkItem::ActivityFailed { instance, .. }
                    | WorkItem::TimerFired { instance, .. }
                    | WorkItem::ExternalRaised { instance, .. }
                    | WorkItem::CancelInstance { instance, .. }
                    | WorkItem::ContinueAsNew { instance, .. } => instance,
                    WorkItem::SubOrchCompleted { parent_instance, .. }
                    | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance,
                    _ => return true,
                };
                item_instance != &target_instance
            })
            .collect();
        
        // Rewrite remaining items atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .ok()?;
            for it in &remaining_items {
                let line = serde_json::to_string(&it).ok()?;
                use std::io::Write as _;
                let _ = tf.write_all(line.as_bytes());
                let _ = tf.write_all(b"\n");
            }
        }
        let _ = std::fs::rename(&tmp, qf);
        
        // Create lock token and persist the batch as JSON array
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let token = format!("{now_ns:x}-{pid:x}");
        let _ = std::fs::create_dir_all(self.orch_lock_dir());
        let lock_path = self.orch_lock_path(&token);
        let batch_json = serde_json::to_string(&batch_items).ok()?;
        let _ = std::fs::write(&lock_path, batch_json);
        Some((batch_items, token))
    }

    async fn ack_orchestrator(&self, token: &str) -> Result<(), String> {
        let path = self.orch_lock_path(token);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn abandon_orchestrator(&self, token: &str) -> Result<(), String> {
        // Read locked batch and re-enqueue at front, then remove lock
        let path = self.orch_lock_path(token);
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        
        // Handle both old single-item format and new batch format for backward compatibility
        let items_to_restore: Vec<WorkItem> = if let Ok(batch) = serde_json::from_str::<Vec<WorkItem>>(&data) {
            // New batch format
            batch
        } else if let Ok(single_item) = serde_json::from_str::<WorkItem>(&data) {
            // Old single-item format
            vec![single_item]
        } else {
            return Err("Invalid lock file format".to_string());
        };
        
        // Prepend to queue
        let qf = &self.orch_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let existing_items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        
        // Prepend the batch items to existing items
        let mut all_items = items_to_restore;
        all_items.extend(existing_items);
        
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &all_items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, &qf).map_err(|e| e.to_string())?;
        // Remove lock
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        Ok(())
    }
    
    // ===== Worker Queue Methods =====
    
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        // Idempotent enqueue: load current items and only append if not present
        let qf = &self.work_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.contains(&item) {
            return Ok(());
        }
        items.push(item);
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, qf).map_err(|e| e.to_string())?;
        Ok(())
    }
    
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        // Pop first item but write it to a lock sidecar to keep invisible until ack/abandon
        let qf = &self.work_queue_file;
        let content = std::fs::read_to_string(qf).ok()?;
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.is_empty() {
            return None;
        }
        let first = items.remove(0);
        // Rewrite remaining items atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .ok()?;
            for it in &items {
                let line = serde_json::to_string(&it).ok()?;
                use std::io::Write as _;
                let _ = tf.write_all(line.as_bytes());
                let _ = tf.write_all(b"\n");
            }
        }
        let _ = std::fs::rename(&tmp, qf);
        // Create lock token and persist the locked item
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let token = format!("{now_ns:x}-{pid:x}");
        let _ = std::fs::create_dir_all(self.work_lock_dir());
        let lock_path = self.work_lock_path(&token);
        let line = serde_json::to_string(&first).ok()?;
        let _ = std::fs::write(&lock_path, line);
        Some((first, token))
    }
    
    async fn ack_worker(&self, token: &str) -> Result<(), String> {
        let path = self.work_lock_path(token);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    
    async fn abandon_worker(&self, token: &str) -> Result<(), String> {
        // Read locked item and re-enqueue at front, then remove lock
        let path = self.work_lock_path(token);
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let item: WorkItem = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        // Prepend to queue
        let qf = &self.work_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        items.insert(0, item);
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, qf).map_err(|e| e.to_string())?;
        // Remove lock
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        Ok(())
    }
    
    // ===== Timer Queue Methods =====
    
    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> {
        // Idempotent enqueue: load current items and only append if not present
        let qf = &self.timer_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.contains(&item) {
            return Ok(());
        }
        items.push(item);
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, qf).map_err(|e| e.to_string())?;
        Ok(())
    }
    
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> {
        // Pop first item but write it to a lock sidecar to keep invisible until ack/abandon
        let qf = &self.timer_queue_file;
        let content = std::fs::read_to_string(qf).ok()?;
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        if items.is_empty() {
            return None;
        }
        let first = items.remove(0);
        // Rewrite remaining items atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .ok()?;
            for it in &items {
                let line = serde_json::to_string(&it).ok()?;
                use std::io::Write as _;
                let _ = tf.write_all(line.as_bytes());
                let _ = tf.write_all(b"\n");
            }
        }
        let _ = std::fs::rename(&tmp, qf);
        // Create lock token and persist the locked item
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let token = format!("{now_ns:x}-{pid:x}");
        let _ = std::fs::create_dir_all(self.timer_lock_dir());
        let lock_path = self.timer_lock_path(&token);
        let line = serde_json::to_string(&first).ok()?;
        let _ = std::fs::write(&lock_path, line);
        Some((first, token))
    }
    
    async fn ack_timer(&self, token: &str) -> Result<(), String> {
        let path = self.timer_lock_path(token);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    
    async fn abandon_timer(&self, token: &str) -> Result<(), String> {
        // Read locked item and re-enqueue at front, then remove lock
        let path = self.timer_lock_path(token);
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let item: WorkItem = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        // Prepend to queue
        let qf = &self.timer_queue_file;
        let content = std::fs::read_to_string(qf).unwrap_or_default();
        let mut items: Vec<WorkItem> = content
            .lines()
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect();
        items.insert(0, item);
        // Rewrite file atomically
        let tmp = qf.with_extension("jsonl.tmp");
        {
            let mut tf = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for it in &items {
                let line = serde_json::to_string(&it).map_err(|e| e.to_string())?;
                use std::io::Write as _;
                tf.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                tf.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, qf).map_err(|e| e.to_string())?;
        // Remove lock
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        Ok(())
    }

    // metadata APIs removed

    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        let inst_dir = self.inst_root(instance);
        let mut max_eid = 0u64;
        if let Ok(mut rd) = fs::read_dir(&inst_dir).await {
            while let Ok(Some(ent)) = rd.next_entry().await {
                if let Some(name) = ent.file_name().to_str() {
                    if let Some(stem) = name.strip_suffix(".jsonl") {
                        if let Ok(id) = stem.parse::<u64>() {
                            max_eid = max_eid.max(id);
                        }
                    }
                }
            }
        }
        if max_eid == 0 { None } else { Some(max_eid) }
    }

    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        match self.latest_execution_id(instance).await {
            Some(lat) => (1..=lat).collect(),
            None => Vec::new(),
        }
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        let path = self.exec_path(instance, execution_id);
        let data = fs::read_to_string(&path).await.unwrap_or_default();
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Event>(line) {
                out.push(ev)
            }
        }
        out
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        fs::create_dir_all(self.inst_root(instance)).await.ok();
        let existing = self.read_with_execution(instance, execution_id).await;
        let path = self.exec_path(instance, execution_id);
        if !fs::try_exists(&path).await.map_err(|e| e.to_string())? {
            return Err(format!("execution not found: {}#{}", instance, execution_id));
        }
        if existing.len() + new_events.len() > self.cap {
            return Err(format!(
                "history cap exceeded (cap={}, have={}, append={})",
                self.cap,
                existing.len(),
                new_events.len()
            ));
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .unwrap();
        for ev in new_events {
            let line = serde_json::to_string(&ev).unwrap();
            file.write_all(line.as_bytes()).await.unwrap();
            file.write_all(b"\n").await.unwrap();
        }
        file.flush().await.ok();
        Ok(())
    }

    async fn create_new_execution(
        &self,
        instance: &str,
        orchestration: &str,
        version: &str,
        input: &str,
        parent_instance: Option<&str>,
        parent_id: Option<u64>,
    ) -> Result<u64, String> {
        let lat = self.latest_execution_id(instance).await.unwrap_or(0) + 1;
        fs::create_dir_all(self.inst_root(instance))
            .await
            .map_err(|e| e.to_string())?;
        let path = self.exec_path(instance, lat);
        let _ = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|e| e.to_string())?;
        self.append_with_execution(
            instance,
            lat,
            vec![Event::OrchestrationStarted {
                name: orchestration.to_string(),
                version: version.to_string(),
                input: input.to_string(),
                parent_instance: parent_instance.map(|s| s.to_string()),
                parent_id,
            }],
        )
        .await?;
        Ok(lat)
    }
}
