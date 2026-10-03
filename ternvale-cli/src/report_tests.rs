use std::io::Read;
use std::path::{Path, PathBuf};

use super::bundle::{write_zip, Item, Source};
use super::{plan, Live, PlanInputs};
use crate::diag::{write_manifest, RunManifest};

fn scratch(test: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ternvale-cli-report-{}-{test}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear leftovers");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn entries(zip: &Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(zip).expect("open zip");
    let mut archive = zip::ZipArchive::new(file).expect("read zip");
    (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i).expect("entry");
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read entry");
            (entry.name().to_string(), bytes)
        })
        .collect()
}

fn inputs<'a>(config: Option<&'a Path>) -> PlanInputs<'a> {
    PlanInputs {
        name: "demo",
        config,
        live: Live::NotRunning,
        doctor: Some("PASS  macOS version  macOS 26.1".into()),
    }
}

#[test]
fn plan_uses_the_newest_run_and_its_sidecars() {
    let dir = scratch("plan");
    std::fs::write(dir.join("ternvale-demo-20261001-100000.log"), "old\n").expect("old log");
    let log = dir.join("ternvale-demo-20261002-120000.log");
    std::fs::write(&log, "host log line\n").expect("log");
    let config = dir.join("vm.toml");
    std::fs::write(&config, "name = \"demo\"\n").expect("config");
    let serial = dir.join("serial.log");
    std::fs::write(&serial, "Linux version 6.x\n").expect("serial");
    write_manifest(&RunManifest {
        name: "demo".into(),
        pid: 1,
        version: "0.1.0".into(),
        config: config.clone(),
        serial_log: serial.clone(),
        host_log: log.clone(),
        started: "now".into(),
    })
    .expect("manifest");
    std::fs::write(
        dir.join("ternvale-demo-20261002-120000.mmio.txt"),
        "# vm demo\n",
    )
    .expect("mmio");
    std::fs::write(
        dir.join("ternvale-demo-20261002-120000.summary.txt"),
        "vm demo stopped\n",
    )
    .expect("summary");

    let plan = plan(&dir, &inputs(None), "20261002-130000").expect("plan");
    assert_eq!(plan.top, "ternvale-report-demo-20261002-130000");
    let find = |name: &str| {
        plan.items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name} planned"))
            .source
            .clone()
    };
    assert!(matches!(find("host.log"), Source::File { path, .. } if path == log));
    assert!(matches!(find("config.toml"), Source::File { path, .. } if path == config));
    assert!(matches!(find("guest-serial.log"), Source::File { path, .. } if path == serial));
    assert!(matches!(find("guest.dtb"), Source::Missing(why) if why.contains("does not exist")));
    assert!(matches!(find("mmio-events.txt"), Source::File { .. }));
    assert!(matches!(find("doctor.txt"), Source::Bytes(_)));
    assert!(
        plan.header[3].starts_with("vm was not running"),
        "{:?}",
        plan.header
    );

    let out = dir.join("report.zip");
    let readme = write_zip(&out, &plan.top, &plan.header, &plan.items).expect("zip");
    let got = entries(&out);
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    for want in [
        "host.log",
        "config.toml",
        "guest-serial.log",
        "mmio-events.txt",
        "summary.txt",
        "run.json",
        "doctor.txt",
        "README.txt",
    ] {
        assert!(
            names.contains(&format!("{}/{want}", plan.top).as_str()),
            "{want} in {names:?}"
        );
    }
    let host = got
        .iter()
        .find(|(n, _)| n.ends_with("/host.log"))
        .expect("host.log");
    assert_eq!(host.1, b"host log line\n");
    assert!(readme.contains("Missing:\n  guest.dtb: "), "{readme}");
    assert!(readme.contains("  config.toml: 14 bytes from "), "{readme}");
}

#[test]
fn plan_without_a_manifest_takes_config_from_the_flag_or_reports_it_missing() {
    let dir = scratch("nomanifest");
    std::fs::write(dir.join("ternvale-demo-20261002-120000.log"), "x\n").expect("log");
    let plan_none = plan(&dir, &inputs(None), "s").expect("plan");
    let config = plan_none
        .items
        .iter()
        .find(|i| i.name == "config.toml")
        .expect("config");
    assert!(matches!(&config.source, Source::Missing(why) if why.contains("--config")));

    let flag = dir.join("given.toml");
    std::fs::write(
        &flag,
        "name = \"demo\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
    )
    .expect("config");
    let plan_flag = plan(&dir, &inputs(Some(&flag)), "s").expect("plan");
    let config = plan_flag
        .items
        .iter()
        .find(|i| i.name == "config.toml")
        .expect("config");
    assert!(matches!(&config.source, Source::File { path, .. } if path == &flag));

    let error = plan(
        &dir,
        &PlanInputs {
            name: "ghost",
            ..inputs(None)
        },
        "s",
    )
    .expect_err("no logs");
    assert!(
        format!("{error:#}").contains("no host logs for vm \"ghost\""),
        "{error:#}"
    );
}

#[test]
fn a_vm_that_never_logged_is_reported_from_its_config() {
    let dir = scratch("nolog");
    let config = dir.join("vm.toml");
    let serial = dir.join("serial.log");
    std::fs::write(&serial, "").expect("serial");
    std::fs::write(
        &config,
        format!(
            "name = \"demo\"\ncpus = 1\nram_mib = 256\nkernel = \"/missing/Image\"\nserial_log = \"{}\"\n",
            serial.display()
        ),
    )
    .expect("config");
    let doctor = "PASS  macOS version  macOS 27.0\nFAIL  kernel         /missing/Image: No such file or directory\n                     fix: point it at an Image";
    let plan = plan(
        &dir,
        &PlanInputs {
            doctor: Some(doctor.into()),
            ..inputs(Some(&config))
        },
        "s",
    )
    .expect("plan");
    assert!(
        plan.header[2].starts_with("no host log for vm demo in "),
        "{:?}",
        plan.header
    );
    assert_eq!(
        plan.header[3],
        "doctor: FAIL  kernel         /missing/Image: No such file or directory"
    );
    assert_eq!(plan.header.len(), 4);
    let names: Vec<&str> = plan.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        ["host.log", "config.toml", "guest-serial.log", "doctor.txt"]
    );
    assert!(
        matches!(&plan.items[0].source, Source::Missing(why) if why.contains("never started logging"))
    );
    assert!(matches!(&plan.items[2].source, Source::File { path, .. } if path == &serial));
}

#[test]
fn dtb_is_bundled_with_a_dts_or_a_reason() {
    let dir = scratch("dtb");
    let log = dir.join("ternvale-demo-20261002-120000.log");
    std::fs::write(&log, "x\n").expect("log");
    let dtb = ternvale_vmm::build_fdt(&ternvale_vmm::GuestFdt {
        bootargs: "console=ttyAMA0".into(),
        ram_base: 0x4000_0000,
        ram_size: 256 << 20,
        initrd_start: 0,
        initrd_end: 0,
        cpu_count: 1,
        firmware: false,
        fw_cfg: false,
    })
    .expect("fdt");
    std::fs::write(dir.join("ternvale-demo-20261002-120000.dtb"), &dtb).expect("dtb");
    let plan = {
        let _spawning = crate::tests::CHILD_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plan(&dir, &inputs(None), "s").expect("plan")
    };
    assert!(plan
        .items
        .iter()
        .any(|i| i.name == "guest.dtb" && matches!(i.source, Source::File { .. })));
    let dts = plan
        .items
        .iter()
        .find(|i| i.name == "guest.dts")
        .expect("dts item");
    match &dts.source {
        Source::Bytes(text) => {
            let text = String::from_utf8_lossy(text);
            assert!(text.contains("console=ttyAMA0"), "{text}");
        }
        Source::Missing(why) => assert!(why.contains("dtc"), "{why}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn big_files_keep_their_tail_from_a_line_start() {
    let dir = scratch("tail");
    let big = dir.join("big.log");
    let text: String = (0..1000).map(|n| format!("line {n:04}\n")).collect();
    std::fs::write(&big, &text).expect("big");
    let out = dir.join("t.zip");
    let readme = write_zip(
        &out,
        "top",
        &["header".into()],
        &[
            Item::file("big.log", &big, 105),
            Item::file("gone.log", &dir.join("gone.log"), 10),
            Item::missing("guest.dtb", "never built"),
        ],
    )
    .expect("zip");
    let got = entries(&out);
    let tail = &got.iter().find(|(n, _)| n == "top/big.log").expect("big").1;
    let want: String = (990..1000).map(|n| format!("line {n:04}\n")).collect();
    assert_eq!(String::from_utf8_lossy(tail), want);
    assert!(
        readme.starts_with("header\n\nContents:\n  big.log: last 100 of 10000 bytes of "),
        "{readme}"
    );
    assert!(readme.contains("Missing:\n  gone.log: open "), "{readme}");
    assert!(readme.contains("  guest.dtb: never built"), "{readme}");
    assert_eq!(got.len(), 2, "big.log and README.txt");
}
