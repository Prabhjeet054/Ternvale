# Persistent virtiofs mount for tag `rosetta` → /mnt/rosetta.

gs_configure_mount() {
    gs_ensure_dir "$MOUNT"
    if gs_has_systemd; then
        gs_install_systemd_mount
    else
        gs_install_fstab_mount
        if [ "$FAMILY" = alpine ]; then
            gs_install_openrc_mount
        fi
    fi
    gs_try_mount_now
}

gs_install_systemd_mount() {
    # systemd.unit(5): Where=/mnt/rosetta → mnt-rosetta.mount
    _unit="/etc/systemd/system/mnt-rosetta.mount"
    gs_install_file "$_unit" 0644 <<EOF
# ${MARKER}
[Unit]
Description=Ternvale Rosetta virtiofs (${TAG})
DefaultDependencies=no
After=local-fs-pre.target
Before=local-fs.target systemd-binfmt.service

[Mount]
What=${TAG}
Where=${MOUNT}
Type=virtiofs
Options=ro,nofail

[Install]
WantedBy=multi-user.target
EOF
    _wants="/etc/systemd/system/multi-user.target.wants/mnt-rosetta.mount"
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=enable dry_run=true unit=mnt-rosetta.mount"
        CHANGED_COUNT=$((CHANGED_COUNT + 1))
        return 0
    fi
    _wdir=$(dirname "$(gs_path "$_wants")")
    mkdir -p "$_wdir"
    _target=$(gs_path "$_unit")
    _link=$(gs_path "$_wants")
    if [ -L "$_link" ] || [ -f "$_link" ]; then
        gs_unchanged "path=$_wants kind=symlink"
    else
        ln -s "$_unit" "$_link" || gs_die "symlink failed path=$_wants"
        gs_undo_line "rm -f '$_link'"
        gs_changed "path=$_wants kind=symlink"
    fi
    if [ -z "$ROOT" ] && gs_has_cmd systemctl; then
        systemctl daemon-reload 2>/dev/null || gs_warn "action=daemon-reload result=failed"
        systemctl enable mnt-rosetta.mount 2>/dev/null || gs_warn "action=enable unit=mnt-rosetta.mount result=failed"
    fi
}

gs_install_fstab_mount() {
    gs_ensure_block /etc/fstab "${TAG} ${MOUNT} virtiofs ro,nofail 0 0"
}

gs_install_openrc_mount() {
    gs_install_file /etc/local.d/ternvale-rosetta.start 0755 <<EOF
#!/bin/sh
# ${MARKER}
mkdir -p ${MOUNT}
if ! grep -q " ${MOUNT} " /proc/mounts 2>/dev/null; then
  mount -t virtiofs ${TAG} ${MOUNT} || true
fi
EOF
    gs_ensure_dir /etc/local.d
    if [ -z "$ROOT" ] && gs_has_cmd rc-update; then
        if [ "$DRY_RUN" -eq 1 ]; then
            gs_info "action=rc-update dry_run=true service=local"
        else
            rc-update add local default 2>/dev/null || gs_warn "action=rc-update service=local result=failed"
        fi
    fi
}

gs_try_mount_now() {
    if [ -n "$ROOT" ]; then
        gs_debug "action=mount skipped=chroot"
        return 0
    fi
    if grep -q " ${MOUNT} " /proc/mounts 2>/dev/null; then
        gs_unchanged "path=$MOUNT kind=mount"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        gs_info "action=mount dry_run=true tag=$TAG path=$MOUNT"
        return 0
    fi
    mkdir -p "$MOUNT" || true
    if mount -t virtiofs "$TAG" "$MOUNT" 2>/dev/null; then
        gs_changed "path=$MOUNT kind=mount-now tag=$TAG"
        gs_undo_line "umount '$MOUNT' 2>/dev/null || true"
    else
        gs_warn "action=mount-now result=failed tag=$TAG hint='virtiofs share missing until reboot or rosetta=true'"
    fi
}
