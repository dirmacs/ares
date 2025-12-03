use crate::types::{Message, Result};
use std::collections::VecDeque;

pub struct ContextManager {
    max_tokens: usize,
    messages: VecDeque<Message>,
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            messages: VecDeque::new(),
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push_back(message);
        self.trim_if_needed();
    }

    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.iter().cloned().collect()
    }

    fn trim_if_needed(&mut self) {
        // Simple token counting (4 chars ≈ 1 token)
        while self.estimate_tokens() > self.max_tokens && self.messages.len() > 1 {
            self.messages.pop_front();
        }
    }

    fn estimate_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.content.len() / 4).sum()
    }
}
