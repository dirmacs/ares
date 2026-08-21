# Loop detection & safety

ARES includes built-in safety mechanisms to prevent agents from getting stuck in infinite loops or crashing mid-execution.

## Loop detection

The `LoopDetector` monitors agent tool-calling conversations for repetitive patterns using a sliding-window hash approach.

### How it works

1. Each agent response is hashed (after whitespace normalization)
2. Hashes are stored in a sliding window (configurable size, default 10)
3. When duplicate hashes exceed a threshold, a loop is detected
4. The detector escalates through 3 tiers of intervention

### Escalation tiers

| Tier | Action | Description |
|------|--------|-------------|
| 1 | `InjectWarning` | Adds a system message warning the agent it's repeating itself |
| 2 | `ForceAlternative` | Forces the agent to take a different approach |
| 3 | `HaltAgent` | Stops the agent entirely and returns an error to the caller |

### Configuration

```rust
use ares::agents::loop_detector::{LoopDetector, LoopDetectorConfig};

let config = LoopDetectorConfig {
    window_size: 10,        // Number of recent responses to track
    threshold: 3,           // Duplicates before triggering
    min_response_length: 20, // Ignore very short responses
};

let mut detector = LoopDetector::new(config);
```

### Usage in agents

Loop detection is automatically applied during multi-turn tool-calling conversations. The `ConfigurableAgent` checks the detector after each response.

## Crash recovery

The `CheckpointManager` provides state serialization for long-running agent tasks.

### Checkpoints

```rust
use ares::agents::checkpoint::{CheckpointManager, Checkpoint};

let manager = CheckpointManager::new("/data/checkpoints");

// Save a checkpoint
let checkpoint = Checkpoint {
    session_id: "session-123".to_string(),
    step: 5,
    messages: vec![/* conversation history */],
    tool_calls: vec![/* pending tool calls */],
    partial_results: vec![/* results so far */],
    status: "in_progress".to_string(),
};
manager.save(&checkpoint)?;

// Resume from latest checkpoint
if let Some(restored) = manager.load_latest("session-123")? {
    // Continue from where we left off
}
```

### Cleanup

Old checkpoints are cleaned up automatically based on age:

```rust
// Remove checkpoints older than 24 hours
manager.cleanup(Duration::from_secs(86400))?;
```

## Emergency stop

The emergency stop is a global kill switch that immediately rejects all agent requests with HTTP 503.

```bash
# Activate emergency stop
curl -X POST http://localhost:3000/api/admin/agents/emergency-stop \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"active": true}'

# Deactivate
curl -X POST http://localhost:3000/api/admin/agents/emergency-stop \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"active": false}'
```

When active, all `/api/chat`, `/api/chat/stream`, `/v1/chat`, and agent execution endpoints return:

```json
{
  "error": "Emergency stop is active. All agent requests are suspended.",
  "code": "EMERGENCY_STOP"
}
```
