//! File read/write tools

use super::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Tool for reading file contents
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            self.workspace_root.join(path)
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the file content with line numbers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to workspace root or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (0-based, optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (optional, defaults to 2000)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("path is required".into()))?;

        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(2000) as usize;

        let full_path = self.resolve_path(path);

        if !full_path.exists() {
            return Err(crate::PawanError::NotFound(format!(
                "File not found: {}",
                full_path.display()
            )));
        }

        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(crate::PawanError::Io)?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let selected_lines: Vec<String> = lines
            .into_iter()
            .skip(offset)
            .take(limit)
            .enumerate()
            .map(|(i, line)| {
                let line_num = offset + i + 1;
                // Truncate very long lines
                let display_line = if line.len() > 2000 {
                    format!("{}...[truncated]", &line[..2000])
                } else {
                    line.to_string()
                };
                format!("{:>6}\t{}", line_num, display_line)
            })
            .collect();

        let output = selected_lines.join("\n");

        Ok(json!({
            "content": output,
            "path": full_path.display().to_string(),
            "total_lines": total_lines,
            "lines_shown": selected_lines.len(),
            "offset": offset
        }))
    }
}

/// Tool for writing file contents
pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            self.workspace_root.join(path)
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if needed. Overwrites existing content."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to workspace root or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("path is required".into()))?;

        let content = args["content"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("content is required".into()))?;

        let full_path = self.resolve_path(path);

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(crate::PawanError::Io)?;
        }

        // Write the file
        tokio::fs::write(&full_path, content)
            .await
            .map_err(crate::PawanError::Io)?;

        let line_count = content.lines().count();

        Ok(json!({
            "success": true,
            "path": full_path.display().to_string(),
            "bytes_written": content.len(),
            "lines": line_count
        }))
    }
}

/// Tool for listing directory contents
pub struct ListDirectoryTool {
    workspace_root: PathBuf,
}

impl ListDirectoryTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            self.workspace_root.join(path)
        }
    }
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list (relative to workspace root or absolute)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list recursively (default: false)"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth for recursive listing (default: 3)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("path is required".into()))?;

        let recursive = args["recursive"].as_bool().unwrap_or(false);
        let max_depth = args["max_depth"].as_u64().unwrap_or(3) as usize;

        let full_path = self.resolve_path(path);

        if !full_path.exists() {
            return Err(crate::PawanError::NotFound(format!(
                "Directory not found: {}",
                full_path.display()
            )));
        }

        if !full_path.is_dir() {
            return Err(crate::PawanError::Tool(format!(
                "Not a directory: {}",
                full_path.display()
            )));
        }

        let mut entries = Vec::new();

        if recursive {
            for entry in walkdir::WalkDir::new(&full_path)
                .max_depth(max_depth)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                let relative = path.strip_prefix(&full_path).unwrap_or(path);
                let is_dir = entry.file_type().is_dir();
                let size = if is_dir {
                    0
                } else {
                    entry.metadata().map(|m| m.len()).unwrap_or(0)
                };

                entries.push(json!({
                    "path": relative.display().to_string(),
                    "is_dir": is_dir,
                    "size": size
                }));
            }
        } else {
            let mut read_dir = tokio::fs::read_dir(&full_path)
                .await
                .map_err(crate::PawanError::Io)?;

            while let Some(entry) = read_dir.next_entry().await.map_err(crate::PawanError::Io)? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let metadata = entry.metadata().await.ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.map(|m| m.len()).unwrap_or(0);

                entries.push(json!({
                    "name": name,
                    "path": path.display().to_string(),
                    "is_dir": is_dir,
                    "size": size
                }));
            }
        }

        Ok(json!({
            "path": full_path.display().to_string(),
            "entries": entries,
            "count": entries.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_read_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "line 1\nline 2\nline 3").unwrap();

        let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();

        assert_eq!(result["total_lines"], 3);
        assert!(result["content"].as_str().unwrap().contains("line 1"));
    }

    #[tokio::test]
    async fn test_write_file() {
        let temp_dir = TempDir::new().unwrap();

        let tool = WriteFileTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "path": "new_file.txt",
                "content": "hello\nworld"
            }))
            .await
            .unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["lines"], 2);

        let content = std::fs::read_to_string(temp_dir.path().join("new_file.txt")).unwrap();
        assert_eq!(content, "hello\nworld");
    }

    #[tokio::test]
    async fn test_list_directory() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("file1.txt"), "content").unwrap();
        std::fs::write(temp_dir.path().join("file2.txt"), "content").unwrap();
        std::fs::create_dir(temp_dir.path().join("subdir")).unwrap();

        let tool = ListDirectoryTool::new(temp_dir.path().to_path_buf());
        let result = tool.execute(json!({"path": "."})).await.unwrap();

        assert_eq!(result["count"], 3);
    }
}
