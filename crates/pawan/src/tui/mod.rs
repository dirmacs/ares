//! Terminal User Interface for Pawan
//!
//! Provides a rich TUI experience using ratatui with:
//! - Streaming response display
//! - Syntax highlighting for code
//! - Message history with scrolling
//! - Multi-line input
//! - Tool execution visualization

use crate::agent::{PawanAgent, Role, ToolCallRecord};
use crate::config::TuiConfig;
use crate::{PawanError, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use tui_textarea::{Input, TextArea};

/// Application state
pub struct App<'a> {
    /// Agent for handling requests
    agent: PawanAgent,
    /// TUI configuration
    config: TuiConfig,
    /// Message history for display
    messages: Vec<DisplayMessage>,
    /// Current input textarea
    input: TextArea<'a>,
    /// Scroll position for messages
    scroll: usize,
    /// Whether currently processing
    processing: bool,
    /// Current streaming content
    current_stream: String,
    /// Tool calls in progress
    tool_calls: Vec<ToolCallRecord>,
    /// Should quit
    should_quit: bool,
    /// Status message
    status: String,
    /// Focused panel
    focus: Panel,
}

/// A message for display in the TUI
#[derive(Clone)]
pub struct DisplayMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Which panel is focused
#[derive(Clone, Copy, PartialEq)]
pub enum Panel {
    Input,
    Messages,
}

impl<'a> App<'a> {
    /// Create a new App
    pub fn new(agent: PawanAgent, config: TuiConfig) -> Self {
        let mut input = TextArea::default();
        input.set_cursor_line_style(Style::default());
        input.set_placeholder_text("Type your message... (Ctrl+Enter to send, Ctrl+C to quit)");

        Self {
            agent,
            config,
            messages: Vec::new(),
            input,
            scroll: 0,
            processing: false,
            current_stream: String::new(),
            tool_calls: Vec::new(),
            should_quit: false,
            status: "Ready".to_string(),
            focus: Panel::Input,
        }
    }

    /// Run the TUI application
    pub async fn run(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode().map_err(PawanError::Io)?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture).map_err(PawanError::Io)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).map_err(PawanError::Io)?;

        // Main loop
        let result = self.main_loop(&mut terminal).await;

        // Restore terminal
        disable_raw_mode().ok();
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )
        .ok();
        terminal.show_cursor().ok();

        result
    }

    /// Main event loop
    async fn main_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // Draw UI
            terminal.draw(|f| self.ui(f)).map_err(PawanError::Io)?;

            // Handle events with timeout for non-blocking
            if event::poll(std::time::Duration::from_millis(100)).map_err(PawanError::Io)? {
                let event = event::read().map_err(PawanError::Io)?;
                self.handle_event(event).await?;
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Handle input events
    async fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => {
                // Global shortcuts
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                        self.should_quit = true;
                        return Ok(());
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                        self.messages.clear();
                        self.agent.clear_history();
                        self.status = "Cleared history".to_string();
                        return Ok(());
                    }
                    _ => {}
                }

                match self.focus {
                    Panel::Input => {
                        // Submit on Ctrl+Enter
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Enter
                        {
                            self.submit_input().await?;
                        }
                        // Tab to switch focus
                        else if key.code == KeyCode::Tab {
                            self.focus = Panel::Messages;
                        }
                        // Pass to textarea
                        else {
                            let input = Input::from(key);
                            self.input.input(input);
                        }
                    }
                    Panel::Messages => match key.code {
                        KeyCode::Tab => {
                            self.focus = Panel::Input;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.scroll = self.scroll.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.scroll = self.scroll.saturating_add(1);
                        }
                        KeyCode::PageUp => {
                            self.scroll = self.scroll.saturating_sub(10);
                        }
                        KeyCode::PageDown => {
                            self.scroll = self.scroll.saturating_add(10);
                        }
                        KeyCode::Home => {
                            self.scroll = 0;
                        }
                        KeyCode::End => {
                            self.scroll = self.messages.len().saturating_sub(1);
                        }
                        _ => {}
                    },
                }
            }
            Event::Mouse(mouse) => {
                if self.config.mouse_support {
                    match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            self.scroll = self.scroll.saturating_sub(self.config.scroll_speed);
                        }
                        event::MouseEventKind::ScrollDown => {
                            self.scroll = self.scroll.saturating_add(self.config.scroll_speed);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Submit the current input
    async fn submit_input(&mut self) -> Result<()> {
        let content: String = self.input.lines().join("\n");
        if content.trim().is_empty() {
            return Ok(());
        }

        // Clear input
        self.input = TextArea::default();
        self.input.set_cursor_line_style(Style::default());
        self.input
            .set_placeholder_text("Type your message... (Ctrl+Enter to send, Ctrl+C to quit)");

        // Add user message
        self.messages.push(DisplayMessage {
            role: Role::User,
            content: content.clone(),
            tool_calls: vec![],
        });

        // Process
        self.processing = true;
        self.status = "Processing...".to_string();
        self.current_stream.clear();
        self.tool_calls.clear();

        // Execute agent
        let response = self.agent.execute(&content).await;

        self.processing = false;

        match response {
            Ok(resp) => {
                self.messages.push(DisplayMessage {
                    role: Role::Assistant,
                    content: resp.content,
                    tool_calls: resp.tool_calls,
                });
                self.status = format!("Done ({} iterations)", resp.iterations);
                // Scroll to bottom
                self.scroll = self.messages.len().saturating_sub(1);
            }
            Err(e) => {
                self.status = format!("Error: {}", e);
                self.messages.push(DisplayMessage {
                    role: Role::Assistant,
                    content: format!("Error: {}", e),
                    tool_calls: vec![],
                });
            }
        }

        Ok(())
    }

    /// Render the UI
    fn ui(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Min(3),    // Messages
                Constraint::Length(6), // Input
                Constraint::Length(1), // Status
            ])
            .split(f.area());

        self.render_messages(f, chunks[0]);
        self.render_input(f, chunks[1]);
        self.render_status(f, chunks[2]);
    }

    /// Render the message list
    fn render_messages(&self, f: &mut Frame, area: Rect) {
        let mut items: Vec<ListItem> = Vec::new();

        for msg in &self.messages {
            let (prefix, style) = match msg.role {
                Role::User => (
                    "You: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Role::Assistant => (
                    "Pawan: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Role::System => ("System: ", Style::default().fg(Color::Yellow)),
                Role::Tool => ("Tool: ", Style::default().fg(Color::Magenta)),
            };

            // Add message
            let header = Line::from(vec![Span::styled(prefix, style)]);
            items.push(ListItem::new(header));

            // Add content lines
            for line in msg.content.lines() {
                let line = Line::from(Span::raw(format!("  {}", line)));
                items.push(ListItem::new(line));
            }

            // Add tool calls
            for tc in &msg.tool_calls {
                let status_icon = if tc.success { "✓" } else { "✗" };
                let tc_line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", status_icon),
                        Style::default().fg(if tc.success { Color::Green } else { Color::Red }),
                    ),
                    Span::styled(
                        format!("{}({}) ", tc.name, tc.duration_ms),
                        Style::default().fg(Color::Magenta),
                    ),
                ]);
                items.push(ListItem::new(tc_line));
            }

            // Separator
            items.push(ListItem::new(Line::from("")));
        }

        // Add streaming content if processing
        if self.processing && !self.current_stream.is_empty() {
            let header = Line::from(vec![Span::styled(
                "Pawan: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]);
            items.push(ListItem::new(header));

            for line in self.current_stream.lines() {
                items.push(ListItem::new(Line::from(format!("  {}", line))));
            }
            items.push(ListItem::new(Line::from(vec![Span::styled(
                "  ▌",
                Style::default().fg(Color::Green),
            )])));
        }

        let border_style = if self.focus == Panel::Messages {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let messages_block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(" Messages (Tab to focus, j/k to scroll) ");

        let list = List::new(items).block(messages_block);

        f.render_widget(list, area);
    }

    /// Render the input area
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let border_style = if self.focus == Panel::Input {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let title = if self.processing {
            " Input (processing...) "
        } else {
            " Input (Ctrl+Enter to send) "
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Render textarea inside (pass reference directly per updated API)
        f.render_widget(&self.input, inner);
    }

    /// Render the status bar
    fn render_status(&self, f: &mut Frame, area: Rect) {
        let status_style = if self.processing {
            Style::default().fg(Color::Yellow)
        } else if self.status.starts_with("Error") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let model = &self.agent.config().model;
        let status = Paragraph::new(Line::from(vec![
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            Span::styled(model, Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::styled(&self.status, status_style),
            Span::raw(" | "),
            Span::styled("Ctrl+L: clear", Style::default().fg(Color::DarkGray)),
            Span::raw(" | "),
            Span::styled("Ctrl+C: quit", Style::default().fg(Color::DarkGray)),
        ]));

        f.render_widget(status, area);
    }
}

/// Run the TUI with the given agent
pub async fn run_tui(agent: PawanAgent, config: TuiConfig) -> Result<()> {
    let mut app = App::new(agent, config);
    app.run().await
}

/// Simple non-TUI interactive mode (fallback)
pub async fn run_simple(mut agent: PawanAgent) -> Result<()> {
    use std::io::{BufRead, Write};

    println!("Pawan - Self-Healing CLI Coding Agent");
    println!("Type 'quit' or 'exit' to quit, 'clear' to clear history");
    println!("---");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    loop {
        print!("> ");
        stdout.flush().ok();

        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();

        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        if line == "quit" || line == "exit" {
            break;
        }

        if line == "clear" {
            agent.clear_history();
            println!("History cleared.");
            continue;
        }

        // Execute
        println!("\nProcessing...\n");

        match agent.execute(line).await {
            Ok(response) => {
                println!("{}\n", response.content);

                if !response.tool_calls.is_empty() {
                    println!("Tool calls made:");
                    for tc in &response.tool_calls {
                        let status = if tc.success { "✓" } else { "✗" };
                        println!("  {} {} ({}ms)", status, tc.name, tc.duration_ms);
                    }
                    println!();
                }
            }
            Err(e) => {
                println!("Error: {}\n", e);
            }
        }
    }

    Ok(())
}
