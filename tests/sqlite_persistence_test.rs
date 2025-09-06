use duroxide::providers::{HistoryStore, WorkItem};
use duroxide::providers::sqlite::SqliteHistoryStore;
use std::sync::Arc;

#[tokio::test]
async fn test_sqlite_basic_persistence() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test.db");
    
    // Pre-create the database file
    std::fs::File::create(&db_path).expect("Failed to create db file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Phase 1: Create store and add data
    {
        let store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store");
        let store: Arc<dyn HistoryStore> = Arc::new(store);
        
        // Enqueue worker items
        store.enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test-input".to_string(),
        }).await.expect("Failed to enqueue worker work");
        
        store.enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 2,
            name: "TestActivity2".to_string(),
            input: "test-input-2".to_string(),
        }).await.expect("Failed to enqueue worker work 2");
        
        println!("Phase 1: Enqueued 2 worker items");
    }
    
    // Phase 2: Drop and recreate store, verify persistence
    {
        println!("Phase 2: Recreating store...");
        
        let store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        let store: Arc<dyn HistoryStore> = Arc::new(store);
        
        // Dequeue and verify worker items
        let mut items_found = 0;
        
        while let Some((item, token)) = store.dequeue_worker_peek_lock().await {
            match &item {
                WorkItem::ActivityExecute { name, input, .. } => {
                    println!("✓ Found persisted worker item: {} with input: {}", name, input);
                    items_found += 1;
                }
                _ => panic!("Unexpected work item type"),
            }
            
            // Ack the item to remove it from queue
            store.ack_worker(&token).await.expect("Failed to ack");
        }
        
        assert_eq!(items_found, 2, "Expected 2 worker items to be persisted");
        println!("✓ All worker items persisted correctly!");
    }
    
    // Phase 3: Verify queue is now empty
    {
        let store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        let store: Arc<dyn HistoryStore> = Arc::new(store);
        
        let result = store.dequeue_worker_peek_lock().await;
        assert!(result.is_none(), "Queue should be empty after acking all items");
        println!("✓ Queue correctly empty after processing");
    }
}

#[tokio::test]
async fn test_sqlite_lock_expiry() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test_locks.db");
    
    // Pre-create the database file
    std::fs::File::create(&db_path).expect("Failed to create db file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Set a short lock timeout for testing
    unsafe { std::env::set_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS", "3000") };
    
    // Phase 1: Create store, dequeue item (creating lock), then drop store
    {
        let store_raw = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store");
        let store_arc = Arc::new(store_raw);
        let store: Arc<dyn HistoryStore> = store_arc.clone();
        
        // Enqueue a work item
        store.enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test-input".to_string(),
        }).await.expect("Failed to enqueue worker work");
        
        // Dequeue it (this creates a lock)
        let (item, token) = store.dequeue_worker_peek_lock().await
            .expect("Should have work item");
        
        match &item {
            WorkItem::ActivityExecute { name, .. } => {
                assert_eq!(name, "TestActivity");
                println!("✓ Dequeued item with lock token: {}", token);
            }
            _ => panic!("Unexpected work item type"),
        }
        
        // Don't ack - leave it locked
        println!("Phase 1: Item locked but not acked");
        
        // Debug dump before dropping
        let dump = store_arc.debug_dump().await;
        println!("Before drop - {}", dump);
    }
    
    // Phase 2: Immediately recreate store, item should still be locked
    {
        let store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        let store: Arc<dyn HistoryStore> = Arc::new(store);
        
        let result = store.dequeue_worker_peek_lock().await;
        assert!(result.is_none(), "Item should still be locked");
        println!("✓ Phase 2: Item correctly remains locked");
    }
    
    // Phase 3: Wait for lock to expire
    {
        println!("Phase 3: Waiting for lock to expire (3s)...");
        tokio::time::sleep(tokio::time::Duration::from_millis(3100)).await;
        
        let store_raw = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        let store_arc = Arc::new(store_raw);
        let store: Arc<dyn HistoryStore> = store_arc.clone();
        
        // Debug dump after waiting
        let dump = store_arc.debug_dump().await;
        println!("After lock expiry wait - {}", dump);
        
        // Check current time vs lock timeout
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        println!("Current timestamp: {}", now);
        
        // Direct SQL query to see the locked item
        {
            use sqlx::{SqlitePool, Row};
            let pool = SqlitePool::connect(&db_url).await.unwrap();
            if let Ok(row) = sqlx::query("SELECT id, lock_token, locked_until FROM worker_queue LIMIT 1")
                .fetch_one(&pool)
                .await {
                let id: i64 = row.get("id");
                let lock_token: Option<String> = row.get("lock_token");
                let locked_until: Option<i64> = row.get("locked_until");
                println!("Worker item - id: {}, lock_token: {:?}, locked_until: {:?}", id, lock_token, locked_until);
                if let Some(until) = locked_until {
                    println!("Lock expired? {} <= {} = {}", until, now, until <= now as i64);
                }
            }
            
            // Test the actual SQL query used by dequeue
            let sql_now = sqlx::query_scalar::<_, i64>("SELECT CAST(strftime('%s', 'now') AS INTEGER)")
                .fetch_one(&pool)
                .await
                .unwrap();
            println!("SQL now timestamp: {}", sql_now);
            
            let available_count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM worker_queue WHERE lock_token IS NULL OR locked_until <= strftime('%s', 'now')"
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            println!("Available items count: {}", available_count);
            
            pool.close().await;
        }
        
        // Now the item should be available
        if let Some((item, token)) = store.dequeue_worker_peek_lock().await {
            match &item {
                WorkItem::ActivityExecute { name, .. } => {
                    assert_eq!(name, "TestActivity");
                    println!("✓ Item available after lock expiry");
                    store.ack_worker(&token).await.expect("Failed to ack");
                }
                _ => panic!("Unexpected work item type"),
            }
        } else {
            panic!("Item was lost! This is the bug we're investigating.");
        }
    }
}

#[tokio::test]
async fn test_sqlite_database_file_creation() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("creation_test.db");
    
    // Verify file doesn't exist initially
    assert!(!db_path.exists(), "DB file should not exist initially");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Pre-create the database file
    std::fs::File::create(&db_path).expect("Failed to create db file");
    
    // Create store
    {
        let _store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store");
        
        // File should now exist
        assert!(db_path.exists(), "DB file should exist after store creation");
        
        let metadata = std::fs::metadata(&db_path).expect("Failed to get metadata");
        println!("Initial DB file size: {} bytes", metadata.len());
        assert!(metadata.len() > 0, "DB file should not be empty");
    }
    
    // Store dropped, file should still exist
    assert!(db_path.exists(), "DB file should persist after store is dropped");
    
    // Add data and verify file grows
    let initial_size = std::fs::metadata(&db_path).unwrap().len();
    
    {
        let store = SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        
        // Add multiple items
        for i in 0..10 {
            store.enqueue_worker_work(WorkItem::ActivityExecute {
                instance: format!("test-{}", i),
                execution_id: 1,
                id: i,
                name: format!("Activity{}", i),
                input: format!("input-{}", i),
            }).await.expect("Failed to enqueue");
        }
    }
    
    let final_size = std::fs::metadata(&db_path).unwrap().len();
    println!("Final DB file size: {} bytes (grew by {} bytes)", final_size, final_size - initial_size);
    assert!(final_size > initial_size, "DB file should grow after adding data");
}