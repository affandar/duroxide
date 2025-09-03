use std::path::Path;
use tempfile::TempDir;

/// Helper function to ensure proper cleanup of temporary directories
/// This is a workaround for tempfile cleanup issues in parallel test execution
pub fn cleanup_temp_dir(temp_dir: TempDir) {
    // Force cleanup by calling the cleanup method
    let _ = temp_dir.close();
}

/// Helper function to clean up any leftover test directories
/// This can be called at the start of tests to clean up any previous runs
pub fn cleanup_test_directories() {
    let current_dir = std::env::current_dir().unwrap();
    if let Ok(entries) = std::fs::read_dir(&current_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name() {
                    if let Some(name_str) = name.to_str() {
                        if name_str.starts_with("test-fs-") {
                            let _ = std::fs::remove_dir_all(&path);
                        }
                    }
                }
            }
        }
    }
}
