//! Golden-file tests: existing JSON-RPC and handshake shapes must not change.

use std::path::PathBuf;

use ternvale_api::{
    generate_swift_client, is_jsonrpc_line, parse_request, ApiError, Event, EventKind,
    HandshakeParams, HandshakeResult, JsonRpcId, JsonRpcResponse, Method, CONTROL_SCHEMA,
    EVENT_KINDS, METHODS, MIN_API_VERSION,
};

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("goldens")
        .join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(golden(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn assert_json_eq(name: &str, got: &str) {
    let want = read(name);
    let got_v: serde_json::Value = serde_json::from_str(got.trim()).expect("got json");
    let want_v: serde_json::Value = serde_json::from_str(want.trim()).expect("want json");
    assert_eq!(got_v, want_v, "{name}\ngot={got}\nwant={want}");
}

#[test]
fn handshake_request_and_result_match_goldens() {
    let req = parse_request(
        r#"{"jsonrpc":"2.0","method":"handshake","params":{"api_version":1},"id":1}"#,
    )
    .expect("parse");
    assert_json_eq(
        "rpc-handshake-request.json",
        &serde_json::to_string(&req).expect("enc"),
    );
    let params: HandshakeParams = serde_json::from_value(req.params.expect("params")).expect("p");
    assert_eq!(params.api_version, 1);
    let result = HandshakeResult::negotiate(1).expect("ok");
    assert_json_eq(
        "rpc-handshake-result.json",
        &serde_json::to_string(&result).expect("enc"),
    );
}

#[test]
fn method_not_found_error_matches_golden() {
    let err = ApiError::MethodNotFound("nope".into());
    let resp = JsonRpcResponse::from_api_error(JsonRpcId::from(1), &err);
    assert_json_eq(
        "rpc-error-method-not-found.json",
        &serde_json::to_string(&resp).expect("enc"),
    );
}

#[test]
fn event_state_matches_golden() {
    let event = Event {
        kind: EventKind::State,
        vm: "win11".into(),
        state: Some("paused".into()),
        tools_outdated: None,
        line: None,
        completed: None,
        total: None,
        permille: None,
    };
    assert_json_eq(
        "event-state.json",
        &serde_json::to_string(&event).expect("enc"),
    );
}

#[test]
fn schema_lists_every_method_and_event() {
    let schema: serde_json::Value = serde_json::from_str(CONTROL_SCHEMA).expect("schema json");
    assert_eq!(schema["api_version"], MIN_API_VERSION);
    let methods = schema["methods"].as_object().expect("methods");
    for method in METHODS {
        assert!(
            methods.contains_key(method.as_str()),
            "schema missing {}",
            method.as_str()
        );
    }
    let kinds = schema["events"]["kinds"].as_array().expect("kinds");
    for kind in EVENT_KINDS {
        assert!(
            kinds.iter().any(|v| v.as_str() == Some(kind.as_str())),
            "schema missing event {}",
            kind.as_str()
        );
    }
    let cmds = schema["legacy_cmd"]["commands"].as_array().expect("cmds");
    for cmd in [
        "status",
        "pause",
        "resume",
        "shutdown",
        "force-stop",
        "query-stats",
        "balloon-set",
        "set-data-key",
    ] {
        assert!(
            cmds.iter().any(|v| v.as_str() == Some(cmd)),
            "missing {cmd}"
        );
    }
}

#[test]
fn generated_swift_matches_committed_file() {
    let generated = generate_swift_client();
    let crate_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("generated/TernvaleControlClient.swift");
    let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../app/Ternvale/Ternvale/Services/GeneratedControlClient.swift");
    for path in [&crate_path, &app_path] {
        let on_disk = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            on_disk,
            generated,
            "{} is out of date; run scripts/gen-swift-client.sh",
            path.display()
        );
    }
    assert!(generated.contains("case handshake = \"handshake\""));
    assert!(generated.contains("case snapshotCreate = \"snapshot.create\""));
    assert!(generated.contains("ternvaleApiVersion: UInt32 = 1"));
    let _ = Method::Handshake;
}

#[test]
fn legacy_cmd_is_not_jsonrpc() {
    assert!(!is_jsonrpc_line(r#"{"cmd":"status"}"#));
}

#[test]
fn handshake_rejects_unknown_version() {
    let err = HandshakeResult::negotiate(99).expect_err("bad");
    match err {
        ApiError::UnsupportedVersion { got, min, max } => {
            assert_eq!(got, 99);
            assert_eq!(min, 1);
            assert_eq!(max, 1);
        }
        other => panic!("{other:?}"),
    }
}
