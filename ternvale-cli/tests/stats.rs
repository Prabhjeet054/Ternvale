//! Live `ternvale stats --watch` while the guest `dd`s the virtio-blk disk.
//!
//! ```sh
//! cargo test -p ternvale-cli --test stats -- --ignored --nocapture --test-threads=1
//! ```

mod support;

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use support::Harness;
use ternvale_cli::protocol::Response;

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(120);

fn blk_io(response: &Response) -> (u64, u64, u64) {
    let metrics = response
        .stats
        .as_ref()
        .and_then(|stats| stats.metrics.as_ref())
        .expect("query-stats metrics");
    (
        metrics.blk.count,
        metrics.blk.latency.samples,
        metrics.blk.latency.max_ns,
    )
}

#[test]
#[ignore = "needs-hv"]
fn stats_watch_rises_during_virtio_blk_dd() {
    let harness = Harness::new("statsdd");
    let image = harness.out.join("disk.img");
    {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&image)
            .expect("disk");
        file.set_len(64 * 1024 * 1024).expect("sparse 64MiB");
    }
    let extra = format!(
        "[[disks]]\npath = \"{}\"\nread_only = false\n",
        image.display()
    );
    let config = harness.write_config_with("virtio-blk", 1, &extra);
    let validate = harness.tv(&["validate", &config.display().to_string()]);
    assert_eq!(validate.code, Some(0), "{validate:?}");
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("vda ready and shell", BOOT, |serial| {
        serial.contains("vda ready") && serial.contains("# ")
    });

    let mut watch = harness.tv_spawn(&[
        "stats",
        &harness.name,
        "--watch",
        "--json",
        "--interval-ms",
        "200",
    ]);
    let stdout = watch.stdout.take().expect("stats stdout");
    let (tx, rx) = mpsc::channel::<Response>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if let Ok(response) = serde_json::from_str::<Response>(&line) {
                let _ = tx.send(response);
            }
        }
    });

    let first = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("first stats --watch sample");
    let (count0, samples0, max0) = blk_io(&first);
    tracing::info!(
        target: T,
        blk_count = count0,
        blk_samples = samples0,
        blk_max_ns = max0,
        "stats --watch before dd"
    );

    std::thread::sleep(Duration::from_millis(200));
    vm.type_chunks(&[
        "dd if=/dev/urand",
        "om of=/dev/vda ",
        "bs=1M count=16 ",
        "&& sync\n",
    ]);
    vm.wait_serial("dd finished", Duration::from_secs(120), |serial| {
        serial.contains("records out") || serial.contains("16+0 records")
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut last = (count0, samples0, max0);
    while std::time::Instant::now() < deadline {
        while let Ok(sample) = rx.try_recv() {
            last = blk_io(&sample);
            tracing::info!(
                target: T,
                blk_count = last.0,
                blk_samples = last.1,
                blk_max_ns = last.2,
                "stats --watch sample"
            );
        }
        if last.0 > count0 && last.1 > samples0 && last.2 >= max0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        last.0 > count0,
        "blk request count did not rise under stats --watch ({count0} -> {})",
        last.0
    );
    assert!(
        last.1 > samples0,
        "blk latency samples did not rise under stats --watch ({samples0} -> {})",
        last.1
    );
    tracing::info!(
        target: T,
        before_count = count0,
        after_count = last.0,
        before_samples = samples0,
        after_samples = last.1,
        before_max_ns = max0,
        after_max_ns = last.2,
        "stats --watch blk counters rose during dd"
    );

    vm.type_chunks(&["/bin/busybox ", "poweroff -f\n"]);
    assert_eq!(vm.wait_exit(Duration::from_secs(30)).code(), Some(0));
    let _ = watch.wait();
}
