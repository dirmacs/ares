#!/usr/bin/env python3
from pathlib import Path

path = Path("/opt/ares/crates/ares-llm/src/coordinator.rs")
text = path.read_text()
if "CoordinatorConfig" in text:
    print("already patched")
    raise SystemExit(0)

DISPATCH = Path("/tmp/r42_dispatch.txt").read_text()
TESTS = Path("/tmp/r42_tests.txt").read_text()

text = text.replace(
    "use crate::client::{LLMClient, TokenUsage};",
    "use crate::capabilities::{CapabilityRequirements, ModelCapabilities};\nuse crate::client::{LLMClient, TokenUsage};",
    1,
)
text = text.replace(
    "use tokio::time::timeout;\n\n/// Configuration for tool calling coordination behavior.",
    "use tokio::time::timeout;\n\n" + DISPATCH + "\n/// Configuration for tool calling coordination behavior.",
    1,
)
text = text.replace(
    "    use ares_types::types::{Result, ToolCall, ToolDefinition};",
    "    use crate::capabilities::ModelCapabilities;\n    use ares_types::types::{Result, ToolCall, ToolDefinition};",
    1,
)
marker = "    fn test_message_to_role_content() {"
text = text.replace(marker, TESTS + marker, 1)
text = text.replace("    #[test]\n\n    fn dispatch_endpoint", "    fn dispatch_endpoint", 1)
text = text.replace(
    "            fallback_chain: vec![\"ollama\".to_string(), \"openai\".to_string()],",
    "            fallback_chain: vec![],",
    1,
)
text = text.replace(
    "    fn test_message_to_role_content() {",
    "    #[test]\n    fn test_message_to_role_content() {",
    1,
)
path.write_text(text)
print("patched", len(text.splitlines()), "lines")
