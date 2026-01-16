//! Edit tool for precise string replacement

use super::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Tool for editing files with precise string replacement
pub struct EditFileTool {
    workspace_root: PathBuf,
}

impl EditFileTool {
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
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string with a new string. \
         The old_string must match exactly (including whitespace). \
         Use replace_all to replace all occurrences."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The string to replace it with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("path is required".into()))?;

        let old_string = args["old_string"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("old_string is required".into()))?;

        let new_string = args["new_string"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("new_string is required".into()))?;

        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        let full_path = self.resolve_path(path);

        if !full_path.exists() {
            return Err(crate::PawanError::NotFound(format!(
                "File not found: {}",
                full_path.display()
            )));
        }

        // Read current content
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(crate::PawanError::Io)?;

        // Count occurrences
        let occurrence_count = content.matches(old_string).count();

        if occurrence_count == 0 {
            return Err(crate::PawanError::Tool(
                "old_string not found in file. Make sure the string matches exactly including whitespace.".to_string()
            ));
        }

        if occurrence_count > 1 && !replace_all {
            return Err(crate::PawanError::Tool(format!(
                "old_string found {} times. Use replace_all: true to replace all, \
                 or provide more context to make the match unique.",
                occurrence_count
            )));
        }

        // Perform replacement
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        // Write back
        tokio::fs::write(&full_path, &new_content)
            .await
            .map_err(crate::PawanError::Io)?;

        // Generate a diff preview
        let diff = generate_diff(&content, &new_content, path);

        Ok(json!({
            "success": true,
            "path": full_path.display().to_string(),
            "replacements": if replace_all { occurrence_count } else { 1 },
            "diff": diff
        }))
    }
}

/// Generate a simple diff between two strings
fn generate_diff(old: &str, new: &str, filename: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(old, new);
    let mut result = String::new();

    result.push_str(&format!("--- a/{}\n", filename));
    result.push_str(&format!("+++ b/{}\n", filename));

    for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            result.push_str("...\n");
        }

        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                result.push_str(&format!("{}{}", sign, change));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_edit_file_single_replacement() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {\n    println!(\"Hello\");\n}").unwrap();

        let tool = EditFileTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "path": "test.rs",
                "old_string": "println!(\"Hello\")",
                "new_string": "println!(\"Hello, World!\")"
            }))
            .await
            .unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["replacements"], 1);

        let new_content = std::fs::read_to_string(&file_path).unwrap();
        assert!(new_content.contains("Hello, World!"));
    }

    #[tokio::test]
    async fn test_edit_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let tool = EditFileTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "path": "test.rs",
                "old_string": "nonexistent",
                "new_string": "replacement"
            }))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_file_multiple_without_replace_all() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "let x = 1;\nlet x = 2;").unwrap();

        let tool = EditFileTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "path": "test.rs",
                "old_string": "let x",
                "new_string": "let y"
            }))
            .await;

        // Should fail because there are multiple occurrences
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_file_replace_all() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "let x = 1;\nlet x = 2;").unwrap();

        let tool = EditFileTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "path": "test.rs",
                "old_string": "let x",
                "new_string": "let y",
                "replace_all": true
            }))
            .await
            .unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["replacements"], 2);

        let new_content = std::fs::read_to_string(&file_path).unwrap();
        assert!(!new_content.contains("let x"));
        assert!(new_content.contains("let y"));
    }
}
