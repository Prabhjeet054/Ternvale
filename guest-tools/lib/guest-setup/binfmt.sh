# Persistent Apple Rosetta binfmt_misc registration.

gs_binfmt_line() {
    printf ':rosetta:M::%s:%s:%s:%s' "$BINFMT_MAGIC" "$BINFMT_MASK" "$INTERPRETER" "$BINFMT_FLAGS"
}

gs_configure_binfmt() {
    gs_install_file /etc/binfmt.d/ternvale-rosetta.conf 0644 <<EOF
# ${MARKER}
# Apple Linux Rosetta magic/mask; flags PCF = preserve + credentials + fix-binary.
$(gs_binfmt_line)
EOF
    gs_install_file /etc/modules-load.d/ternvale-binfmt.conf 0644 <<EOF
# ${MARKER}
binfmt_misc
EOF
    case "$FAMILY" in
    alpine)
        gs_install_alpine_binfmt
        ;;
    *)
        gs_maybe_update_binfmts
        ;;
    esac
    gs_register_binfmt_now
}

gs_install_alpine_binfmt() {
    gs_install_file /etc/local.d/ternvale-binfmt.start 0755 <<EOF
#!/bin/sh
# ${MARKER}
modprobe binfmt_misc 2>/dev/null || true
if [ ! -d /proc/sys/fs/binfmt_misc ]; then
  exit 0
fi
if ! grep -q binfmt_misc /proc/mounts 2>/dev/null; then
  mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true
fi
if [ -e /proc/sys/fs/binfmt_misc/rosetta ]; then
  exit 0
fi
if [ -x ${INTERPRETER} ] && [ -w /proc/sys/fs/binfmt_misc/register ]; then
  printf '%s' '$(gs_binfmt_line)' >/proc/sys/fs/binfmt_misc/register || true
fi
EOF
    gs_ensure_block /etc/modules "binfmt_misc"
}

gs_maybe_update_binfmts() {
    if [ -n "$ROOT" ]; then
        return 0
    fi
    if ! gs_has_cmd update-binfmts; then
        gs_debug "action=update-binfmts skipped=missing"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=update-binfmts dry_run=true name=rosetta"
        return 0
    fi
    if update-binfmts --display rosetta >/dev/null 2>&1; then
        gs_unchanged "kind=update-binfmts name=rosetta"
        return 0
    fi
    # Debian binfmt-support. Apple snippet: credentials/preserve/fix-binary.
    if update-binfmts --install rosetta "$INTERPRETER" \
        --magic "$BINFMT_MAGIC" \
        --mask "$BINFMT_MASK" \
        --credentials yes --preserve "$PRESERVE" --fix-binary yes; then
        gs_changed "kind=update-binfmts name=rosetta"
        gs_undo_line "update-binfmts --remove rosetta '$INTERPRETER' 2>/dev/null || true"
    else
        gs_warn "action=update-binfmts result=failed hint='systemd-binfmt file still installed'"
    fi
}

gs_register_binfmt_now() {
    if [ -n "$ROOT" ]; then
        return 0
    fi
    _reg=/proc/sys/fs/binfmt_misc/register
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=binfmt-register dry_run=true"
        return 0
    fi
    if [ -e /proc/sys/fs/binfmt_misc/rosetta ]; then
        gs_unchanged "kind=binfmt name=rosetta"
        return 0
    fi
    if [ ! -w "$_reg" ]; then
        gs_warn "action=binfmt-register result=skipped hint='no binfmt_misc; reboot after modules-load.d'"
        return 0
    fi
    if [ ! -x "$INTERPRETER" ]; then
        gs_warn "action=binfmt-register result=skipped interpreter=$INTERPRETER hint='mount Rosetta share first'"
        return 0
    fi
    # Kernel parses \xHH in the register string; do not decode on the host.
    if printf '%s' "$(gs_binfmt_line)" >"$_reg"; then
        gs_changed "kind=binfmt-register name=rosetta"
        gs_undo_line "echo -1 >/proc/sys/fs/binfmt_misc/rosetta 2>/dev/null || true"
    else
        gs_warn "action=binfmt-register result=failed"
    fi
}
