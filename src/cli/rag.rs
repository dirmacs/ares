use clap::{Args, Subcommand, ValueEnum};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    path::{Path, PathBuf},
};

#[derive(Subcommand, Debug)]
pub enum RagCommands {
    /// Recursively ingest local text documents into an ARES RAG collection.
    IngestDir(RagIngestDirArgs),
    /// Search an ARES RAG collection.
    Search(RagSearchArgs),
}

#[derive(Args, Debug)]
pub struct RagIngestDirArgs {
    /// ARES server base URL.
    #[arg(long, default_value = "http://localhost:3000")]
    pub host: String,

    /// RAG collection name to ingest into.
    #[arg(long)]
    pub collection: String,

    /// Directory containing documents to ingest.
    #[arg(long)]
    pub docs_path: PathBuf,

    /// Login email/user. Required unless --token is provided.
    #[arg(long)]
    pub user: Option<String>,

    /// Login password. Required unless --token is provided.
    #[arg(long)]
    pub password: Option<String>,

    /// Bearer token. When provided, login is skipped.
    #[arg(long)]
    pub token: Option<String>,

    /// Chunking strategy to request from the server.
    #[arg(long, value_enum, default_value_t = ChunkingStrategyArg::Word)]
    pub chunking_strategy: ChunkingStrategyArg,

    /// Tag to attach to ingested documents. Repeat for multiple tags.
    #[arg(long)]
    pub tag: Vec<String>,

    /// Show the files that would be ingested without sending API requests.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct RagSearchArgs {
    /// ARES server base URL.
    #[arg(long, default_value = "http://localhost:3000")]
    pub host: String,

    /// RAG collection name to search.
    #[arg(long)]
    pub collection: String,

    /// Search query.
    #[arg(long)]
    pub query: String,

    /// Login email/user. Required unless --token is provided.
    #[arg(long)]
    pub user: Option<String>,

    /// Login password. Required unless --token is provided.
    #[arg(long)]
    pub password: Option<String>,

    /// Bearer token. When provided, login is skipped.
    #[arg(long)]
    pub token: Option<String>,

    /// Maximum number of results to return.
    #[arg(long, default_value_t = 10)]
    pub top_k: usize,

    /// Search strategy to request from the server.
    #[arg(long, value_enum, default_value_t = SearchStrategyArg::Semantic)]
    pub strategy: SearchStrategyArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ChunkingStrategyArg {
    Word,
    Semantic,
    Character,
}

impl fmt::Display for ChunkingStrategyArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Word => f.write_str("word"),
            Self::Semantic => f.write_str("semantic"),
            Self::Character => f.write_str("character"),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SearchStrategyArg {
    Semantic,
    Bm25,
    Fuzzy,
    Hybrid,
}

impl fmt::Display for SearchStrategyArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Semantic => f.write_str("semantic"),
            Self::Bm25 => f.write_str("bm25"),
            Self::Fuzzy => f.write_str("fuzzy"),
            Self::Hybrid => f.write_str("hybrid"),
        }
    }
}

#[derive(Debug, Serialize)]
struct RagIngestPayload<'a> {
    collection: &'a str,
    content: &'a str,
    title: &'a str,
    source: &'a str,
    tags: &'a [String],
    chunking_strategy: String,
}

#[derive(Debug, Deserialize)]
struct RagIngestResponse {
    chunks_created: usize,
}

#[derive(Debug, Serialize)]
struct RagSearchPayload<'a> {
    collection: &'a str,
    query: &'a str,
    limit: usize,
    strategy: String,
}

#[derive(Debug, Deserialize)]
struct TokenPayload {
    access_token: Option<String>,
    token: Option<String>,
    data: Option<TokenData>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    token: Option<String>,
}

#[derive(Debug)]
struct Document {
    path: PathBuf,
    title: String,
    content: String,
}

pub async fn run(command: RagCommands) -> Result<(), Box<dyn Error>> {
    match command {
        RagCommands::IngestDir(args) => ingest_dir(args).await,
        RagCommands::Search(args) => search(args).await,
    }
}

async fn ingest_dir(args: RagIngestDirArgs) -> Result<(), Box<dyn Error>> {
    let docs = discover_documents(&args.docs_path)?;
    if docs.is_empty() {
        return Err(format!(
            "no ingestible text documents found under {}",
            args.docs_path.display()
        )
        .into());
    }

    if args.dry_run {
        for doc in &docs {
            println!(
                "{}\t{}\t{} bytes",
                doc.path.display(),
                doc.title,
                doc.content.len()
            );
        }
        println!("dry_run=true documents={}", docs.len());
        return Ok(());
    }

    let client = reqwest::Client::new();
    let token = bearer_token(
        &client,
        &args.host,
        args.token.as_deref(),
        args.user.as_deref(),
        args.password.as_deref(),
    )
    .await?;
    let endpoint = api_url(&args.host, "/api/rag/ingest");
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut chunks = 0usize;

    for doc in &docs {
        let source = doc.path.to_string_lossy();
        let payload = RagIngestPayload {
            collection: &args.collection,
            content: &doc.content,
            title: &doc.title,
            source: &source,
            tags: &args.tag,
            chunking_strategy: args.chunking_strategy.to_string(),
        };

        let response = client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await?;

        if response.status() == StatusCode::OK {
            let body: RagIngestResponse = response.json().await?;
            chunks += body.chunks_created;
            succeeded += 1;
            println!(
                "ingested\t{}\t{} chunks",
                doc.path.display(),
                body.chunks_created
            );
        } else {
            failed += 1;
            eprintln!("failed\t{}\tHTTP {}", doc.path.display(), response.status());
        }
    }

    println!(
        "summary\tdocuments={} succeeded={} failed={} chunks={}",
        docs.len(),
        succeeded,
        failed,
        chunks
    );

    if failed == 0 {
        Ok(())
    } else {
        Err(format!("{failed} document(s) failed ingestion").into())
    }
}

async fn search(args: RagSearchArgs) -> Result<(), Box<dyn Error>> {
    let client = reqwest::Client::new();
    let token = bearer_token(
        &client,
        &args.host,
        args.token.as_deref(),
        args.user.as_deref(),
        args.password.as_deref(),
    )
    .await?;
    let response = client
        .post(api_url(&args.host, "/api/rag/search"))
        .bearer_auth(token)
        .json(&RagSearchPayload {
            collection: &args.collection,
            query: &args.query,
            limit: args.top_k,
            strategy: args.strategy.to_string(),
        })
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        eprintln!("search failed: HTTP {status}");
        if !body.is_empty() {
            eprintln!("{body}");
        }
        return Err("RAG search failed".into());
    }

    let body: serde_json::Value = response.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn bearer_token(
    client: &reqwest::Client,
    host: &str,
    token: Option<&str>,
    user: Option<&str>,
    password: Option<&str>,
) -> Result<String, Box<dyn Error>> {
    if let Some(token) = token {
        if !token.is_empty() {
            return Ok(token.to_owned());
        }
    }

    let user = user.ok_or("--user is required unless --token is provided")?;
    let password = password.ok_or("--password is required unless --token is provided")?;
    let response = client
        .post(api_url(host, "/api/auth/login"))
        .json(&json!({ "email": user, "password": password }))
        .send()
        .await?;

    if !response.status().is_success() {
        eprintln!("login failed: HTTP {}", response.status());
        return Err("ARES login failed".into());
    }

    let payload: TokenPayload = response.json().await?;
    payload
        .access_token
        .or(payload.token)
        .or_else(|| payload.data.and_then(|data| data.token))
        .ok_or_else(|| "login response did not include an access token".into())
}

fn discover_documents(root: &Path) -> Result<Vec<Document>, Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("docs path is not a directory: {}", root.display()).into());
    }

    let mut docs = Vec::new();
    visit_dir(root, &mut docs)?;
    docs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(docs)
}

fn visit_dir(dir: &Path, docs: &mut Vec<Document>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit_dir(&path, docs)?;
        } else if file_type.is_file() && is_supported_document(&path) {
            match fs::read_to_string(&path) {
                Ok(content) if !content.trim().is_empty() => docs.push(Document {
                    title: document_title(&path, &content),
                    content,
                    path,
                }),
                Ok(_) => {}
                Err(err) => eprintln!("skipping unreadable document {}: {err}", path.display()),
            }
        }
    }
    Ok(())
}

fn is_supported_document(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "txt" | "text" | "json" | "jsonl")
    )
}

fn document_title(path: &Path, content: &str) -> String {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or("document")
                .replace(['-', '_'], " ")
        })
}

fn api_url(host: &str, path: &str) -> String {
    format!("{}{}", host.trim_end_matches('/'), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_trims_duplicate_slashes() {
        assert_eq!(
            api_url("http://localhost:3000/", "/api/rag/search"),
            "http://localhost:3000/api/rag/search"
        );
    }

    #[test]
    fn document_title_prefers_first_heading() {
        let title = document_title(Path::new("docs/my-file.md"), "intro\n# Real Title\nbody");
        assert_eq!(title, "Real Title");
    }

    #[test]
    fn document_title_falls_back_to_file_stem() {
        let title = document_title(Path::new("docs/my_file.md"), "body");
        assert_eq!(title, "my file");
    }

    #[test]
    fn supported_documents_match_text_extensions() {
        assert!(is_supported_document(Path::new("a.md")));
        assert!(is_supported_document(Path::new("a.JSONL")));
        assert!(!is_supported_document(Path::new("a.png")));
    }
}
