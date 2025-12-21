//! Colored output helpers for CLI
//!
//! Provides consistent, colored terminal output for the A.R.E.S CLI.

use owo_colors::OwoColorize;
use std::io::{self, Write};

/// Output style configuration
pub struct Output {
    /// Whether to use colored output
    pub colored: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

impl Output {
    /// Create a new output helper with colors enabled
    pub fn new() -> Self {
        Self { colored: true }
    }

    /// Create a new output helper with colors disabled
    pub fn no_color() -> Self {
        Self { colored: false }
    }

    /// Print the A.R.E.S banner
    pub fn banner(&self) {
        if self.colored {
            println!(
                r#"
   {}
   {}
   {}
   {}
   {}
"#,
                "    _    ____  _____ ____  ".bright_cyan().bold(),
                "   / \\  |  _ \\| ____/ ___| ".bright_cyan().bold(),
                "  / _ \\ | |_) |  _| \\___ \\ ".cyan().bold(),
                " / ___ \\|  _ <| |___ ___) |".blue().bold(),
                "/_/   \\_\\_| \\_\\_____|____/ ".blue().bold(),
            );
            println!(
                "   {} {}\n",
                "Agentic Retrieval Enhanced Server".bright_white().bold(),
                format!("v{}", env!("CARGO_PKG_VERSION")).dimmed()
            );
        } else {
            println!(
                r#"
    _    ____  _____ ____
   / \  |  _ \| ____/ ___|
  / _ \ | |_) |  _| \___ \
 / ___ \|  _ <| |___ ___) |
/_/   \_\_| \_\_____|____/

   Agentic Retrieval Enhanced Server v{}
"#,
                env!("CARGO_PKG_VERSION")
            );
        }
    }

    /// Print a success message with a checkmark
    pub fn success(&self, message: &str) {
        if self.colored {
            println!("  {} {}", "✓".green().bold(), message.green());
        } else {
            println!("  [OK] {}", message);
        }
    }

    /// Print an info message
    pub fn info(&self, message: &str) {
        if self.colored {
            println!("  {} {}", "•".blue(), message);
        } else {
            println!("  [INFO] {}", message);
        }
    }

    /// Print a warning message
    pub fn warning(&self, message: &str) {
        if self.colored {
            println!("  {} {}", "⚠".yellow().bold(), message.yellow());
        } else {
            println!("  [WARN] {}", message);
        }
    }

    /// Print an error message
    pub fn error(&self, message: &str) {
        if self.colored {
            eprintln!("  {} {}", "✗".red().bold(), message.red());
        } else {
            eprintln!("  [ERROR] {}", message);
        }
    }

    /// Print a step message (for multi-step operations)
    pub fn step(&self, step_num: u32, total: u32, message: &str) {
        if self.colored {
            println!(
                "  {} {}",
                format!("[{}/{}]", step_num, total).dimmed(),
                message.bright_white()
            );
        } else {
            println!("  [{}/{}] {}", step_num, total, message);
        }
    }

    /// Print a file creation message
    pub fn created(&self, file_type: &str, path: &str) {
        if self.colored {
            println!(
                "  {} {} {}",
                "✓".green().bold(),
                file_type.dimmed(),
                path.bright_white()
            );
        } else {
            println!("  [CREATED] {} {}", file_type, path);
        }
    }

    /// Print a file skipped message
    pub fn skipped(&self, path: &str, reason: &str) {
        if self.colored {
            println!(
                "  {} {} {}",
                "○".yellow(),
                path.dimmed(),
                format!("({})", reason).yellow()
            );
        } else {
            println!("  [SKIPPED] {} ({})", path, reason);
        }
    }

    /// Print a directory creation message
    pub fn created_dir(&self, path: &str) {
        if self.colored {
            println!(
                "  {} {} {}",
                "✓".green().bold(),
                "directory".dimmed(),
                path.bright_white()
            );
        } else {
            println!("  [CREATED] directory {}", path);
        }
    }

    /// Print a header for a section
    pub fn header(&self, title: &str) {
        if self.colored {
            println!("\n  {}", title.bright_white().bold().underline());
        } else {
            println!("\n  === {} ===", title);
        }
    }

    /// Print a subheader
    pub fn subheader(&self, title: &str) {
        if self.colored {
            println!("\n  {}", title.cyan().bold());
        } else {
            println!("\n  --- {} ---", title);
        }
    }

    /// Print a key-value pair
    pub fn kv(&self, key: &str, value: &str) {
        if self.colored {
            println!("    {}: {}", key.dimmed(), value.bright_white());
        } else {
            println!("    {}: {}", key, value);
        }
    }

    /// Print a list item
    pub fn list_item(&self, item: &str) {
        if self.colored {
            println!("    {} {}", "•".blue(), item);
        } else {
            println!("    - {}", item);
        }
    }

    /// Print a hint/tip message
    pub fn hint(&self, message: &str) {
        if self.colored {
            println!("\n  {} {}", "💡".dimmed(), message.dimmed().italic());
        } else {
            println!("\n  [TIP] {}", message);
        }
    }

    /// Print a command suggestion
    pub fn command(&self, cmd: &str) {
        if self.colored {
            println!("     {}", format!("$ {}", cmd).bright_cyan());
        } else {
            println!("     $ {}", cmd);
        }
    }

    /// Print completion message with next steps
    pub fn complete(&self, message: &str) {
        if self.colored {
            println!("\n  {} {}", "🚀".green(), message.bright_green().bold());
        } else {
            println!("\n  [DONE] {}", message);
        }
    }

    /// Prompt for confirmation (returns true if user confirms)
    pub fn confirm(&self, message: &str) -> bool {
        if self.colored {
            print!(
                "  {} {} [y/N]: ",
                "?".bright_yellow().bold(),
                message.bright_white()
            );
        } else {
            print!("  [?] {} [y/N]: ", message);
        }

        io::stdout().flush().ok();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let input = input.trim().to_lowercase();
            input == "y" || input == "yes"
        } else {
            false
        }
    }

    /// Print a table header row
    pub fn table_header(&self, columns: &[&str]) {
        if self.colored {
            let header: String = columns
                .iter()
                .map(|c| format!("{:<15}", c))
                .collect::<Vec<_>>()
                .join(" ");
            println!("    {}", header.bright_white().bold());
            println!("    {}", "─".repeat(columns.len() * 16).dimmed());
        } else {
            let header: String = columns
                .iter()
                .map(|c| format!("{:<15}", c))
                .collect::<Vec<_>>()
                .join(" ");
            println!("    {}", header);
            println!("    {}", "-".repeat(columns.len() * 16));
        }
    }

    /// Print a table row
    pub fn table_row(&self, values: &[&str]) {
        let row: String = values
            .iter()
            .map(|v| format!("{:<15}", v))
            .collect::<Vec<_>>()
            .join(" ");
        println!("    {}", row);
    }

    /// Print newline
    pub fn newline(&self) {
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_new() {
        let output = Output::new();
        assert!(output.colored);
    }

    #[test]
    fn test_output_no_color() {
        let output = Output::no_color();
        assert!(!output.colored);
    }

    #[test]
    fn test_output_default() {
        let output = Output::default();
        assert!(output.colored);
    }

    #[test]
    fn test_confirm_parsing() {
        // Note: We can't easily test interactive confirm in unit tests,
        // but we can verify the Output struct is created correctly
        let output = Output::new();
        assert!(output.colored);

        let output_no_color = Output::no_color();
        assert!(!output_no_color.colored);
    }

    #[test]
    fn test_table_row_formatting() {
        // Verify table row doesn't panic with various inputs
        let output = Output::no_color();

        // These should not panic
        output.table_row(&["a", "b", "c"]);
        output.table_row(&["long_value_here", "another", "third"]);
        output.table_row(&[]);
    }

    #[test]
    fn test_table_header_formatting() {
        // Verify table header doesn't panic with various inputs
        let output = Output::no_color();

        // These should not panic
        output.table_header(&["Name", "Model", "Tools"]);
        output.table_header(&["Single"]);
        output.table_header(&[]);
    }

    #[test]
    fn test_output_methods_no_panic() {
        // Smoke test - ensure none of the output methods panic
        let output = Output::no_color();

        output.success("test success");
        output.info("test info");
        output.warning("test warning");
        output.error("test error");
        output.step(1, 3, "step message");
        output.created("file", "path/to/file");
        output.skipped("path", "reason");
        output.created_dir("some/dir");
        output.header("Test Header");
        output.subheader("Test Subheader");
        output.kv("key", "value");
        output.list_item("item");
        output.hint("hint message");
        output.command("some command");
        output.complete("complete message");
        output.newline();
    }

    #[test]
    fn test_output_methods_colored_no_panic() {
        // Smoke test for colored output
        let output = Output::new();

        output.success("test success");
        output.info("test info");
        output.warning("test warning");
        output.error("test error");
        output.step(1, 3, "step message");
        output.created("file", "path/to/file");
        output.skipped("path", "reason");
        output.created_dir("some/dir");
        output.header("Test Header");
        output.subheader("Test Subheader");
        output.kv("key", "value");
        output.list_item("item");
        output.hint("hint message");
        output.command("some command");
        output.complete("complete message");
        output.newline();
        output.banner();
    }
}
