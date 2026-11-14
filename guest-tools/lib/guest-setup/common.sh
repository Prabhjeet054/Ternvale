# Shared helpers for ternvale-guest-setup. Sourced, not executed.
# Paths are prefixed with ROOT (empty on a real guest).

MARKER="ternvale-guest-setup"
UNDO_VERSION=1
STATE_REL="/var/lib/ternvale"
UNDO_NAME="guest-setup-undo.sh"
JOURNAL_NAME="guest-setup.journal"
STATE_MAGIC="TERNVALE_GUEST_SETUP"
STATE_VER=1

TAG="${ROSETTA_TAG:-rosetta}"
MOUNT="${ROSETTA_MOUNT:-/mnt/rosetta}"
INTERPRETER="${MOUNT}/rosetta"
# Apple "Running Intel Binaries in Linux VMs" (20-byte x86_64 ELF + mask).
BINFMT_MAGIC='\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00'
BINFMT_MASK='\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff'
# update-binfmts: credentials + preserve + fix-binary → kernel PCF.
# Apple: on macOS 13 use preserve no (flag without P).
PRESERVE="${ROSETTA_PRESERVE:-yes}"
BINFMT_FLAGS="PCF"

gs_path() {
    printf '%s%s' "${ROOT}" "$1"
}

gs_log() {
    _lvl=$1
    shift
    printf '%s target=ternvale::guest-setup %s\n' "$_lvl" "$*" >&2
}

gs_info() { gs_log INFO "$@"; }
gs_warn() { gs_log WARN "$@"; }
gs_error() { gs_log ERROR "$@"; }
gs_debug() { gs_log DEBUG "$@"; }

gs_die() {
    gs_error "$@"
    exit 2
}

gs_state_dir() {
    printf '%s' "$(gs_path "$STATE_REL")"
}

gs_init_state() {
    _dir=$(gs_state_dir)
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=state dry_run=true dir=$_dir"
        return 0
    fi
    mkdir -p "$_dir" || gs_die "mkdir state failed path=$_dir"
    _state="$_dir/state"
    if [ ! -f "$_state" ]; then
        printf '%s\nversion=%s\n' "$STATE_MAGIC" "$STATE_VER" >"$_state" || gs_die "write state failed"
        gs_info "action=create path=$_state changed=true"
        gs_undo_line "rm -f '$_state'"
    fi
}

gs_journal() {
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=journal dry_run=true $*"
        return 0
    fi
    printf '%s\n' "$*" >>"$(gs_state_dir)/$JOURNAL_NAME"
}

gs_undo_init() {
    UNDO_FILE="$(gs_state_dir)/$UNDO_NAME"
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=undo_init dry_run=true path=$UNDO_FILE"
        return 0
    fi
    mkdir -p "$(gs_state_dir)" || gs_die "mkdir state failed path=$(gs_state_dir)"
    if [ ! -f "$UNDO_FILE" ]; then
        printf '%s\n' "#!/bin/sh" \
            "# ${STATE_MAGIC} undo version=${UNDO_VERSION}" \
            "set -eu" \
            "echo \"INFO target=ternvale::guest-setup action=undo start\" >&2" \
            >"$UNDO_FILE" || gs_die "write undo failed path=$UNDO_FILE"
        chmod 0755 "$UNDO_FILE" || true
        gs_info "action=create path=$UNDO_FILE changed=true"
    fi
}

# Insert after the 4-line header so undo runs newest-first.
gs_undo_line() {
    _cmd=$1
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=undo_record dry_run=true cmd=${_cmd}"
        return 0
    fi
    _new="${UNDO_FILE}.new"
    {
        sed -n '1,4p' "$UNDO_FILE"
        printf '%s\n' "$_cmd"
        sed -n '5,$p' "$UNDO_FILE"
    } >"$_new"
    mv "$_new" "$UNDO_FILE"
}

gs_changed() {
    CHANGED_COUNT=$((CHANGED_COUNT + 1))
    gs_info "action=changed $*"
    gs_journal "$*"
}

gs_unchanged() {
    gs_info "action=unchanged $*"
}

# Write path from stdin. Backup existing foreign files; skip identical.
gs_install_file() {
    _dest=$1
    _mode=${2:-0644}
    _path=$(gs_path "$_dest")
    _tmp=$(mktemp "${TMPDIR:-/tmp}/ternvale-gs.XXXXXX") || gs_die "mktemp failed"
    cat >"$_tmp" || gs_die "read stdin failed path=$_dest"
    # cksum is POSIX; Fedora container images often omit `cmp` (diffutils).
    if [ -f "$_path" ] && [ "$(cksum <"$_tmp")" = "$(cksum <"$_path")" ]; then
        rm -f "$_tmp"
        gs_unchanged "path=$_dest"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        rm -f "$_tmp"
        gs_info "action=write dry_run=true path=$_dest mode=$_mode"
        CHANGED_COUNT=$((CHANGED_COUNT + 1))
        return 0
    fi
    _dir=$(dirname "$_path")
    mkdir -p "$_dir" || gs_die "mkdir failed path=$_dir"
    if [ -f "$_path" ]; then
        _bak="$(gs_state_dir)/backup$(echo "$_dest" | tr / _)"
        cp -p "$_path" "$_bak" || gs_die "backup failed path=$_dest"
        gs_undo_line "mv -f '$_bak' '$_path'"
    else
        gs_undo_line "rm -f '$_path'"
    fi
    mv "$_tmp" "$_path" || gs_die "write failed path=$_dest"
    chmod "$_mode" "$_path" || true
    gs_changed "path=$_dest mode=$_mode"
}

gs_ensure_dir() {
    _d=$(gs_path "$1")
    if [ -d "$_d" ]; then
        gs_unchanged "path=$1 kind=dir"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=mkdir dry_run=true path=$1"
        CHANGED_COUNT=$((CHANGED_COUNT + 1))
        return 0
    fi
    mkdir -p "$_d" || gs_die "mkdir failed path=$1"
    gs_undo_line "rmdir '$_d' 2>/dev/null || true"
    gs_changed "path=$1 kind=dir"
}

# Idempotent marked block in a text file (fstab, modules, …).
gs_ensure_block() {
    _file=$1
    _block=$2
    _path=$(gs_path "$_file")
    _begin="# ${MARKER} begin"
    _end="# ${MARKER} end"
    if [ -f "$_path" ] && grep -F "$_begin" "$_path" >/dev/null 2>&1; then
        gs_unchanged "path=$_file kind=block"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=append_block dry_run=true path=$_file"
        CHANGED_COUNT=$((CHANGED_COUNT + 1))
        return 0
    fi
    _dir=$(dirname "$_path")
    mkdir -p "$_dir" || gs_die "mkdir failed path=$_dir"
    if [ -f "$_path" ]; then
        _bak="$(gs_state_dir)/backup$(echo "$_file" | tr / _)"
        cp -p "$_path" "$_bak" || gs_die "backup failed path=$_file"
        gs_undo_line "mv -f '$_bak' '$_path'"
    else
        : >"$_path"
        gs_undo_line "rm -f '$_path'"
    fi
    {
        printf '\n%s\n%s\n%s\n' "$_begin" "$_block" "$_end"
    } >>"$_path" || gs_die "append failed path=$_file"
    gs_changed "path=$_file kind=block"
}

gs_has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

gs_has_systemd() {
    [ -d "$(gs_path /usr/lib/systemd/system)" ] || [ -d "$(gs_path /lib/systemd/system)" ]
}
