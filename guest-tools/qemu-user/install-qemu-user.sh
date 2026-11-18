#!/bin/sh
# Install distro qemu-user-static in a Linux guest. Never downloads a Ternvale
# copy of QEMU. QEMU is GPL and runs as a separate interpreter process.
# Host: `ternvale x86 install-qemu-user` (typed yes). Do not vendor binaries.
set -eu

HERE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)

DRY_RUN=0
ROOT=

usage() {
    cat <<'EOF'
Usage: install-qemu-user.sh [--dry-run] [--root DIR]

Installs the guest distro's qemu-user-static (Debian/Ubuntu/Fedora) or
qemu-x86_64 (Alpine). Registers binfmt via the package. QEMU is GPL; Ternvale
does not ship it.

  --dry-run   Log planned packages; write the recipe under --root only.
  --root DIR  Prefix paths (host tests). Does not run apt/dnf/apk.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --root)
        shift
        [ $# -ge 1 ] || {
            echo "ERROR target=ternvale::compat reason=missing-arg arg=--root" >&2
            exit 2
        }
        ROOT=$1
        ;;
    --root=*) ROOT=${1#--root=} ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "ERROR target=ternvale::compat reason=unknown-arg arg=$1" >&2
        exit 2
        ;;
    esac
    shift
done

if [ -z "$ROOT" ] && [ -n "${TERNVALE_GUEST_ROOT:-}" ]; then
    ROOT=$TERNVALE_GUEST_ROOT
fi

path() {
    printf '%s%s' "${ROOT}" "$1"
}

log() {
    _lvl=$1
    shift
    printf '%s target=ternvale::compat %s\n' "$_lvl" "$*" >&2
}

die() {
    log ERROR "$@"
    exit 2
}

read_os() {
    _f=$(path /etc/os-release)
    if [ ! -f "$_f" ]; then
        _f=$(path /usr/lib/os-release)
    fi
    [ -f "$_f" ] || die "reason=no-os-release"
    OS_ID=
    OS_ID_LIKE=
    while IFS='=' read -r _k _v; do
        case "$_k" in
        ID | ID_LIKE) ;;
        *) continue ;;
        esac
        _v=${_v#\"}
        _v=${_v%\"}
        case "$_k" in
        ID) OS_ID=$_v ;;
        ID_LIKE) OS_ID_LIKE=$_v ;;
        esac
    done <"$_f"
}

FAMILY=
detect() {
    read_os
    _like=" ${OS_ID} ${OS_ID_LIKE} "
    case "$_like" in
    *" debian "* | *" ubuntu "* | *" linuxmint "* | *" pop "*)
        FAMILY=debian
        PKG="qemu-user-static"
        INTERPRETER=/usr/bin/qemu-x86_64-static
        ;;
    *" fedora "* | *" rhel "* | *" centos "* | *" rocky "* | *" alma "*)
        FAMILY=fedora
        PKG="qemu-user-static"
        INTERPRETER=/usr/bin/qemu-x86_64-static
        ;;
    *" alpine "*)
        FAMILY=alpine
        PKG="qemu-x86_64"
        INTERPRETER=/usr/bin/qemu-x86_64
        ;;
    *)
        die "reason=unsupported-distro id=${OS_ID:-unknown} hint='install qemu-user-static from your distro'"
        ;;
    esac
    log INFO "action=detect family=$FAMILY pkg=$PKG gpl=qemu-user-static separate_program=true"
}

write_docs() {
    _doc=$(path /usr/share/doc/ternvale)
    if [ "$DRY_RUN" -eq 1 ] && [ -z "$ROOT" ]; then
        log INFO "action=docs dry_run=true path=/usr/share/doc/ternvale/QEMU-USER.txt"
        return 0
    fi
    mkdir -p "$_doc" || die "mkdir doc failed"
    cat >"$_doc/QEMU-USER.txt" <<EOF
qemu-user-static (GPL) — last-resort x86 engine for Ternvale Linux guests.

Ternvale does not bundle QEMU. This guest installs $PKG from the distro
($FAMILY). The interpreter ($INTERPRETER) is a separate program registered
with binfmt_misc. See docs/X86.md and docs/TSO.md.

Do not copy QEMU into Ternvale.app.
EOF
    log INFO "action=docs path=/usr/share/doc/ternvale/QEMU-USER.txt pkg=$PKG"
}

write_metrics_stub() {
    _dir=$(path /var/lib/ternvale)
    if [ "$DRY_RUN" -eq 1 ] && [ -z "$ROOT" ]; then
        log INFO "action=metrics-stub dry_run=true"
        return 0
    fi
    mkdir -p "$_dir" || die "mkdir state failed"
    _m="$_dir/x86-metrics.json"
    if [ -f "$_m" ]; then
        log INFO "action=unchanged path=/var/lib/ternvale/x86-metrics.json"
        return 0
    fi
    printf '%s\n' '{"engine":"qemu-user","processes":0,"failures":0}' >"$_m"
    log INFO "action=create path=/var/lib/ternvale/x86-metrics.json"
}

install_live() {
    if [ "${TERNVALE_X86_BREAK_GUEST_NET:-}" = "1" ]; then
        die "reason=guest-network-unreachable hint='no route or DNS in the guest'"
    fi
    if [ -n "$ROOT" ]; then
        log INFO "action=pkg skipped=root hint='host tests do not apt-get'"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        log INFO "action=pkg dry_run=true family=$FAMILY pkg=$PKG"
        return 0
    fi
    # Fail fast when the guest has no network (do not hang the app spinner).
    case "$FAMILY" in
    debian)
        DEBIAN_FRONTEND=noninteractive apt-get \
            -o Acquire::Retries=0 \
            -o Acquire::http::Timeout=10 \
            -o Acquire::https::Timeout=10 \
            -o Acquire::ftp::Timeout=10 \
            update -y
        DEBIAN_FRONTEND=noninteractive apt-get \
            -o Acquire::Retries=0 \
            -o Acquire::http::Timeout=10 \
            -o Acquire::https::Timeout=10 \
            install -y "$PKG"
        ;;
    fedora)
        if command -v dnf >/dev/null 2>&1; then
            dnf install -y --setopt=timeout=10 --setopt=retries=0 "$PKG"
        else
            yum install -y --setopt=timeout=10 "$PKG"
        fi
        ;;
    alpine)
        apk add --no-cache --timeout 10 "$PKG" 2>/dev/null || apk add --no-cache "$PKG"
        ;;
    esac
    log INFO "action=pkg result=ok pkg=$PKG interpreter=$INTERPRETER"
    if [ -x "$INTERPRETER" ]; then
        log INFO "action=interpreter present=true path=$INTERPRETER"
    else
        log WARN "action=interpreter present=false path=$INTERPRETER hint='package layout differs'"
    fi
}

copy_metrics_helper() {
    _dest=$(path /usr/local/lib/ternvale/qemu-user)
    if [ "$DRY_RUN" -eq 1 ] && [ -z "$ROOT" ]; then
        log INFO "action=helper dry_run=true"
        return 0
    fi
    mkdir -p "$_dest" || die "mkdir helper failed"
    if [ -f "$HERE/metrics.sh" ]; then
        cp "$HERE/metrics.sh" "$_dest/metrics.sh"
        chmod 0755 "$_dest/metrics.sh" || true
        log INFO "action=helper path=/usr/local/lib/ternvale/qemu-user/metrics.sh"
    fi
}

detect
write_docs
write_metrics_stub
copy_metrics_helper
install_live
log INFO "action=done family=$FAMILY pkg=$PKG dry_run=$DRY_RUN root=${ROOT:-/}"
