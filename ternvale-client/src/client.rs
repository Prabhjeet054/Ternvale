//! Tokio Unix JSON-RPC client.

use std::path::Path;
use std::time::Duration;

use ternvale_api::{
    parse_request, Event, HandshakeParams, HandshakeResult, JsonRpcRequest, JsonRpcResponse,
    Method, API_VERSION, JSONRPC_VERSION,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::ClientError;

const MAX_LINE: usize = 64 * 1024;

/// Async JSON-RPC client. Call [`Self::handshake`] before other methods.
pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: i64,
    handshook: bool,
}

impl Client {
    /// Connect to a Unix socket (hub `api.sock` or a per-VM socket).
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(path = %path.as_ref().display()))]
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let path_ref = path.as_ref();
        let stream = UnixStream::connect(path_ref).await.map_err(|source| {
            tracing::warn!(
                target: "ternvale::api",
                path = %path_ref.display(),
                error = %source,
                "client connect failed"
            );
            ClientError::Connect {
                path: path_ref.display().to_string(),
                source,
            }
        })?;
        let (read, write) = stream.into_split();
        tracing::info!(
            target: "ternvale::api",
            path = %path_ref.display(),
            "client connected"
        );
        Ok(Self {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
            handshook: false,
        })
    }

    /// JSON-RPC `handshake` with [`API_VERSION`].
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub async fn handshake(&mut self) -> Result<HandshakeResult, ClientError> {
        let params = HandshakeParams {
            api_version: API_VERSION,
        };
        let value = self
            .call_raw(Method::Handshake, Some(serde_json::to_value(params)?))
            .await?;
        let result: HandshakeResult = serde_json::from_value(value)?;
        self.handshook = result.ok;
        tracing::info!(
            target: "ternvale::api",
            api_version = result.api_version,
            "client handshake ok"
        );
        Ok(result)
    }

    /// Call `method` after a successful handshake (`handshake` itself is exempt).
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(method = method.as_str()))]
    pub async fn call(
        &mut self,
        method: Method,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ClientError> {
        if method != Method::Handshake && !self.handshook {
            return Err(ClientError::NeedHandshake(method.as_str()));
        }
        self.call_raw(method, params).await
    }

    /// `subscribe`; the connection then yields events via [`Self::next_event`].
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub async fn subscribe(&mut self, events: &[&str]) -> Result<serde_json::Value, ClientError> {
        self.call(
            Method::Subscribe,
            Some(serde_json::json!({ "events": events })),
        )
        .await
    }

    /// Read one JSON-RPC notification (`method = event`).
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub async fn next_event(&mut self) -> Result<Event, ClientError> {
        let line = self.read_line().await?;
        let request = parse_request(&line)?;
        if request.method != "event" {
            return Err(ClientError::Api(ternvale_api::ApiError::InvalidRequest(
                format!("expected event, got {}", request.method),
            )));
        }
        let params = request.params.unwrap_or(serde_json::Value::Null);
        Ok(serde_json::from_value(params)?)
    }

    async fn call_raw(
        &mut self,
        method: Method,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.as_str().to_string(),
            params,
            id: Some(serde_json::Value::from(id)),
        };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        tracing::debug!(
            target: "ternvale::api",
            method = method.as_str(),
            id,
            "client send"
        );
        self.writer.write_all(line.as_bytes()).await?;
        let reply = self.read_line().await?;
        let response: JsonRpcResponse = serde_json::from_str(&reply)?;
        if let Some(error) = response.error {
            tracing::warn!(
                target: "ternvale::api",
                code = error.code,
                message = %error.message,
                "client rpc error"
            );
            return Err(ClientError::Rpc {
                code: error.code,
                message: error.message,
            });
        }
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    async fn read_line(&mut self) -> Result<String, ClientError> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(30), self.reader.read_line(&mut line))
            .await
            .map_err(|_| {
                ClientError::Read(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "json-rpc read timed out",
                ))
            })??;
        if n == 0 {
            return Err(ClientError::Empty);
        }
        if line.len() > MAX_LINE {
            return Err(ClientError::Read(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "json-rpc line too long",
            )));
        }
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use super::Client;
    use crate::ClientError;
    use ternvale_api::{HandshakeResult, Method};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "tv-client-{tag}-{}-{}",
                std::process::id(),
                tag.len()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("dir");
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn handshake_then_list() {
        let dir = Scratch::new("hs");
        let path = dir.0.join("api.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("c"));
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).expect("hs");
            assert!(line.contains("handshake"));
            writer
                .write_all(
                    br#"{"jsonrpc":"2.0","result":{"ok":true,"api_version":1,"min_api_version":1,"max_api_version":1},"id":1}
"#,
                )
                .expect("w");
            line.clear();
            reader.read_line(&mut line).expect("list");
            assert!(line.contains("list"));
            writer
                .write_all(
                    br#"{"jsonrpc":"2.0","result":{"ok":true,"vms":[]},"id":2}
"#,
                )
                .expect("w2");
        });
        let mut client = Client::connect(&path).await.expect("connect");
        let hs: HandshakeResult = client.handshake().await.expect("hs");
        assert_eq!(hs.api_version, 1);
        let listed = client.call(Method::List, None).await.expect("list");
        assert_eq!(listed["ok"], true);
        server.join().expect("server");
    }

    #[tokio::test]
    async fn refuses_calls_before_handshake() {
        let dir = Scratch::new("nohs");
        let path = dir.0.join("api.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let _server = std::thread::spawn(move || {
            let _ = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        let mut client = Client::connect(&path).await.expect("connect");
        match client.call(Method::List, None).await {
            Err(ClientError::NeedHandshake("list")) => {}
            other => panic!("{other:?}"),
        }
    }
}
