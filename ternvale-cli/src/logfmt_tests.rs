use super::{parse, strip_ansi, target_matches, Filter, Level, LineFilter, Record};

const COLORED: &str = "2026-10-02T16:03:07.994254Z  INFO ThreadId(01) vm{\u{1b}[3mvm\u{1b}[0m\u{1b}[2m=\u{1b}[0mit-agentfs-3441}: ternvale::boot: vm state initialized vm=\"it\" cpus=1 state=created";

fn record(level: Level, target: &str) -> Option<Record> {
    Some(Record {
        level,
        target: target.to_string(),
    })
}

#[test]
fn parses_text_headers_with_and_without_spans() {
    assert_eq!(parse(COLORED), record(Level::Info, "ternvale::boot"));
    assert_eq!(
        parse("2026-10-02T16:03:07.994043Z  INFO ThreadId(01) ternvale::log: logging initialized log_file=/x.log"),
        record(Level::Info, "ternvale::log")
    );
    assert_eq!(
        parse("2026-10-02T16:02:23.678146Z  WARN ThreadId(14) vm{vm=demo}:agent-conn{vm=demo conn=1}: ternvale::agent: lost: eof k=v"),
        record(Level::Warn, "ternvale::agent")
    );
    assert_eq!(
        parse("2026-10-02T16:02:23.678146Z DEBUG ThreadId(02) dispatch: ternvale::cli: dispatch command=Status"),
        record(Level::Debug, "ternvale::cli")
    );
    assert_eq!(
        parse("2026-10-02T16:02:23.678146Z ERROR ternvale::hv: hv_vm_create returned HV_DENIED"),
        record(Level::Error, "ternvale::hv"),
        "no thread id"
    );
    assert_eq!(
        parse("2026-10-02T16:02:23.678146Z TRACE ThreadId(03) mio: registering: fd=3"),
        record(Level::Trace, "mio"),
        "a target without ::"
    );
}

#[test]
fn span_fields_with_colons_do_not_end_the_span() {
    assert_eq!(
        parse(
            "2026-10-02T16:02:23Z  INFO ThreadId(01) control{path=a: b conn=1}: ternvale::cli: x"
        ),
        record(Level::Info, "ternvale::cli")
    );
}

#[test]
fn parses_json_lines() {
    let line = r#"{"timestamp":"2026-10-02T16:02:23Z","level":"WARN","fields":{"message":"x"},"target":"ternvale::virtio::blk","threadId":"ThreadId(3)"}"#;
    assert_eq!(parse(line), record(Level::Warn, "ternvale::virtio::blk"));
    assert_eq!(parse(r#"{"level":"WARN"}"#), None);
}

#[test]
fn continuation_lines_are_not_records() {
    for line in [
        "",
        "   0: std::backtrace::Backtrace::create",
        "stack backtrace:",
        "2026 not a header",
        "2026-10-02T16:02:23Z  LOUD ThreadId(01) ternvale::x: y",
    ] {
        assert_eq!(parse(line), None, "{line:?}");
    }
}

#[test]
fn level_names_parse_case_insensitively() {
    assert_eq!("WARN".parse::<Level>(), Ok(Level::Warn));
    assert_eq!("warning".parse::<Level>(), Ok(Level::Warn));
    assert_eq!("Trace".parse::<Level>(), Ok(Level::Trace));
    assert!("verbose"
        .parse::<Level>()
        .unwrap_err()
        .contains("expected trace"));
    assert!(Level::Error > Level::Warn && Level::Debug > Level::Trace);
}

#[test]
fn targets_match_on_path_boundaries() {
    assert!(target_matches("ternvale::virtio::blk", "ternvale::virtio"));
    assert!(target_matches("ternvale::virtio", "ternvale::virtio"));
    assert!(!target_matches("ternvale::virtiofs", "ternvale::virtio"));
    assert!(target_matches("ternvale::virtio::blk", "virtio::blk"));
    assert!(target_matches("ternvale::agent", "agent"));
    assert!(target_matches("mio", "mio"));
    assert!(!target_matches("ternvale::cli", "ternvale::virtio"));
}

#[test]
fn filter_combines_level_and_targets() {
    let filter = Filter {
        level: Some(Level::Info),
        targets: vec!["virtio".into(), "ternvale::agent".into()],
    };
    assert!(filter.accepts(&Record {
        level: Level::Warn,
        target: "ternvale::virtio::net".into()
    }));
    assert!(!filter.accepts(&Record {
        level: Level::Debug,
        target: "ternvale::virtio::net".into()
    }));
    assert!(!filter.accepts(&Record {
        level: Level::Error,
        target: "ternvale::cli".into()
    }));
    assert!(Filter::default().is_empty());
}

#[test]
fn continuation_lines_follow_their_record() {
    let mut lines = LineFilter::new(Filter {
        level: Some(Level::Error),
        targets: Vec::new(),
    });
    assert!(!lines.accept("orphan before any header"));
    assert!(!lines.accept("2026-10-02T16:02:23Z  INFO ThreadId(01) ternvale::cli: hi"));
    assert!(!lines.accept("  continuation of info"));
    assert!(lines.accept("2026-10-02T16:02:23Z ERROR ThreadId(01) ternvale::log: panic"));
    assert!(lines.accept("   0: backtrace frame"));
    let mut all = LineFilter::new(Filter::default());
    assert!(all.accept("orphan before any header"));
}

#[test]
fn strips_ansi_escapes() {
    assert_eq!(
        strip_ansi("vm{\u{1b}[3mvm\u{1b}[0m\u{1b}[2m=\u{1b}[0mdemo}"),
        "vm{vm=demo}"
    );
    assert_eq!(strip_ansi("plain \u{1b} text"), "plain \u{1b} text");
    assert_eq!(strip_ansi("\u{1b}[1;31mred"), "red");
}
