//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0).
//! Requests run concurrently; responses go through one writer task.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::tools::{self, App, TOOLS};

const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Tools for NTU CareerAxis (careeraxis.ntu.edu.sg), NTU's career portal: \
search jobs/internships and events, read full listings, look up employers, and see the user's \
bookings, bookmarks and career-advice appointments. Start with search_jobs or search_events; \
use get_job/get_event for full text. If a tool returns NEEDS_LOGIN, call careeraxis_login.";

pub async fn serve(app: App) -> Result<()> {
    let app = Arc::new(app);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            if out.write_all(line.as_bytes()).await.is_err() || out.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = out.flush().await;
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}).to_string());
                continue;
            }
        };
        let Some(id) = msg.get("id").cloned() else { continue }; // notification
        let app = app.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let method = msg["method"].as_str().unwrap_or_default();
            let reply = match handle(&app, method, &msg["params"]).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
            };
            let _ = tx.send(reply.to_string());
        });
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

async fn handle(app: &App, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOLS.iter().find(|p| **p == asked).unwrap_or(&PROTOCOLS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "careeraxis-mcp", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": TOOLS.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": {"readOnlyHint": t.name != "careeraxis_login", "openWorldHint": true},
            })).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            if tools::find(name).is_none() {
                return Err((-32602, format!("unknown tool '{name}'")));
            }
            let args = if params["arguments"].is_object() { params["arguments"].clone() } else { json!({}) };
            Ok(match tools::call(app, name, &args).await {
                Ok(v) => json!({"content": [{"type": "text", "text": v.to_string()}]}),
                Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
            })
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}
