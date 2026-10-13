//! Checks for `uefi-linux-tpm` ([`crate::uefi_linux_tpm`]): what the Linux
//! guest's tpm2-tools printed on each boot, that the TPM identity (EK and an
//! NV index) survived the restart, and that the host log shows the same
//! random bytes leaving swtpm through the CRB.

use std::collections::BTreeMap;
use std::path::Path;

use crate::firmware_tpm::{first_command, hex};

pub const DONE: &str = "ternvale-tpm: done";
const MARK: &str = "ternvale-tpm: ";
/// `TPM2_PT_MANUFACTURER` of libtpms (swtpm's TPM): "IBM" in ASCII.
const MANUFACTURER_IBM: &str = "0x49424D00";

/// One boot's `ternvale-tpm:` lines: `<what> key=value …`.
pub struct Guest {
    results: BTreeMap<String, BTreeMap<String, String>>,
    notes: Vec<String>,
}

impl Guest {
    pub fn parse(serial: &str) -> Guest {
        let mut results = BTreeMap::new();
        let mut notes = Vec::new();
        for rest in serial
            .lines()
            .filter_map(|l| l.split_once(MARK).map(|(_, rest)| rest.trim_end()))
        {
            let (what, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            if matches!(what, "pcr" | "stderr") {
                notes.push(rest.to_string());
                continue;
            }
            let fields = tail
                .split_whitespace()
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            results.insert(what.to_string(), fields);
        }
        Guest { results, notes }
    }

    pub fn get(&self, what: &str, key: &str) -> &str {
        self.results
            .get(what)
            .and_then(|f| f.get(key))
            .map_or("", String::as_str)
    }

    fn randoms(&self) -> [&str; 2] {
        [self.get("random1", "hex"), self.get("random2", "hex")]
    }
}

fn is_hex(text: &str, bytes: usize) -> bool {
    text.len() == 2 * bytes && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Per boot: Linux bound `tpm_crb` to `\_SB.TPM2`, and every tpm2-tools step
/// succeeded with plausible output. Boot 1 defines the NV index, boot 2 finds it.
pub fn check_boot(boot: u32, guest: &Guest) -> Result<(), String> {
    let mut problems = Vec::new();
    for (key, want) in [
        ("acpi_tpm2", "y"),
        ("tpmrm0", "y"),
        ("driver", "tpm_crb"),
        ("major", "2"),
    ] {
        if guest.get("probe", key) != want {
            problems.push(format!(
                "probe {key}={:?}, want {want}",
                guest.get("probe", key)
            ));
        }
    }
    for what in ["random1", "random2", "ek", "nv"] {
        if guest.get(what, "rc") != "0" {
            problems.push(format!("{what} rc={:?}", guest.get(what, "rc")));
        }
    }
    let [r1, r2] = guest.randoms();
    for r in [r1, r2] {
        if !is_hex(r, 32) || r.bytes().all(|b| b == b'0') {
            problems.push(format!("random {r:?} is not 32 non-zero bytes"));
        }
    }
    if r1 == r2 {
        problems.push("both tpm2_getrandom calls returned the same bytes".to_string());
    }
    if guest.get("manufacturer", "raw") != MANUFACTURER_IBM {
        problems.push(format!(
            "TPM2_PT_MANUFACTURER {:?}, want {MANUFACTURER_IBM} (libtpms)",
            guest.get("manufacturer", "raw")
        ));
    }
    let want_state = if boot == 1 { "new" } else { "existing" };
    if guest.get("nv", "state") != want_state || !is_hex(guest.get("nv", "hex"), 32) {
        problems.push(format!(
            "nv state={:?} hex={:?}, want state={want_state} and 32 bytes",
            guest.get("nv", "state"),
            guest.get("nv", "hex")
        ));
    }
    for note in &guest.notes {
        if note.starts_with("stderr") {
            tracing::warn!(target: "ternvale::boot", boot, line = %note, "guest tpm2-tools stderr");
        } else {
            tracing::info!(target: "ternvale::boot", boot, line = %note, "guest tpm2-tools output");
        }
    }
    tracing::info!(
        target: "ternvale::boot",
        boot,
        driver = guest.get("probe", "driver"),
        hwrng = guest.get("probe", "hwrng"),
        firmware_event_log_bytes = guest.get("eventlog", "bytes"),
        random1 = r1,
        random2 = r2,
        manufacturer = guest.get("manufacturer", "raw"),
        ek_bytes = guest.get("ek", "hex").len() / 2,
        nv_state = guest.get("nv", "state"),
        nv = guest.get("nv", "hex"),
        "guest tpm2-tools results"
    );
    if problems.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", boot, problems = %problems.join("; "), "guest tpm check failed");
    Err(format!("boot {boot}: {}", problems.join("; ")))
}

/// Across the restart: the same EK (and the one the host derives from the
/// state file) and the same NV contents, but fresh random bytes.
pub fn check_restart(first: &Guest, second: &Guest, host_ek: &[u8]) -> Result<(), String> {
    let mut problems = Vec::new();
    let ek = first.get("ek", "hex");
    if ek.is_empty() || ek != second.get("ek", "hex") {
        problems.push(format!(
            "EK changed across the restart: {ek} vs {}",
            second.get("ek", "hex")
        ));
    }
    let host = format!("{:04x}{}", host_ek.len(), hex(host_ek));
    if ek != host {
        problems.push(format!(
            "guest EK {ek} is not the host's EK from the state file {host}"
        ));
    }
    let nv = first.get("nv", "hex");
    if nv != second.get("nv", "hex") {
        problems.push(format!(
            "NV index changed across the restart: {nv} vs {}",
            second.get("nv", "hex")
        ));
    }
    let mut randoms: Vec<&str> = first
        .randoms()
        .into_iter()
        .chain(second.randoms())
        .collect();
    randoms.sort_unstable();
    randoms.dedup();
    if randoms.len() != 4 {
        problems.push("tpm2_getrandom repeated bytes across boots".to_string());
    }
    tracing::info!(
        target: "ternvale::boot",
        ek_tail = &ek[ek.len().saturating_sub(32)..],
        ek_matches_host = ek == host,
        nv,
        "tpm identity across the restart"
    );
    if problems.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", problems = %problems.join("; "), "tpm identity check failed");
    Err(problems.join("\n"))
}

/// The value of ` key=` on a log line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let key = format!(" {key}=");
    let at = line.find(&key)? + key.len();
    line[at..].split_whitespace().next()
}

/// swtpm started for boot 1 (new TPM), boot 2 and the host EK read; EDK2's
/// `TPM2_Startup` worked on both boots; every guest random value is in a
/// successful CRB response from swtpm (`TPM2B_DIGEST`: size 0x0020, bytes);
/// and the TPM device logged no WARN or ERROR.
pub fn check_host_log(host: &Path, guests: &[Guest]) -> Result<(), String> {
    let log =
        std::fs::read_to_string(host).map_err(|err| format!("read {}: {err}", host.display()))?;
    let mut problems = Vec::new();
    let ready: Vec<bool> = log
        .lines()
        .filter(|l| l.contains("swtpm ready"))
        .map(|l| l.contains("new_tpm=true"))
        .collect();
    if ready != [true, false, false] {
        problems.push(format!(
            "swtpm ready new_tpm={ready:?}, want [true, false, false]"
        ));
    }
    let startups = log
        .lines()
        .filter(|l| first_command(l) == Some(("0x144", "0x0", "swtpm")))
        .count();
    if startups != 2 {
        problems.push(format!("{startups} successful EDK2 TPM2_Startup, want 2"));
    }
    let responses: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("tpm response"))
        .filter_map(|l| field(l, "bytes"))
        .filter(|b| b.starts_with("8001") && b.get(12..20) == Some("00000000"))
        .collect();
    for (boot, guest) in guests.iter().enumerate() {
        for random in guest.randoms() {
            let digest = format!("0020{random}");
            if !responses.iter().any(|b| b[20..].starts_with(&digest)) {
                problems.push(format!(
                    "boot {}: random {random} is in no successful CRB response",
                    boot + 1
                ));
            }
        }
    }
    let get_random = log
        .lines()
        .filter(|l| l.contains("tpm command locality=") && field(l, "code") == Some("0x17b"))
        .count();
    let complaints: Vec<&str> = log
        .lines()
        .filter(|l| {
            l.contains(" ternvale::tpm: ") && (l.contains("  WARN ") || l.contains(" ERROR "))
        })
        .collect();
    if !complaints.is_empty() {
        problems.push(format!("tpm WARN/ERROR lines:\n{}", complaints.join("\n")));
    }
    tracing::info!(
        target: "ternvale::boot",
        responses = responses.len(),
        get_random,
        "guest random bytes found in swtpm's CRB responses"
    );
    if problems.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", problems = %problems.join("; "), "tpm host log check failed");
    Err(format!("{}\nlog={}", problems.join("\n"), host.display()))
}
