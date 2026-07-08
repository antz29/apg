use anyhow::{bail, Context, Result};
use lsp_types::{
    CallHierarchyItem, CallHierarchyOutgoingCall, ClientCapabilities,
    DocumentSymbol, DocumentSymbolClientCapabilities, InitializeParams,
    InitializeResult, SymbolInformation, TextDocumentClientCapabilities, Uri,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

pub struct LspClient {
    stdin: tokio::io::BufWriter<tokio::process::ChildStdin>,
    stdout: BufReader<tokio::process::ChildStdout>,
    _child: Child,
    next_id: u64,
}

impl LspClient {
    pub async fn spawn(cmd: &str) -> Result<Self> {
        let mut child = Command::new(cmd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn LSP server: `{cmd}`. \
                     Set JDTLS_CMD env var or ensure `jdtls` is on PATH."
                )
            })?;

        let stdin = tokio::io::BufWriter::new(child.stdin.take().unwrap());
        let stdout = BufReader::new(child.stdout.take().unwrap());

        Ok(Self { stdin, stdout, _child: child, next_id: 0 })
    }

    pub async fn initialize(&mut self, root_uri: &Uri) -> Result<InitializeResult> {
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            client_info: Some(lsp_types::ClientInfo {
                name: "java_apg".into(),
                version: Some("0.1.0".into()),
            }),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    document_symbol: Some(DocumentSymbolClientCapabilities {
                        hierarchical_document_symbol_support: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            #[allow(deprecated)]
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: root_uri.clone(),
                name: "project".into(),
            }]),
            ..Default::default()
        };
        self.request("initialize", &params).await
    }

    pub async fn initialized(&mut self) -> Result<()> {
        self.notify("initialized", &serde_json::json!({})).await
    }

    pub async fn did_open(&mut self, uri: &Uri, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            &serde_json::json!({
                "textDocument": {
                    "uri": uri.as_str(),
                    "languageId": "java",
                    "version": 1,
                    "text": text,
                }
            }),
        )
        .await
    }

    #[allow(deprecated)]
    pub async fn document_symbols(&mut self, uri: &Uri) -> Result<Vec<DocumentSymbol>> {
        let result: Value = self
            .request(
                "textDocument/documentSymbol",
                &serde_json::json!({
                    "textDocument": { "uri": uri.as_str() }
                }),
            )
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }
        if let Ok(symbols) = serde_json::from_value::<Vec<DocumentSymbol>>(result.clone()) {
            return Ok(symbols);
        }
        // Fallback: flat SymbolInformation[] — wrap each as a flat DocumentSymbol
        #[allow(deprecated)]
        if let Ok(flat) = serde_json::from_value::<Vec<SymbolInformation>>(result) {
            eprintln!("  warn: {} returned flat SymbolInformation[]", uri.as_str());
            return Ok(flat.into_iter().map(|s| DocumentSymbol {
                name: s.name,
                detail: None,
                kind: s.kind,
                tags: s.tags,
                deprecated: None,
                range: s.location.range,
                selection_range: s.location.range,
                children: None,
            }).collect());
        }
        Ok(Vec::new())
    }

    pub async fn outgoing_calls(
        &mut self,
        item: CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyOutgoingCall>> {
        self.request("callHierarchy/outgoingCalls", &serde_json::json!({ "item": item }))
            .await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let _: Value = self.request("shutdown", &serde_json::json!({})).await?;
        self.notify("exit", &serde_json::json!({})).await
    }

    async fn request<P: serde::Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<R> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write(&msg).await?;
        self.read_response(id).await
    }

    async fn notify(&mut self, method: &str, params: &Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write(&msg).await
    }

    async fn write(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(body.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            if n == 0 {
                bail!("LSP server closed connection");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len.parse()?;
            }
        }
        let mut buf = vec![0u8; content_length];
        self.stdout.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }

    async fn read_response<R: DeserializeOwned>(&mut self, expected_id: u64) -> Result<R> {
        loop {
            let msg = self.read().await?;
            if let Some(msg_id) = msg.get("id").and_then(|v| v.as_u64()) {
                if msg_id == expected_id {
                    if let Some(err) = msg.get("error") {
                        let code = err["code"].as_u64().unwrap_or(0);
                        let message = err["message"].as_str().unwrap_or("unknown");
                        bail!("LSP error [{code}]: {message}");
                    }
                    return Ok(serde_json::from_value(msg["result"].clone())?);
                }
            }
        }
    }
}
