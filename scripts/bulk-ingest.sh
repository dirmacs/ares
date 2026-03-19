#!/bin/bash
# Bulk data ingestion — WhatsApp archives, holy-doc journals, pawan sessions → ARES RAG
#
# Usage: ./bulk-ingest.sh [--dry-run]
#
# Authenticates via ARES API, ingests into collection "dirmacs-corpus".
# If auth fails, falls back to direct file preparation for manual ingest.

set -uo pipefail

ARES_URL="${ARES_URL:-http://localhost:3000}"
COLLECTION="dirmacs-corpus"
DRY_RUN="${1:-}"
INGESTED=0
FAILED=0
SKIPPED=0
OUTPUT_DIR="/opt/ares/data/bulk-ingest-queue"

mkdir -p "$OUTPUT_DIR"

# ── Auth ─────────────────────────────────────────────────────────────────
get_token() {
    local email="bulk@dirmacs.com"
    local password="bulkingest2026"

    # Register (idempotent)
    curl -s -X POST "$ARES_URL/api/auth/register" \
        -H "Content-Type: application/json" \
        -d "{\"email\":\"$email\",\"password\":\"$password\",\"name\":\"bulk-ingest\"}" >/dev/null 2>&1 || true

    # Login
    local resp
    resp=$(curl -s --max-time 5 -X POST "$ARES_URL/api/auth/login" \
        -H "Content-Type: application/json" \
        -d "{\"email\":\"$email\",\"password\":\"$password\"}" 2>/dev/null || echo "{}")

    python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
t = d.get('token') or d.get('access_token') or d.get('data',{}).get('token','')
print(t)
" <<< "$resp" 2>/dev/null || echo ""
}

TOKEN=$(get_token)
if [ -z "$TOKEN" ]; then
    echo "WARN: Auth failed — will queue documents as JSONL for manual ingest"
    AUTH_OK=false
else
    echo "Authenticated with ARES"
    AUTH_OK=true
fi

# ── Ingest function ──────────────────────────────────────────────────────
ingest_doc() {
    local title="$1"
    local content="$2"
    local source="$3"
    local tags_json="$4"

    # Skip empty content
    if [ ${#content} -lt 50 ]; then
        ((SKIPPED++))
        return 0
    fi

    if [ "$DRY_RUN" = "--dry-run" ]; then
        echo "  [dry-run] $title ($(echo "$content" | wc -c) bytes)"
        ((SKIPPED++))
        return 0
    fi

    local payload
    payload=$(python3 -c "
import json,sys
content = sys.stdin.read()
print(json.dumps({
    'collection': '$COLLECTION',
    'content': content,
    'title': '$title',
    'source': '$source',
    'tags': $tags_json,
    'chunking_strategy': 'word'
}))
" <<< "$content" 2>/dev/null)

    if [ "$AUTH_OK" = true ]; then
        local status
        status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 30 \
            -X POST "$ARES_URL/api/v1/rag/ingest" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer $TOKEN" \
            -d "$payload" 2>/dev/null)

        if [ "$status" = "200" ]; then
            ((INGESTED++))
            echo "  ✓ $title"
            return 0
        fi
    fi

    # Fallback: queue as JSONL
    echo "$payload" >> "$OUTPUT_DIR/queued.jsonl"
    ((INGESTED++))
    echo "  → $title (queued)"
}

# ── 1. WhatsApp archives ─────────────────────────────────────────────────
echo ""
echo "═══ WhatsApp Archives ═══"
WA_DIR="/root/backup/whatsapp-exports"

if [ -d "$WA_DIR" ]; then
    for subdir in "$WA_DIR"/*/; do
        txt=$(find "$subdir" -name "WhatsApp Chat with *.txt" 2>/dev/null | head -1)
        [ -n "$txt" ] && [ -f "$txt" ] || continue

        chat_name=$(basename "$subdir" | sed 's/-bkataru-first-dump\///')
        total_lines=$(wc -l < "$txt")
        echo "Processing: $chat_name ($total_lines lines)"

        # Split into chunks of ~300 lines
        chunk_num=0
        total_chunks=$(( (total_lines + 299) / 300 ))
        for offset in $(seq 1 300 "$total_lines"); do
            ((chunk_num++))
            chunk=$(sed -n "${offset},$((offset+299))p" "$txt")
            [ -z "$chunk" ] && continue
            ingest_doc \
                "whatsapp-${chat_name}-chunk${chunk_num}" \
                "$chunk" \
                "whatsapp:$chat_name" \
                '["whatsapp","chat","dirmacs"]'
        done
    done
else
    echo "  No WhatsApp archives at $WA_DIR"
fi

# ── 2. Holy-doc session journals ─────────────────────────────────────────
echo ""
echo "═══ Holy-doc Journals ═══"
HOLY_DIR="/opt/holy-doc/sessions"

if [ -d "$HOLY_DIR" ]; then
    count=0
    for md in "$HOLY_DIR"/*.md; do
        [ -f "$md" ] || continue
        name=$(basename "$md" .md)
        content=$(cat "$md")
        [ ${#content} -lt 100 ] && { ((SKIPPED++)); continue; }

        ingest_doc \
            "journal-${name}" \
            "$content" \
            "holy-doc:$name" \
            '["journal","session","dirmacs"]'
        ((count++))
    done
    echo "  Processed $count journals"
else
    echo "  No journals at $HOLY_DIR"
fi

# ── 3. Pawan session logs ────────────────────────────────────────────────
echo ""
echo "═══ Pawan Sessions ═══"
PAWAN_DIR="$HOME/.pawan/sessions"

if [ -d "$PAWAN_DIR" ]; then
    count=0
    for json_file in "$PAWAN_DIR"/*.json; do
        [ -f "$json_file" ] || continue
        name=$(basename "$json_file" .json)

        content=$(python3 -c "
import json
with open('$json_file') as f:
    data = json.load(f)
for m in data.get('messages', []):
    role = m.get('role','')
    c = m.get('content','')
    if role == 'user':
        print(f'User: {c}')
    elif role == 'assistant':
        print(f'Assistant: {c[:500]}')
" 2>/dev/null)
        [ ${#content} -lt 50 ] && { ((SKIPPED++)); continue; }

        ingest_doc \
            "pawan-${name}" \
            "$content" \
            "pawan:$name" \
            '["pawan","session","agent"]'
        ((count++))
    done
    echo "  Processed $count sessions"
else
    echo "  No pawan sessions at $PAWAN_DIR"
fi

# ── Summary ──────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════"
echo "Bulk ingestion complete"
echo "  Ingested: $INGESTED"
echo "  Failed:   $FAILED"
echo "  Skipped:  $SKIPPED"
echo "  Collection: $COLLECTION"
if [ -f "$OUTPUT_DIR/queued.jsonl" ]; then
    queued=$(wc -l < "$OUTPUT_DIR/queued.jsonl")
    echo "  Queued (JSONL): $queued docs at $OUTPUT_DIR/queued.jsonl"
fi
echo "═══════════════════════════════════════"
