//! Poll VM/agent state and emit JSON-RPC `event` notifications.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use ternvale_api::{Event, EventKind};
use ternvale_devices::AgentServer;
use ternvale_vmm::VmControl;

use crate::protocol::{AgentJson, StatusJson};

const POLL: Duration = Duration::from_millis(200);

/// Write notifications until the client goes away.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = control.name()))]
pub fn stream_vm_events(
    writer: &mut UnixStream,
    control: &VmControl,
    agent: Option<&AgentServer>,
    tools_iso: Option<&Path>,
    kinds: &[EventKind],
) {
    let want = |kind: EventKind| kinds.contains(&kind);
    let mut last_state = String::new();
    let mut last_agent: Option<String> = None;
    let mut last_tools: Option<bool> = None;
    std::thread::sleep(POLL);
    loop {
        let mut status = StatusJson::from(&control.status());
        if let Some(agent) = agent {
            status.agent = Some(AgentJson::from(&agent.status()));
        }
        crate::tools::fill_tools_outdated(&mut status, tools_iso);

        if want(EventKind::State) && status.state != last_state {
            let event = Event {
                kind: EventKind::State,
                vm: status.name.clone(),
                state: Some(status.state.clone()),
                tools_outdated: None,
                line: None,
                completed: None,
                total: None,
                permille: None,
            };
            if !emit(writer, &event) {
                return;
            }
            last_state = status.state.clone();
        }

        let agent_state = status.agent.as_ref().map(|a| a.state.as_str());
        if let Some(now) = agent_state {
            let prev = last_agent.as_deref();
            if want(EventKind::AgentConnect) && now == "connected" && prev != Some("connected") {
                let event = Event {
                    kind: EventKind::AgentConnect,
                    vm: status.name.clone(),
                    state: Some(now.to_string()),
                    tools_outdated: None,
                    line: None,
                    completed: None,
                    total: None,
                    permille: None,
                };
                if !emit(writer, &event) {
                    return;
                }
            }
            if want(EventKind::AgentDisconnect) && prev == Some("connected") && now != "connected" {
                let event = Event {
                    kind: EventKind::AgentDisconnect,
                    vm: status.name.clone(),
                    state: Some(now.to_string()),
                    tools_outdated: None,
                    line: None,
                    completed: None,
                    total: None,
                    permille: None,
                };
                if !emit(writer, &event) {
                    return;
                }
            }
            last_agent = Some(now.to_string());
        }

        if want(EventKind::GuestToolsOutdated) && status.tools_outdated != last_tools {
            if let Some(flag) = status.tools_outdated {
                let event = Event {
                    kind: EventKind::GuestToolsOutdated,
                    vm: status.name.clone(),
                    state: None,
                    tools_outdated: Some(flag),
                    line: None,
                    completed: None,
                    total: None,
                    permille: None,
                };
                if !emit(writer, &event) {
                    return;
                }
            }
            last_tools = status.tools_outdated;
        }

        std::thread::sleep(POLL);
        // Drop out if the peer closed (write of a 0-byte probe would still succeed).
        if writer.take_error().is_err() {
            return;
        }
    }
}

fn emit(writer: &mut UnixStream, event: &Event) -> bool {
    let note = event.notification();
    let mut line = match serde_json::to_string(&note) {
        Ok(line) => line,
        Err(error) => {
            tracing::error!(target: "ternvale::api", error = %error, "encode event");
            return false;
        }
    };
    tracing::debug!(
        target: "ternvale::api",
        kind = event.kind.as_str(),
        vm = event.vm.as_str(),
        "event"
    );
    line.push('\n');
    writer.write_all(line.as_bytes()).is_ok()
}
