//! Lexer for the Suriv surface syntax (spec v2 §1).
//!
//! Longest-match closed phrase table over the 11 semantic tokens
//! (`semanticTokenCount := 11`, Syntax.lean:116), then resource literals,
//! then identifiers, then prose. Indentation is not significant; block
//! extent is by phrase.
//!
//! Token stream shape: each line is emitted as its keyword/word tokens
//! followed by one `Period` per sentence-final `.` and one `Newline`.
//! Prose that carries inline sentence periods (`It uses gpt-4o. It can
//! diagnose.`) emits several `Period` tokens on the same line.

pub const SEMANTIC_TOKEN_COUNT: usize = 11;

/// Row count of the closed phrase table inside `Lexer::match_keyword`.
/// Exposed structurally so tests can assert the vocabulary's ceiling
/// without duplicating the table: `phrase_table_len() -
/// phrase_table_kind_rows()` MUST equal `SEMANTIC_TOKEN_COUNT`, which
/// mirrors the Lean `semanticTokenCount` (Syntax.lean:116).
pub fn phrase_table_len() -> usize {
    Lexer::PHRASE_TABLE.len()
}

/// How many phrase-table rows spell kind tokens. Five surface spellings
/// (`an agent`, `a tool`, `a knowledge source`, `a connector`, `a human`)
/// share one semantic token (`Token::Kind`), so the semantic-token
/// ceiling is the row count minus this.
pub fn phrase_table_kind_rows() -> usize {
    Lexer::PHRASE_TABLE
        .iter()
        .filter(|(_, t)| matches!(t, Token::Kind(_)))
        .count()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    // Boundary (Syntax.lean:81-82)
    HarnessBegins, // "this harness begins as"
    HarnessEnds,   // "this harness ends"
    // Block (§3.1)
    BringIn,    // "bring in"
    Kind(Kind), // "an agent" / "a tool" / "a knowledge source" / "a connector" / "a human"
    // Call (§3.2)
    HandThisTo, // "hand this to"
    // Coeffect (§3.9)
    Require, // "require"
    // Conditional (§3.3)
    When, // "when"
    // Parallel (§3.4)
    AtTheSameTime, // "at the same time"
    // Data (§3.5)
    Resource(Resource),
    // Binding (§3.7-3.8)
    CallThis, // "call this"
    Recall,   // "recall"
    // Prose and identifiers (Syntax.lean:107-110)
    Name(String),
    Period,
    Newline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Tool,
    Knowledge,
    Connector,
    Human,
}

/// Resource spellings are exactly four (§1, Primitives.lean:136-141).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    File(String),
    Dir(String),
    Env(String),
    Inline(String),
}

impl Kind {
    /// `kindFromText` (Syntax.lean:287-294): exactly one spelling per kind.
    /// `a knowledge source` is the only knowledge spelling (no-aliases).
    pub fn from_text(s: &str) -> Option<Kind> {
        match s {
            "an agent" => Some(Kind::Agent),
            "a tool" => Some(Kind::Tool),
            "a knowledge source" => Some(Kind::Knowledge),
            "a connector" => Some(Kind::Connector),
            "a human" => Some(Kind::Human),
            _ => None,
        }
    }
}

pub struct Lexer {
    chars: Vec<char>,
    pos: usize,
    /// Trailing `.` characters split off the word just taken, emitted
    /// after that word's `Name`.
    pending_periods: usize,
}

impl Lexer {
    pub fn new(src: &str) -> Lexer {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
            pending_periods: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    pub fn lex(&mut self) -> Result<Vec<Token>, String> {
        let mut out = Vec::new();
        loop {
            if self.pending_periods > 0 {
                let n = self.pending_periods;
                self.pending_periods = 0;
                for _ in 0..n {
                    out.push(Token::Period);
                }
                continue;
            }
            // Separator whitespace. (`,` is lexed as part of words — see
            // `take_word` — and stripped at declaration sites instead.)
            while matches!(self.peek(), Some(' ') | Some('\t') | Some('\r')) {
                self.pos += 1;
            }
            let Some(c) = self.peek() else { break };
            if c == '\n' {
                self.pos += 1;
                out.push(Token::Newline);
                continue;
            }
            if let Some(tok) = self.match_resource()? {
                out.push(tok);
                continue;
            }
            if let Some(tok) = self.match_keyword() {
                out.push(tok);
                continue;
            }
            if c == '.' {
                self.pos += 1;
                out.push(Token::Period);
                continue;
            }
            let word = self.take_word();
            if word.is_empty() {
                // Unrecognised character: consume it so lexing terminates.
                self.pos += 1;
                continue;
            }
            out.push(Token::Name(word));
        }
        Ok(out)
    }

    /// `@file("path")` / `@dir("path")` / `@env("NAME")` / `@inline("text")`.
    /// Scanned from the current position so prose after the resource that
    /// contains brackets cannot shift the delimiters. A period immediately
    /// after `)` terminates the statement (§1).
    fn match_resource(&mut self) -> Result<Option<Token>, String> {
        let rest: String = self.chars[self.pos..].iter().collect();
        let Some(kind) = ["file", "dir", "env", "inline"]
            .into_iter()
            .find(|k| rest.starts_with(&format!("@{k}(")))
        else {
            return Ok(None);
        };
        let mut i = self.pos + 1 + kind.len();
        if self.chars.get(i) != Some(&'(') {
            return Err(format!("resource @{kind}: missing `(`"));
        }
        i += 1;
        if self.chars.get(i) != Some(&'"') {
            return Err(format!(
                "resource @{kind}: argument must be a quoted string"
            ));
        }
        i += 1;
        let text_start = i;
        while i < self.chars.len() && self.chars[i] != '"' {
            i += 1;
        }
        if i >= self.chars.len() {
            return Err(format!("resource @{kind}: missing closing quote"));
        }
        let value: String = self.chars[text_start..i].iter().collect();
        i += 1;
        if self.chars.get(i) != Some(&')') {
            return Err(format!("resource @{kind}: missing `)`"));
        }
        self.pos = i + 1;
        if self.peek() == Some('.') {
            self.pos += 1;
            self.pending_periods = 1;
        }
        let resource = match kind {
            "file" => Resource::File(value),
            "dir" => Resource::Dir(value),
            "env" => Resource::Env(value),
            "inline" => Resource::Inline(value),
            _ => unreachable!(),
        };
        Ok(Some(Token::Resource(resource)))
    }

    /// The closed phrase table (spec v2 §1). Longest-match: a longer
    /// phrase sharing a prefix wins over a shorter one. Reactive
    /// spellings (`watch`, `on change of`) are deliberately ABSENT: they
    /// lex as ordinary `Name` words and the parser matches them at
    /// statement start (reactive-spec §2, ceiling rule: zero lexer-token
    /// changes).
    const PHRASE_TABLE: &'static [(&'static str, Token)] = &[
        ("this harness begins as", Token::HarnessBegins),
        ("this harness ends", Token::HarnessEnds),
        ("at the same time", Token::AtTheSameTime),
        ("hand this to", Token::HandThisTo),
        ("a knowledge source", Token::Kind(Kind::Knowledge)),
        ("bring in", Token::BringIn),
        ("a human", Token::Kind(Kind::Human)),
        ("an agent", Token::Kind(Kind::Agent)),
        ("a tool", Token::Kind(Kind::Tool)),
        ("a connector", Token::Kind(Kind::Connector)),
        ("call this", Token::CallThis),
        ("require", Token::Require),
        ("recall", Token::Recall),
        ("when", Token::When),
    ];

    /// Longest-match over the closed phrase table. A trailing separator
    /// (space / newline / period / comma / end) is required so `bring inx`
    /// is not a keyword.
    fn match_keyword(&mut self) -> Option<Token> {
        let rest: String = self.chars[self.pos..].iter().collect();
        let mut best: Option<(usize, Token)> = None;
        for (phrase, tok) in Self::PHRASE_TABLE {
            if let Some(after_str) = rest.strip_prefix(phrase) {
                let after = after_str.chars().next();
                let terminated = matches!(
                    after,
                    None | Some(' ') | Some('\t') | Some('\n') | Some('\r') | Some('.') | Some(',')
                );
                if terminated && best.as_ref().is_none_or(|(len, _)| phrase.len() > *len) {
                    best = Some((phrase.len(), tok.clone()));
                }
            }
        }
        let (len, tok) = best?;
        self.pos += len;
        Some(tok)
    }

    /// Identifiers and prose words. `.` and `,` join a word so a sentence
    /// keeps `claude-3.5-sonnet.`, `urgency,`, and `router,` intact; the
    /// trailing `.` characters are then emitted as `Period` tokens via
    /// `pending_periods` (emitted exactly once — `pos` advances PAST the
    /// punctuation so the `.` arm never re-emits them). A trailing `,`
    /// stays attached to the word: prose keeps its commas, and
    /// declaration sites strip them (parser `parse_name`).
    fn take_word(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ',' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        // Trailing `.` chars become pending Periods; a trailing `,` stays
        // attached (prose keeps commas; declaration sites strip them).
        let no_dots = word.trim_end_matches('.');
        let dots = word.len() - no_dots.len();
        // A word of bare commas (` , `) is a separator, not a name.
        let core = no_dots.trim_end_matches(',');
        if core.is_empty() {
            return String::new();
        }
        // Do NOT rewind: pos already sits past the punctuation, so the
        // `.` arm and the separator loop never see it again. Pending
        // periods are the sole emission path (fixes double-Period bug).
        // Commas survive only when attached to word content.
        let keep_comma = no_dots.len() > core.len();
        self.pending_periods = dots;
        if keep_comma {
            format!("{core},")
        } else {
            core.to_string()
        }
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    Lexer::new(src).lex()
}
