#!/bin/sh
# PID 1 for Step 97 live disks. Authentic distro userspace; not systemd.
# Reboot path applies the unit/fstab/binfmt.d/local.d files guest-setup wrote.
set -eu
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
MARKER="ternvale-guest-setup"

log() { printf '%s\n' "$*"; }

insmod_tv() {
    for m in fuse virtiofs binfmt_misc; do
        f="/lib/modules/tv/${m}.ko"
        if [ -f "$f" ] && command -v insmod >/dev/null 2>&1; then
            insmod "$f" 2>/dev/null || true
        fi
    done
}

mount_essential() {
    mkdir -p /proc /sys /dev /tmp /mnt/rosetta
    mount -t proc proc /proc 2>/dev/null || true
    mount -t sysfs sys /sys 2>/dev/null || true
    mount -t devtmpfs dev /dev 2>/dev/null || true
    insmod_tv
}

fingerprint() {
    find /etc /var/lib/ternvale /usr/share/doc/ternvale /usr/local/lib/ternvale \
        -type f 2>/dev/null | sort | while read -r f; do
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$f"
        else
            cksum "$f"
        fi
    done
}

phase_from_cmdline() {
    # shellcheck disable=SC2013
    for tok in $(cat /proc/cmdline); do
        case "$tok" in
        tvgs.phase=*)
            printf '%s' "${tok#tvgs.phase=}"
            return 0
            ;;
        esac
    done
    printf '%s' first
}

apply_persistent() {
    # systemd .mount unit written by guest-setup.
    unit=/etc/systemd/system/mnt-rosetta.mount
    if [ -f "$unit" ]; then
        what=$(sed -n 's/^What=//p' "$unit" | head -n 1)
        where=$(sed -n 's/^Where=//p' "$unit" | head -n 1)
        typ=$(sed -n 's/^Type=//p' "$unit" | head -n 1)
        mkdir -p "$where"
        if mount -t "$typ" "$what" "$where" 2>/dev/null; then
            log "TVGS_PERSIST_MOUNT=unit"
        else
            log "TVGS_PERSIST_MOUNT=unit-already"
        fi
    elif grep -q " ${MARKER} begin" /etc/fstab 2>/dev/null; then
        mkdir -p /mnt/rosetta
        mount -a 2>/dev/null || mount -t virtiofs rosetta /mnt/rosetta
        log "TVGS_PERSIST_MOUNT=fstab"
    fi
    if [ -x /etc/local.d/ternvale-rosetta.start ]; then
        /etc/local.d/ternvale-rosetta.start || true
        log "TVGS_PERSIST_MOUNT=local.d"
    fi
    mkdir -p /proc/sys/fs/binfmt_misc
    mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true
    if [ -x /etc/local.d/ternvale-binfmt.start ]; then
        /etc/local.d/ternvale-binfmt.start || true
        log "TVGS_PERSIST_BINFMT=local.d"
    elif [ -f /etc/binfmt.d/ternvale-rosetta.conf ] && [ -w /proc/sys/fs/binfmt_misc/register ]; then
        if [ ! -e /proc/sys/fs/binfmt_misc/rosetta ]; then
            grep -v '^#' /etc/binfmt.d/ternvale-rosetta.conf | grep -v '^$' \
                | while read -r line; do
                    printf '%s' "$line" >/proc/sys/fs/binfmt_misc/register || true
                done
        fi
        log "TVGS_PERSIST_BINFMT=binfmt.d"
    fi
}

run_x86() {
    runtime=/mnt/rosetta/rosetta
    if [ -d "$runtime" ] && [ -e "$runtime/rosetta" ]; then
        runtime="$runtime/rosetta"
    fi
    if [ -x "$runtime" ]; then
        set +e
        "$runtime" /x86/hello
        log "DIRECT_HELLO_RC=$?"
        set -e
    else
        log "DIRECT_HELLO_RC=missing"
    fi
    set +e
    /x86/hello
    log "HELLO_RC=$?"
    set -e
    if [ -e /proc/sys/fs/binfmt_misc/rosetta ]; then
        log "BINFMT_ENTRY_OK"
    else
        log "BINFMT_ENTRY_MISSING"
    fi
}

id_line() {
    if [ -f /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        log "TVGS_DISTRO id=${ID:-?} pretty=${PRETTY_NAME:-?}"
    fi
}

setup_bin=/usr/local/sbin/ternvale-guest-setup

mount_essential
id_line
log "TVGS_START phase=$(phase_from_cmdline)"

case "$(phase_from_cmdline)" in
first)
    before=$(fingerprint)
    "$setup_bin" --dry-run
    after=$(fingerprint)
    if [ "$before" = "$after" ]; then
        log "TVGS_DRY_OK"
    else
        log "TVGS_DRY_FAIL"
        printf '%s\n' "$before" | log "TVGS_FP_BEFORE"
        printf '%s\n' "$after" | log "TVGS_FP_AFTER"
    fi
    "$setup_bin"
    apply_persistent
    run_x86
    log "TVGS_APPLY_DONE"
    "$setup_bin" >/tmp/tvgs-second.log 2>&1 || true
    cat /tmp/tvgs-second.log
    if grep -q 'action=done changed=0' /tmp/tvgs-second.log; then
        log "TVGS_IDEMPOTENT_OK"
    elif grep 'action=changed' /tmp/tvgs-second.log >/dev/null; then
        log "TVGS_IDEMPOTENT_FAIL"
    else
        log "TVGS_IDEMPOTENT_OK"
    fi
    log "TVGS_FIRST_DONE"
    ;;
reboot)
    apply_persistent
    run_x86
    if [ -f /etc/systemd/system/mnt-rosetta.mount ] || grep -q virtiofs /etc/fstab 2>/dev/null; then
        log "TVGS_REBOOT_CONFIG_PRESENT"
    fi
    log "TVGS_REBOOT_DONE"
    "$setup_bin" --undo
    if [ -f /etc/systemd/system/mnt-rosetta.mount ] || [ -f /etc/binfmt.d/ternvale-rosetta.conf ]; then
        log "TVGS_UNDO_LEAK"
    else
        log "TVGS_UNDO_OK"
    fi
    if [ -e /proc/sys/fs/binfmt_misc/rosetta ]; then
        echo -1 >/proc/sys/fs/binfmt_misc/rosetta 2>/dev/null || true
    fi
    set +e
    /x86/hello
    log "UNDO_HELLO_RC=$?"
    set -e
    log "TVGS_UNDO_DONE"
    ;;
*)
    log "TVGS_BAD_PHASE"
    ;;
esac
log "TVGS_LIVE_DONE"
while :; do
    sleep 3600
done
