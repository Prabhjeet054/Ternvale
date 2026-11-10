#!/usr/bin/env bash
# Record whether this host can run Hypervisor.framework guests.
# GitHub-hosted macOS arm64 runners are VMs; kern.hv_support is typically
# missing and hv_vm_create returns HV_UNSUPPORTED. See docs/CI.md.
set -euo pipefail

log() { printf 'hv-probe: %s\n' "$*" >&2; }

os=$(sw_vers -productName 2>/dev/null || echo unknown)
ver=$(sw_vers -productVersion 2>/dev/null || echo unknown)
arch=$(uname -m)
log "os=${os} version=${ver} arch=${arch}"
log "GITHUB_RUNNER=${RUNNER_NAME:-local} RUNNER_OS=${RUNNER_OS:-} RUNNER_ARCH=${RUNNER_ARCH:-}"

support="unknown"
if out=$(sysctl -n kern.hv_support 2>/dev/null); then
    support="${out}"
    log "kern.hv_support=${support}"
else
    support="missing"
    log "kern.hv_support: oid missing (nested VM; Hypervisor.framework not exposed)"
fi

if [[ "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
        echo "## Hypervisor.framework probe"
        echo
        echo "| Field | Value |"
        echo "|---|---|"
        echo "| OS | ${os} ${ver} (${arch}) |"
        echo "| Runner | ${RUNNER_NAME:-local} |"
        echo "| \`kern.hv_support\` | ${support} |"
        echo
        echo "GitHub-hosted macOS arm64 images remain nested VMs without HVF"
        echo "(actions/runner-images#9460, #13505, 2026-01-08 staff confirmation)."
        echo "needs-hv jobs run on the self-hosted Apple Silicon runner. See docs/CI.md."
    } >>"${GITHUB_STEP_SUMMARY}"
fi

# Hosted PR jobs must not fail this probe; HV work is on self-hosted hardware.
exit 0
