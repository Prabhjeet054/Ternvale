//! Create → start → wait for agent → snapshot → stop (JSON-RPC hub).

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use ternvale_api::{EventKind, Method};

use crate::{Client, ClientError};

/// Inputs for [`run`].
pub struct Lifecycle {
    /// Hub socket (`api.sock`).
    pub socket: std::path::PathBuf,
    /// Library VM name.
    pub name: String,
    /// Snapshot label.
    pub snapshot: String,
    /// TOML written by `create`.
    pub config_toml: String,
    /// How long to wait for `agent-connect`.
    pub agent_timeout: Duration,
}

/// Drive one VM through create/start/agent/snapshot/stop on `lifecycle.socket`.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = lifecycle.name.as_str(), socket = %lifecycle.socket.display()))]
pub async fn run(lifecycle: &Lifecycle) -> Result<(), ClientError> {
    tracing::info!(
        target: "ternvale::api",
        vm = lifecycle.name.as_str(),
        socket = %lifecycle.socket.display(),
        "lifecycle start"
    );
    let mut client = Client::connect(Path::new(&lifecycle.socket)).await?;
    let hs = client.handshake().await?;
    if !hs.ok || hs.api_version != 1 {
        return Err(ClientError::Failed(format!(
            "handshake rejected api_version={}",
            hs.api_version
        )));
    }
    expect_ok(
        client
            .call(
                Method::Create,
                Some(serde_json::json!({
                    "name": lifecycle.name,
                    "config_toml": lifecycle.config_toml,
                })),
            )
            .await?,
        "create",
    )?;
    expect_ok(
        client
            .call(
                Method::Start,
                Some(serde_json::json!({ "name": lifecycle.name })),
            )
            .await?,
        "start",
    )?;
    expect_ok(client.subscribe(&["agent-connect"]).await?, "subscribe")?;
    wait_agent(&mut client, &lifecycle.name, lifecycle.agent_timeout).await?;
    expect_ok(
        client
            .call(
                Method::SnapshotCreate,
                Some(serde_json::json!({
                    "name": lifecycle.name,
                    "snapshot": lifecycle.snapshot,
                })),
            )
            .await?,
        "snapshot.create",
    )?;
    expect_ok(
        client
            .call(
                Method::Stop,
                Some(serde_json::json!({ "name": lifecycle.name })),
            )
            .await?,
        "stop",
    )?;
    tracing::info!(
        target: "ternvale::api",
        vm = lifecycle.name.as_str(),
        snapshot = lifecycle.snapshot.as_str(),
        "lifecycle done"
    );
    Ok(())
}

async fn wait_agent(client: &mut Client, vm: &str, timeout: Duration) -> Result<(), ClientError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ClientError::Failed(
                "timed out waiting for agent-connect".into(),
            ));
        }
        let event = tokio::time::timeout(left, client.next_event())
            .await
            .map_err(|_| ClientError::Failed("timed out waiting for agent-connect".into()))??;
        tracing::debug!(
            target: "ternvale::api",
            kind = event.kind.as_str(),
            vm = event.vm.as_str(),
            "lifecycle event"
        );
        if event.kind == EventKind::AgentConnect && event.vm == vm {
            return Ok(());
        }
    }
}

fn expect_ok(value: Value, method: &str) -> Result<Value, ClientError> {
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        tracing::warn!(target: "ternvale::api", method, error = message, "lifecycle method failed");
        return Err(ClientError::Failed(format!("{method}: {message}")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{run, Lifecycle};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("tv-life-{}", std::process::id()));
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

    fn reply(id: i64, body: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","result":{body},"id":{id}}}"#)
    }

    #[tokio::test]
    async fn create_start_agent_snapshot_stop() {
        let dir = Scratch::new();
        let path = dir.0.join("api.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("c"));
            let mut writer = stream;
            let mut n = 0i64;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("r") == 0 {
                    break;
                }
                n += 1;
                if line.contains("handshake") {
                    let extra = r#"{"ok":true,"api_version":1,"min_api_version":1,"max_api_version":1,"build":"test"}"#;
                    writeln!(writer, r#"{{"jsonrpc":"2.0","result":{extra},"id":{n}}}"#)
                        .expect("hs");
                } else if line.contains("subscribe") {
                    writeln!(
                        writer,
                        "{}",
                        reply(n, r#"{"ok":true,"subscribed":["agent-connect"]}"#)
                    )
                    .expect("sub");
                    writeln!(
                        writer,
                        r#"{{"jsonrpc":"2.0","method":"event","params":{{"kind":"agent-connect","vm":"lab","state":"connected"}}}}"#
                    )
                    .expect("ev");
                } else {
                    writeln!(writer, "{}", reply(n, r#"{"ok":true}"#)).expect("ok");
                }
            }
        });
        run(&Lifecycle {
            socket: path,
            name: "lab".into(),
            snapshot: "boot".into(),
            config_toml: "name = \"lab\"\n".into(),
            agent_timeout: Duration::from_secs(5),
        })
        .await
        .expect("lifecycle");
        server.join().expect("join");
    }
}
