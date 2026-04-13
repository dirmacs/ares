#!/usr/bin/env python3
"""
EHB Knowledge Base Ingestion Script

This script ingests all markdown documents from /opt/ehb into the ARES RAG system.
It uses the ARES API to chunk documents, generate embeddings, and store them in the vector database.

Usage:
    python3 ingest_ehb_kb.py [--host HOST] [--collection COLLECTION] [--user USER] [--password PASSWORD]

Requirements:
    - requests: pip install requests
    - ARES server running with ares-vector and local-embeddings features enabled
"""

import argparse
import os
import sys
from pathlib import Path
from typing import Optional, Tuple, List
import requests

# Default configuration
DEFAULT_HOST = "http://localhost:3000"
DEFAULT_COLLECTION = "ehb_knowledge_base"
DEFAULT_DOCS_PATH = "/opt/ehb"

# Colors for terminal output
class Colors:
    HEADER = '\033[95m'
    OKBLUE = '\033[94m'
    OKCYAN = '\033[96m'
    OKGREEN = '\033[92m'
    WARNING = '\033[93m'
    FAIL = '\033[91m'
    ENDC = '\033[0m'
    BOLD = '\033[1m'
    UNDERLINE = '\033[4m'


def find_markdown_files(docs_path: str) -> List[Path]:
    """Recursively find all markdown files in a directory."""
    docs_dir = Path(docs_path)
    if not docs_dir.exists():
        print(f"{Colors.FAIL}Error: Documentation path does not exist: {docs_path}{Colors.ENDC}")
        sys.exit(1)
    
    markdown_files = list(docs_dir.rglob("*.md"))
    print(f"{Colors.OKGREEN}Found {len(markdown_files)} markdown files in {docs_path}{Colors.ENDC}")
    return markdown_files


def read_markdown_file(file_path: Path) -> Optional[Tuple[str, str]]:
    """Read a markdown file and extract title and content."""
    try:
        content = file_path.read_text(encoding='utf-8')
        
        if not content.strip():
            return None
        
        # Extract title from first # heading or use filename
        for line in content.split('\n'):
            if line.startswith('# '):
                title = line[2:].strip()
                break
        else:
            title = file_path.stem.replace('-', ' ').replace('_', ' ').title()
        
        return title, content
    except Exception as e:
        print(f"{Colors.WARNING}Warning: Could not read {file_path}: {e}{Colors.ENDC}")
        return None


def authenticate(host: str, username: str, password: str) -> Optional[str]:
    """Authenticate with ARES and return JWT token."""
    try:
        response = requests.post(
            f"{host}/auth/login",
            json={"username": username, "password": password},
            timeout=10
        )
        
        if response.status_code == 200:
            data = response.json()
            token = data.get("access_token") or data.get("token")
            if token:
                print(f"{Colors.OKGREEN}Authentication successful{Colors.ENDC}")
                return token
            else:
                print(f"{Colors.FAIL}Error: No token in response{Colors.ENDC}")
        else:
            print(f"{Colors.FAIL}Error: Authentication failed ({response.status_code}){Colors.ENDC}")
            print(response.text)
        
        return None
    except requests.exceptions.RequestException as e:
        print(f"{Colors.FAIL}Error: Connection failed: {e}{Colors.ENDC}")
        return None


def ingest_document(
    host: str,
    token: str,
    collection: str,
    title: str,
    content: str,
    source: str,
    chunking_strategy: str = "word"
) -> bool:
    """Ingest a single document into the RAG system."""
    try:
        response = requests.post(
            f"{host}/api/rag/ingest",
            headers={"Authorization": f"Bearer {token}"},
            json={
                "collection": collection,
                "title": title,
                "content": content,
                "source": source,
                "chunking_strategy": chunking_strategy,
                "tags": ["ehb", "knowledge-base"]
            },
            timeout=120  # Longer timeout for large documents
        )
        
        if response.status_code == 200:
            data = response.json()
            chunks = data.get("chunks_created", 0)
            print(f"  {Colors.OKGREEN}✓{Colors.ENDC} {title}: {chunks} chunks created")
            return True
        else:
            print(f"  {Colors.FAIL}✗{Colors.ENDC} {title}: HTTP {response.status_code}")
            print(f"     {response.text[:200]}")
            return False
            
    except requests.exceptions.RequestException as e:
        print(f"  {Colors.FAIL}✗{Colors.ENDC} {title}: {e}")
        return False


def search_documents(host: str, token: str, collection: str, query: str, top_k: int = 5) -> None:
    """Search the RAG system and display results."""
    try:
        response = requests.post(
            f"{host}/api/rag/search",
            headers={"Authorization": f"Bearer {token}"},
            json={
                "collection": collection,
                "query": query,
                "top_k": top_k,
                "strategy": "semantic"
            },
            timeout=60
        )
        
        if response.status_code == 200:
            data = response.json()
            results = data.get("results", [])
            
            print(f"\n{Colors.HEADER}Search Results for: '{query}'{Colors.ENDC}")
            print(f"Found {len(results)} results\n")
            
            for i, result in enumerate(results[:top_k], 1):
                score = result.get("score", 0)
                title = result.get("metadata", {}).get("title", "Unknown")
                content = result.get("content", "")
                preview = content[:150] + "..." if len(content) > 150 else content
                
                print(f"{Colors.OKBLUE}{i}. [{title}] (score: {score:.4f}){Colors.ENDC}")
                print(f"   {preview}\n")
        else:
            print(f"{Colors.FAIL}Search failed: HTTP {response.status_code}{Colors.ENDC}")
            print(response.text)
            
    except requests.exceptions.RequestException as e:
        print(f"{Colors.FAIL}Search error: {e}{Colors.ENDC}")


def main():
    parser = argparse.ArgumentParser(
        description="Ingest EHB Knowledge Base into ARES RAG System",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    # Interactive mode (will prompt for credentials)
    python3 ingest_ehb_kb.py
    
    # With command-line arguments
    python3 ingest_ehb_kb.py --host http://localhost:3000 --user admin --password secret
    
    # Test search after ingestion
    python3 ingest_ehb_kb.py --search "mental health assessment"
        """
    )
    
    parser.add_argument(
        "--host",
        default=DEFAULT_HOST,
        help=f"ARES server host (default: {DEFAULT_HOST})"
    )
    parser.add_argument(
        "--collection",
        default=DEFAULT_COLLECTION,
        help=f"RAG collection name (default: {DEFAULT_COLLECTION})"
    )
    parser.add_argument(
        "--docs-path",
        default=DEFAULT_DOCS_PATH,
        help=f"Path to documentation directory (default: {DEFAULT_DOCS_PATH})"
    )
    parser.add_argument(
        "--user",
        help="Username for authentication (optional, will prompt if not provided)"
    )
    parser.add_argument(
        "--password",
        help="Password for authentication (optional, will prompt if not provided)"
    )
    parser.add_argument(
        "--chunking-strategy",
        choices=["word", "semantic", "character"],
        default="word",
        help="Chunking strategy (default: word)"
    )
    parser.add_argument(
        "--search",
        metavar="QUERY",
        help="Run a search query instead of ingestion"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be ingested without actually ingesting"
    )
    
    args = parser.parse_args()
    
    print(f"\n{Colors.HEADER}{'='*60}{Colors.ENDC}")
    print(f"{Colors.HEADER}  EHB Knowledge Base Ingestion Script{Colors.ENDC}")
    print(f"{Colors.HEADER}{'='*60}{Colors.ENDC}\n")
    
    # Find markdown files
    print(f"{Colors.BOLD}Step 1: Discovering documents{Colors.ENDC}")
    markdown_files = find_markdown_files(args.docs_path)
    
    if not markdown_files:
        print(f"{Colors.FAIL}No markdown files found. Exiting.{Colors.ENDC}")
        sys.exit(1)
    
    # Authenticate
    print(f"\n{Colors.BOLD}Step 2: Authenticating with ARES{Colors.ENDC}")
    username = args.user or input("Username: ")
    
    if args.password:
        password = args.password
    else:
        import getpass
        password = getpass.getpass("Password: ")
    
    token = authenticate(args.host, username, password)
    if not token:
        print(f"{Colors.FAIL}Authentication failed. Exiting.{Colors.ENDC}")
        sys.exit(1)
    
    # Search mode
    if args.search:
        print(f"\n{Colors.BOLD}Step 3: Running search{Colors.ENDC}")
        search_documents(args.host, token, args.collection, args.search)
        return
    
    # Ingestion mode
    print(f"\n{Colors.BOLD}Step 3: Ingesting documents{Colors.ENDC}")
    print(f"Collection: {args.collection}")
    print(f"Chunking strategy: {args.chunking_strategy}")
    print(f"Dry run: {args.dry_run}\n")
    
    if args.dry_run:
        print(f"{Colors.OKCYAN}Dry run mode - showing documents to be ingested:{Colors.ENDC}")
        for file_path in markdown_files[:10]:  # Show first 10
            result = read_markdown_file(file_path)
            if result:
                title, _ = result
                print(f"  - {title} ({file_path.name})")
        if len(markdown_files) > 10:
            print(f"  ... and {len(markdown_files) - 10} more")
        return
    
    successful = 0
    failed = 0
    total_chunks = 0
    
    for i, file_path in enumerate(markdown_files, 1):
        print(f"\n[{i}/{len(markdown_files)}] Processing: {file_path.name}")
        
        result = read_markdown_file(file_path)
        if not result:
            failed += 1
            continue
        
        title, content = result
        source = file_path.stem
        
        if ingest_document(
            args.host,
            token,
            args.collection,
            title,
            content,
            source,
            args.chunking_strategy
        ):
            successful += 1
        else:
            failed += 1
    
    # Summary
    print(f"\n{Colors.HEADER}{'='*60}{Colors.ENDC}")
    print(f"{Colors.BOLD}Ingestion Summary{Colors.ENDC}")
    print(f"{Colors.HEADER}{'='*60}{Colors.ENDC}")
    print(f"Total documents: {len(markdown_files)}")
    print(f"{Colors.OKGREEN}Successful: {successful}{Colors.ENDC}")
    if failed > 0:
        print(f"{Colors.FAIL}Failed: {failed}{Colors.ENDC}")
    print(f"\n{Colors.OKGREEN}Ingestion complete!{Colors.ENDC}")
    print(f"\n{Colors.BOLD}To search the knowledge base:{Colors.ENDC}")
    print(f"  python3 {sys.argv[0]} --search 'your query here'")


if __name__ == "__main__":
    main()
