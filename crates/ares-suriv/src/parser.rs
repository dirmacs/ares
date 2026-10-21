//! Parser: tokens -> AST (spec v2 §2-§3, §6).
//!
//! Statements are period-terminated. Block extent is by phrase, never by
//! indentation (§1). The phrase sets that close a block are:
//!
//! - harness body: `this harness ends` (end of file)
//! - `when` body: next `when`, next `bring in`, `this harness ends`
//! - entity body: next `bring in`, next `when`, next `at the same time`,
//!   `this harness ends` (§3.1)
//! - branch list: next `at the same time`, next `when`, next `bring in`,
//!   `this harness ends`
//!
//! Consecutive block-opening phrases are therefore siblings inside the
//! enclosing body, which is what makes the §9 nesting and the Lean
//! `exampleHarness` come out identical without reading indentation.

use crate::ast::{Entity, Expr, Harness};
use crate::lexer::{Kind, Resource, Token};

pub fn parse(tokens: &[Token]) -> Result<Harness, String> {
    let line_starts = line_start_indices(tokens);
    Parser {
        tokens,
        pos: 0,
        line_starts,
    }
    .parse_harness()
}

/// Indices of the first token of each source line. `Newline` tokens are
/// separators between lines; a line's first token is the one right after
/// a separator (or index 0). This is what tells a statement-opening
/// phrase from the same word inside prose (`notices when ...`).
fn line_start_indices(tokens: &[Token]) -> Vec<usize> {
    let mut starts = Vec::new();
    if !tokens.is_empty() {
        starts.push(0);
    }
    for (i, t) in tokens.iter().enumerate() {
        if matches!(t, Token::Newline) && i + 1 < tokens.len() {
            starts.push(i + 1);
        }
    }
    starts
}

/// Phrases that close a nested block. `None` (end of tokens) always
/// closes. The harness body is closed only by `this harness ends`.
#[derive(Clone, Copy)]
enum Scope {
    Harness,
    Block,
    Branches,
    /// An entity body (§3.1): closed by the next `bring in`, `when`,
    /// `at the same time`, watch phrase, and the harness end. A watch
    /// phrase inside it is an ordinary statement (§2: "placement is a
    /// statement in E's body, exactly like `require`"), so it does not
    /// close the entity body — this is what distinguishes it from
    /// `Scope::Block`.
    Entity,
    /// A `watch` / `on change of` body (§reactive-spec §2): block extent
    /// by phrase like a `when` body, but `when` and `at the same time`
    /// inside it are ORDINARY statements (the watch body is copied
    /// verbatim into each desugared `when`), so they do not close it.
    /// Only the next `bring in`, the next watch phrase, and the harness
    /// end close it.
    Watch,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Indices of each line's first token (see `line_start_indices`).
    line_starts: Vec<usize>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Token::Newline)) {
            self.pos += 1;
        }
    }

    /// True when the current token begins a statement, i.e. it is the
    /// first token on its line. The lexer's closed phrase table matches
    /// keywords anywhere, so `notices when the ledger comes back.` lexes
    /// `when` mid-line (§3.6 prose): a phrase preceded by a word, a
    /// period, or a resource on the same line is an ordinary word inside
    /// prose or a condition, never a statement head. Statement extent is
    /// by phrase (§1), and a phrase starts a line.
    ///
    /// Tracked by counting the `Newline` tokens seen so far rather than
    /// by looking at the previous token: `expect_period` consumes the
    /// newline that terminates a statement, so the token after it starts
    /// a fresh line even though its predecessor is a word.
    fn at_statement_start(&self) -> bool {
        // `line_starts` holds the index of the first token of every line
        // emitted so far; membership is exact.
        self.line_starts.contains(&self.pos)
    }

    /// True when `pos` begins a reactive watch phrase (`watch` or
    /// `on change of`). The spellings are ordinary `Name` words (the
    /// lexer's closed phrase table has no reactive rows), so the phrase
    /// is recognised here on word sequences instead of tokens.
    ///
    /// Callers gate on `at_statement_start()` first: a mid-line `watch`
    /// is prose (§3.6) and never opens or closes a block.
    fn at_watch_phrase_start(&self, word: &str) -> bool {
        match word {
            "watch" => true,
            // `on change of` must be three consecutive tokens on the same
            // line: spaces emit no tokens, so a `Newline` between any of
            // them means the phrase is broken across lines and the words
            // are prose, not the reactive spelling.
            "on" => {
                matches!(self.tokens.get(self.pos + 1), Some(Token::Name(w)) if w == "change")
                    && matches!(self.tokens.get(self.pos + 2), Some(Token::Name(w)) if w == "of")
            }
            _ => false,
        }
    }

    fn closes(&self, scope: Scope) -> bool {
        // A phrase token that is NOT at a statement start is prose: it
        // closes nothing. (`notices when ...` must not end a block.)
        if !self.at_statement_start() {
            return false;
        }
        match self.peek() {
            None => true,
            Some(Token::HarnessEnds) => true,
            // Inside a `watch` body, `when` and `at the same time` are
            // ORDINARY statements (the body is copied verbatim into each
            // desugared `when`), so they do not close it.
            Some(Token::When) => matches!(scope, Scope::Block | Scope::Branches | Scope::Entity),
            Some(Token::BringIn) => !matches!(scope, Scope::Harness),
            // A watch phrase closes any nested block — including another
            // watch body and a `when` body — but not an entity body or
            // the harness body, where it is a sibling statement (§2:
            // "placement is a statement in E's body, exactly like
            // `require`"). Without the `Entity` exclusion the fixture's
            // `on change of` line ended the entity body early, so the
            // epoch `when` never reached the entity and its body's two
            // describes vanished from the compiled prompt.
            //
            // Matched on `Name` words, not tokens (reactive-spec §2
            // ceiling rule): the reactive spellings are NOT in the closed
            // phrase table, so the lexer emits them as ordinary words and
            // the parser recognises the phrase here. Same-line 3-word
            // lookahead keeps `on change of` whole: a line break inside
            // the phrase leaves it prose.
            Some(Token::Name(w))
                if self.at_watch_phrase_start(w)
                    && matches!(scope, Scope::Block | Scope::Branches | Scope::Watch) =>
            {
                true
            }
            Some(Token::AtTheSameTime) => {
                matches!(scope, Scope::Block | Scope::Branches | Scope::Entity)
            }
            _ => false,
        }
    }

    fn parse_harness(&mut self) -> Result<Harness, String> {
        self.skip_newlines();
        match self.bump() {
            Some(Token::HarnessBegins) => {}
            other => {
                return Err(format!(
                    "expected `this harness begins as`, found {other:?}"
                ));
            }
        }
        let name = self.parse_name("harness name")?;
        self.expect_period()?;
        let body = self.parse_body(Scope::Harness)?;
        self.skip_newlines();
        match self.bump() {
            Some(Token::HarnessEnds) => {}
            other => return Err(format!("expected `this harness ends`, found {other:?}")),
        }
        Ok(Harness { name, body })
    }

    fn parse_body(&mut self, scope: Scope) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        loop {
            self.skip_newlines();
            if self.closes(scope) {
                return Ok(out);
            }
            // A multi-watch statement expands to sibling `when`s in the
            // enclosing body (reactive-spec §2: siblings in written
            // order, NOT a fan-out), so the vector is extended, not
            // pushed as one node.
            out.extend(self.parse_statement_vec()?);
        }
    }

    /// Parse one statement, which may expand to several sibling nodes.
    /// Every non-watch form yields exactly one node; a multi-service
    /// `watch <a>, <b>.` yields one `when` per service, in written
    /// order (reactive-spec §2 row 3). A single-service watch and the
    /// `on change of` form yield their one bare `When`.
    fn parse_statement_vec(&mut self) -> Result<Vec<Expr>, String> {
        if !self.at_statement_start() {
            return Ok(vec![self.parse_describe()?]);
        }
        let watch = match self.peek() {
            Some(Token::Name(w)) if self.at_watch_phrase_start(w) => Some(w.clone()),
            _ => None,
        };
        match watch.as_deref() {
            Some("watch") => {
                self.bump();
                self.parse_watch()
            }
            Some("on") => {
                // `on change of`: consume the three-word phrase.
                self.bump();
                self.bump();
                self.bump();
                self.parse_on_change_of()
            }
            _ => Ok(vec![self.parse_statement()?]),
        }
    }

    fn parse_statement(&mut self) -> Result<Expr, String> {
        // A phrase token mid-line is prose, not a statement head (§3.6):
        // `The router notices when the ledger comes back.` is one
        // describe line, and `notices hand this to the desk.` is prose
        // too. Only line-leading phrases open statements (§1: extent by
        // phrase, and a phrase starts a line).
        if !self.at_statement_start() {
            return self.parse_describe();
        }
        match self.peek() {
            Some(Token::BringIn) => self.parse_entity(),
            Some(Token::HandThisTo) => {
                self.bump();
                let target = self.parse_target()?;
                self.expect_period()?;
                Ok(Expr::Instruct(target))
            }
            Some(Token::When) => {
                self.bump();
                let condition = self.parse_condition()?;
                self.skip_newlines();
                let body = self.parse_body(Scope::Block)?;
                Ok(Expr::When(condition, body))
            }
            Some(Token::AtTheSameTime) => {
                self.bump();
                self.skip_newlines();
                let mut branches = Vec::new();
                while !self.closes(Scope::Branches) {
                    self.skip_newlines(); // else parse_statement sees Newline → Describe("") consumes nothing → infinite loop
                    // Spec v2 states no branch delimiter (§3.4), so each
                    // statement is one branch: the worked example's two
                    // `hand this to` lines become two branches, matching
                    // `exampleHarness`'s `together [[...], [...]]`.
                    // A multi-watch branch keeps its sibling whens in ONE
                    // branch (sequential whens), not spread across
                    // branches — that would invent a fan-out.
                    branches.push(self.parse_statement_vec()?);
                }
                Ok(Expr::Together(branches))
            }
            Some(Token::CallThis) => {
                self.bump();
                let name = self.parse_name("binding name")?;
                self.expect_period()?;
                Ok(Expr::Bind(name))
            }
            Some(Token::Recall) => {
                self.bump();
                let name = self.parse_name("recalled name")?;
                self.expect_period()?;
                Ok(Expr::Recall(name))
            }
            Some(Token::Require) => {
                self.bump();
                let services = self.parse_service_list()?;
                self.expect_period()?;
                Ok(Expr::Require(services))
            }
            Some(Token::Resource(r)) => {
                let r = r.clone();
                self.bump();
                self.expect_period()?;
                Ok(Expr::Resource(r))
            }
            _ => self.parse_describe(),
        }
    }

    fn parse_entity(&mut self) -> Result<Expr, String> {
        self.bump(); // bring in
        let name = self.parse_name("entity name")?;
        let kind = match self.bump() {
            Some(Token::Kind(k)) => *k,
            other => return Err(format!("expected a kind after `,`, found {other:?}")),
        };
        let realm = match self.peek() {
            Some(Token::Name(w)) if w == "in" => {
                self.bump();
                Some(self.parse_name("realm")?)
            }
            _ => None,
        };
        self.expect_period()?;
        let body = self.parse_body(Scope::Entity)?;
        Ok(Expr::Entity(Entity {
            name,
            kind,
            realm,
            body,
        }))
    }

    /// `watch <service>.` / `watch <a>, <b>.` / `watch <s> until <cond>.`
    /// (reactive-spec §2). Each service yields one `when` node, in
    /// written order, each with its own verbatim copy of the body.
    /// `until <cond>` conjoins the presence predicate with the condition
    /// as a one-line spelling of nested `when`s (§3.3: no `else`, no
    /// `loop`). The watch form declares nothing: it observes, so it
    /// never emits a `require` node (§2 "Observes, never declares").
    ///
    /// Returns the sibling `when` nodes: one per watched service, in
    /// written order (see the comment at the return site).
    fn parse_watch(&mut self) -> Result<Vec<Expr>, String> {
        let first = self.parse_name("watched service")?;
        let until = match self.peek() {
            Some(Token::Name(w)) if w == "until" => {
                self.bump();
                Some(self.parse_condition()?)
            }
            _ => None,
        };
        self.expect_period()?;
        let mut services = vec![first];
        if until.is_none() {
            // Comma-separated names (`watch <a>, <b>.`) are the same
            // shape `require <a>, <b>.` uses; a bare separator word is
            // dropped by `parse_name`. `expect_period` consumes the
            // statement's own `Newline`, so the slurp must skip it: the
            // remaining `Name`s on FOLLOWING lines would otherwise be
            // slurped as extra watched services (or, with the loop
            // skipped, the body's first line would start at a `Period`
            // and yield a stray `Describe(".")`).
            self.skip_newlines();
            while matches!(self.peek(), Some(Token::Name(_))) {
                services.push(self.parse_name("watched service")?);
                self.skip_newlines();
            }
        }
        self.skip_newlines();
        let body = self.parse_body(Scope::Watch)?;
        if body.is_empty() {
            return Err("`watch` with empty body".into());
        }
        // One `when` node per watched service, in written order, each
        // carrying its own verbatim copy of the body. The nodes are
        // SIBLINGS in the enclosing body (reactive-spec §2 row 3), not a
        // fan-out wrapped in `Together` — §5c never states join
        // semantics for watch, so inventing a parallel node would assert
        // concurrency the spec does not give. A one-service watch is a
        // single bare `when` statement ("placement is a statement in E's
        // body, exactly like `require`", §5's compiled form shows a bare
        // `.when` there).
        Ok(services
            .into_iter()
            .map(|service| {
                let condition = match &until {
                    Some(cond) => format!("{service} is present and {cond}"),
                    None => format!("{service} is present"),
                };
                Expr::When(condition, body.clone())
            })
            .collect())
    }

    /// `on change of <service>[ appearing | disappearing].`
    /// (reactive-spec §2) → one `when` node with the epoch predicate, or
    /// the presence / absence predicate for the `appearing` /
    /// `disappearing` tails. Body verbatim, exactly once. Returns the
    /// one `when` node as a single-element vector so the caller's
    /// statement path is uniform.
    fn parse_on_change_of(&mut self) -> Result<Vec<Expr>, String> {
        let service = self.parse_name("watched service")?;
        let tail = match self.peek() {
            Some(Token::Name(w)) if w == "appearing" => {
                self.bump();
                Some("appearing")
            }
            Some(Token::Name(w)) if w == "disappearing" => {
                self.bump();
                Some("disappearing")
            }
            _ => None,
        };
        self.expect_period()?;
        self.skip_newlines();
        let body = self.parse_body(Scope::Watch)?;
        if body.is_empty() {
            return Err("`on change of` with empty body".into());
        }
        let condition = match tail {
            Some("appearing") => format!("{service} is present"),
            Some("disappearing") => format!("{service} is absent"),
            _ => format!("{service} changed epoch"),
        };
        Ok(vec![Expr::When(condition, body)])
    }

    /// `hand this to <name>.` and the human-kind form
    /// `hand this to a human.` (§3.2: `a human` is a kind spelling, so
    /// the target resolves to a kind rather than a declared name).
    fn parse_target(&mut self) -> Result<String, String> {
        match self.bump() {
            Some(Token::Name(n)) => Ok(n.clone()),
            Some(Token::Kind(Kind::Human)) => Ok("a human".to_string()),
            other => Err(format!("expected dispatch target, found {other:?}")),
        }
    }

    /// `when` condition is everything to end-of-line (§3.3), re-joined
    /// from its tokens with single spaces.
    fn parse_condition(&mut self) -> Result<String, String> {
        let mut words: Vec<String> = Vec::new();
        loop {
            match self.peek() {
                None | Some(Token::Newline) | Some(Token::Period) => break,
                Some(Token::Name(n)) => {
                    words.push(n.clone());
                    self.pos += 1;
                }
                other => return Err(format!("unexpected token in condition: {other:?}")),
            }
        }
        if words.is_empty() {
            return Err("`when` with empty condition".into());
        }
        Ok(words.join(" "))
    }

    fn parse_service_list(&mut self) -> Result<Vec<String>, String> {
        let mut services = vec![self.parse_name("service name")?];
        loop {
            if matches!(
                self.peek(),
                Some(Token::Period) | None | Some(Token::Newline)
            ) {
                break;
            }
            if matches!(self.peek(), Some(Token::Name(_))) {
                services.push(self.parse_name("service name")?);
            } else {
                break;
            }
        }
        Ok(services)
    }

    /// Prose: one line of non-keyword text (§3.6, §7). A resource inside
    /// the sentence keeps its plain reading in the prompt (§5(a)).
    /// Inline sentence periods end a sentence, not the line: the
    /// describe node covers the whole line and its text keeps the
    /// periods as written.
    fn parse_describe(&mut self) -> Result<Expr, String> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None | Some(Token::Newline) => break,
                Some(Token::Period) => {
                    self.pos += 1;
                    // Periods attach to the preceding word (`them.`), never
                    // spaced (`them .`); the following word adds the space.
                    text.push('.');
                }
                Some(Token::Name(n)) => {
                    // Words space-separate always (after `.` too: `them. It`).
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(n);
                    self.pos += 1;
                }
                Some(Token::Resource(r)) => {
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(&resource_text(r));
                    self.pos += 1;
                }
                // A phrase token that is not at a statement start is an
                // ordinary word in the sentence (§3.6: prose is one line
                // of non-keyword text, and `notices when the ledger comes
                // back.` keeps its `when`). Its surface spelling is the
                // keyword phrase itself, re-joined with single spaces
                // like every other word.
                Some(tok) if !self.at_statement_start() => {
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(&phrase_text(tok));
                    self.pos += 1;
                }
                other => return Err(format!("unexpected token in prose: {other:?}")),
            }
        }
        Ok(Expr::Describe(text))
    }

    fn parse_name(&mut self, what: &str) -> Result<String, String> {
        match self.bump() {
            // Attached commas (`router,`, `billing-specialist,`) are
            // declaration separators: strip exactly one here. Prose
            // keeps its commas via parse_describe/parse_condition.
            Some(Token::Name(n)) => Ok(n.strip_suffix(',').unwrap_or(n).to_string()),
            other => Err(format!("expected {what}, found {other:?}")),
        }
    }

    fn expect_period(&mut self) -> Result<(), String> {
        // §1: "A period terminates a statement." A line whose prose
        // carries inline sentence periods (`It uses gpt-4o. It can
        // diagnose.`) emits more than one `Period`; the remainder are
        // consumed here and the statement ends at the newline.
        while matches!(self.peek(), Some(Token::Period)) {
            self.pos += 1;
        }
        if matches!(self.peek(), Some(Token::Newline)) {
            self.pos += 1;
        }
        Ok(())
    }
}

/// Round-trip a resource back to its surface spelling for prose text.
fn resource_text(r: &Resource) -> String {
    match r {
        Resource::File(p) => format!("@file(\"{p}\")"),
        Resource::Dir(p) => format!("@dir(\"{p}\")"),
        Resource::Env(n) => format!("@env(\"{n}\")"),
        Resource::Inline(t) => format!("@inline(\"{t}\")"),
    }
}

/// Surface spelling of a phrase token that appears mid-line, where the
/// lexer's closed phrase table matched it but the parser reads it as an
/// ordinary word (§3.6). Mirrors `Lexer`'s keyword table.
fn phrase_text(tok: &Token) -> String {
    match tok {
        Token::HarnessBegins => "this harness begins as".to_string(),
        Token::HarnessEnds => "this harness ends".to_string(),
        Token::AtTheSameTime => "at the same time".to_string(),
        Token::HandThisTo => "hand this to".to_string(),
        Token::BringIn => "bring in".to_string(),
        Token::Kind(Kind::Agent) => "an agent".to_string(),
        Token::Kind(Kind::Tool) => "a tool".to_string(),
        Token::Kind(Kind::Knowledge) => "a knowledge source".to_string(),
        Token::Kind(Kind::Connector) => "a connector".to_string(),
        Token::Kind(Kind::Human) => "a human".to_string(),
        Token::CallThis => "call this".to_string(),
        Token::Require => "require".to_string(),
        Token::Recall => "recall".to_string(),
        Token::When => "when".to_string(),
        // Callers only reach this for statement-opening phrases; anything
        // else keeps the token's own spelling.
        other => format!("{other:?}"),
    }
}
