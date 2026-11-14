# Docker / Podman: wait for Rosetta mount + binfmt. No qemu-user-static.

gs_configure_containers() {
    _docker=0
    _podman=0
    if [ -n "$ROOT" ]; then
        if [ -x "$(gs_path /usr/bin/docker)" ] || [ -x "$(gs_path /usr/bin/dockerd)" ] || [ -d "$(gs_path /etc/docker)" ]; then
            _docker=1
        fi
        if [ -x "$(gs_path /usr/bin/podman)" ] || [ -d "$(gs_path /etc/containers)" ]; then
            _podman=1
        fi
    else
        if gs_has_cmd docker || gs_has_cmd dockerd || [ -d /etc/docker ]; then
            _docker=1
        fi
        if gs_has_cmd podman || [ -d /etc/containers ]; then
            _podman=1
        fi
    fi
    if [ "$_docker" -eq 0 ] && [ "$_podman" -eq 0 ]; then
        gs_info "action=containers result=skipped hint='docker/podman not detected'"
        return 0
    fi
    gs_install_file /etc/profile.d/ternvale-rosetta.sh 0644 <<EOF
# ${MARKER}
# Kernel binfmt (Rosetta) runs linux/amd64 images. Do not install qemu-user-static
# alongside it — the two handlers fight for the same ELF magic.
#   docker run --platform linux/amd64 alpine uname -m
#   podman run --arch=amd64 alpine uname -m
EOF
    if [ "$_docker" -eq 1 ]; then
        gs_container_dropin docker.service
        gs_warn "kind=docker msg='do not install qemu-user-static; it registers a second x86_64 binfmt'"
        gs_info "kind=docker cmd='docker run --platform linux/amd64 …'"
    fi
    if [ "$_podman" -eq 1 ]; then
        gs_container_dropin podman.service
        gs_info "kind=podman cmd='podman run --arch=amd64 …'"
    fi
}

gs_container_dropin() {
    _svc=$1
    _drop="/etc/systemd/system/${_svc}.d/ternvale-rosetta.conf"
    if ! gs_has_systemd; then
        gs_debug "action=dropin skipped=no-systemd service=$_svc"
        return 0
    fi
    gs_install_file "$_drop" 0644 <<EOF
# ${MARKER}
[Unit]
After=mnt-rosetta.mount systemd-binfmt.service
RequiresMountsFor=${MOUNT}
EOF
}
