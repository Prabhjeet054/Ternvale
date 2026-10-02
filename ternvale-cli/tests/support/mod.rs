//! Plumbing for the `ternvale` binary integration tests: signing, a private
//! run directory, spawning `ternvale run` with a stdin pipe, the guest serial
//! log, a held control connection, and the host log's state transitions.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ternvale_cli::protocol::Response;
use ternvale_log::{LogConfig, LogGuard};

const T: &str = "ternvale::cli";

/// Workspace root.
pub fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

/// The `ternvale` binary, ad-hoc signed with the hypervisor entitlement once per process.
pub fn signed_binary() -> PathBuf {
    static SIGNED: OnceLock<PathBuf> = OnceLock::new();
    SIGNED
        .get_or_init(|| {
            let bin = PathBuf::from(env!("CARGO_BIN_EXE_ternvale"));
            let entitlements = root().join("entitlements/ternvale.entitlements");
            let out = Command::new("codesign")
                .args(["--sign", "-", "--force", "--entitlements"])
                .arg(&entitlements)
                .arg(&bin)
                .output()
                .expect("run codesign");
            assert!(out.status.success(), "codesign: {}", String::from_utf8_lossy(&out.stderr));
            tracing::info!(target: T, bin = %bin.display(), "signed ternvale with the hypervisor entitlement");
            bin
        })
        .clone()
}

/// Exit status and output of one `ternvale` client command.
#[derive(Debug)]
pub struct CliOut {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CliOut {
    /// The `error: …` line, without log lines.
    pub fn error_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|line| line.starts_with("error: "))
            .unwrap_or("")
    }
}

/// One test's VM name, private run directory, artifacts, and host logging.
pub struct Harness {
    pub name: String,
    pub bin: PathBuf,
    pub out: PathBuf,
    pub run_dir: PathBuf,
    _log: LogGuard,
}

impl Harness {
    /// `test` becomes part of the VM name, so it must be `[a-z0-9-]` and short.
    pub fn new(test: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let out = root().join(format!("target/control-logs/{stamp}-{test}"));
        std::fs::create_dir_all(&out).expect("artifact dir");
        let log = ternvale_log::init(LogConfig::new(format!("itest-{test}"), out.clone()))
            .expect("init logging");
        // A short private directory: sun_path is 104 bytes, and tests must not
        // touch the user's real run directory.
        let run_dir = PathBuf::from(format!("/tmp/tvit-{}-{test}", std::process::id()));
        if run_dir.exists() {
            std::fs::remove_dir_all(&run_dir).expect("clear old run dir");
        }
        let name = format!("it-{test}-{}", std::process::id());
        let bin = signed_binary();
        tracing::info!(target: T, vm = %name, out = %out.display(), run_dir = %run_dir.display(), "integration test start");
        Self {
            name,
            bin,
            out,
            run_dir,
            _log: log,
        }
    }

    /// Control socket of this test's VM.
    pub fn socket(&self) -> PathBuf {
        self.run_dir.join(format!("{}.sock", self.name))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bin);
        command.args(args).env("TERNVALE_RUN_DIR", &self.run_dir);
        command
    }

    /// Run a client command to completion and log what it printed.
    pub fn tv(&self, args: &[&str]) -> CliOut {
        let out = self.command(args).output().expect("run ternvale");
        let result = CliOut {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        tracing::info!(
            target: T,
            cmd = %format!("ternvale {}", args.join(" ")),
            code = ?result.code,
            stdout = %result.stdout.trim_end(),
            error = result.error_line(),
            "cli"
        );
        result
    }

    /// Start a client command without waiting (for `stop`, which blocks until exit).
    pub fn tv_spawn(&self, args: &[&str]) -> Child {
        tracing::info!(target: T, cmd = %format!("ternvale {}", args.join(" ")), "cli (background)");
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ternvale")
    }

    /// Write a busybox-initramfs config with `cpus` vCPUs and return its path.
    pub fn write_config(&self, cpus: u32) -> PathBuf {
        let assets = root().join("test-assets");
        for file in ["Image", "initramfs.cpio"] {
            assert!(
                assets.join(file).is_file(),
                "missing {} (see docs/BOOT.md)",
                assets.join(file).display()
            );
        }
        let path = self.out.join("vm.toml");
        let text = format!(
            "name = \"{}\"\ncpus = {cpus}\nram_mib = 256\nkernel = \"{}\"\ninitrd = \"{}\"\nserial_log = \"{}\"\n",
            self.name,
            assets.join("Image").display(),
            assets.join("initramfs.cpio").display(),
            self.out.join("guest-serial.log").display()
        );
        std::fs::write(&path, text).expect("write config");
        path
    }

    /// `ternvale run <config>` in the background, stdin on a pipe.
    pub fn spawn_run(&self, config: &Path) -> Vm {
        let stdout = std::fs::File::create(self.out.join("run.stdout")).expect("run.stdout");
        let stderr = std::fs::File::create(self.out.join("run.stderr")).expect("run.stderr");
        let mut child = self
            .command(&["run", &config.display().to_string()])
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("spawn ternvale run");
        let stdin = child.stdin.take().expect("stdin pipe");
        tracing::info!(target: T, pid = child.id(), "ternvale run started in the background");
        Vm {
            child,
            stdin,
            serial: self.out.join("guest-serial.log"),
        }
    }

    /// Newest `~/Library/Logs/Ternvale/ternvale-<name>-*.log`.
    pub fn host_log(&self) -> PathBuf {
        let dir = LogConfig::default_log_dir().expect("log dir");
        let prefix = format!("ternvale-{}-", self.name);
        std::fs::read_dir(&dir)
            .expect("read log dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())
            .map(|entry| entry.path())
            .unwrap_or_else(|| panic!("no {prefix}*.log in {}", dir.display()))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.run_dir.exists() {
            if let Err(error) = std::fs::remove_dir_all(&self.run_dir) {
                tracing::warn!(target: T, error = %error, "could not remove the run dir");
            }
        }
    }
}

/// A background `ternvale run`. Killed on drop if still alive.
pub struct Vm {
    child: Child,
    stdin: ChildStdin,
    serial: PathBuf,
}

impl Vm {
    /// Type into the guest console. The PL011 RX FIFO is 16 bytes, so each
    /// chunk must fit and gets time to drain.
    pub fn type_chunks(&mut self, chunks: &[&str]) {
        for chunk in chunks {
            assert!(chunk.len() <= 16, "chunk {chunk:?} overflows the RX FIFO");
            self.stdin
                .write_all(chunk.as_bytes())
                .expect("write guest stdin");
            self.stdin.flush().expect("flush guest stdin");
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Guest serial output so far.
    pub fn serial(&self) -> String {
        std::fs::read_to_string(&self.serial).unwrap_or_default()
    }

    /// Wait until `ready` holds for the serial log; fail if `run` exits first.
    pub fn wait_serial(&mut self, what: &str, limit: Duration, ready: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + limit;
        loop {
            if ready(&self.serial()) {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                panic!("ternvale run exited ({status}) while waiting for {what}");
            }
            assert!(
                Instant::now() < deadline,
                "no {what} in the serial log after {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait for `run` to exit.
    pub fn wait_exit(&mut self, limit: Duration) -> ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                tracing::info!(target: T, %status, "ternvale run exited");
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "ternvale run still alive after {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            tracing::warn!(target: T, pid = self.child.id(), "killing ternvale run left behind by a failed test");
            if let Err(error) = self.child.kill() {
                tracing::warn!(target: T, error = %error, "kill failed");
            }
        }
    }
}

/// A control connection kept open across requests (and across `stop`).
pub struct Held {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Held {
    pub fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect control socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        Self {
            writer: stream.try_clone().expect("clone"),
            reader: BufReader::new(stream),
        }
    }

    /// Send one raw JSON line. `None` once the VM process has closed the connection.
    pub fn send(&mut self, line: &str) -> Option<Response> {
        if self
            .writer
            .write_all(format!("{line}\n").as_bytes())
            .is_err()
        {
            return None;
        }
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                tracing::info!(target: T, request = line, response = reply.trim_end(), "held connection");
                Some(serde_json::from_str(reply.trim_end()).expect("response json"))
            }
        }
    }
}

/// Drop ANSI escape sequences (span fields in the log file carry them).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Every `vm state from=… to=…` line in `host_log` as `(from, to, line)`.
pub fn transitions(host_log: &Path) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(host_log).expect("read host log");
    let field = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .unwrap_or("")
            .to_string()
    };
    text.lines()
        .map(strip_ansi)
        .filter(|line| line.contains(" vm state ") && line.contains(" from="))
        .map(|line| (field(&line, "from="), field(&line, "to="), line))
        .collect()
}
