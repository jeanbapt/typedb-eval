use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

pub struct McpHttpClient {
    client: reqwest::Client,
    base_url: String,
    session_id: Option<String>,
    request_id: u64,
}

impl McpHttpClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            session_id: None,
            request_id: 0,
        }
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "typedb-eval-runner", "version": "0.1.0" }
                }),
            )
            .await?;
        let _ = result;
        self.rpc(
            "notifications/initialized",
            json!({}),
        )
        .await
        .ok();
        Ok(())
    }

    pub async fn list_tools(&mut self) -> Result<Vec<String>> {
        let result = self.rpc("tools/list", json!({})).await?;
        let names = result
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tool| tool.get("name").and_then(|n| n.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Ok(names)
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolResult> {
        let result = self
            .rpc(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments
                }),
            )
            .await?;
        serde_json::from_value(result).context("parse MCP tool result")
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;

        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let url = format!("{}/mcp", self.base_url);
        let mut req = self
            .client
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(session) = &self.session_id {
            req = req.header("Mcp-Session-Id", session);
        }

        let resp = req.send().await.context("MCP HTTP request failed")?;
        if let Some(session) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            self.session_id = Some(session.to_string());
        }

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("MCP HTTP {status}: {text}"));
        }

        parse_mcp_response(&text, id)
    }
}

fn parse_mcp_response(text: &str, id: u64) -> Result<Value> {
    // Plain JSON (some MCP servers)
    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        if let Some(err) = parsed.get("error") {
            return Err(anyhow!("MCP error: {err}"));
        }
        if let Some(result) = parsed.get("result") {
            return Ok(result.clone());
        }
    }

    // Streamable HTTP (SSE): event: message / data: {...}
    for line in text.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let parsed: Value =
            serde_json::from_str(payload).context("invalid MCP SSE JSON payload")?;
        if parsed.get("id").and_then(|v| v.as_u64()) != Some(id) {
            continue;
        }
        if let Some(err) = parsed.get("error") {
            return Err(anyhow!("MCP error: {err}"));
        }
        return parsed
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP SSE response missing result: {payload}"));
    }

    Err(anyhow!("MCP response missing result for id {id}: {text}"))
}

pub fn tool_text(result: &McpToolResult) -> String {
    result
        .content
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
