# Foreign x86_64 userspace. Debian/Ubuntu: dpkg amd64. Others: document only.

gs_configure_multiarch() {
    case "$FAMILY" in
    debian) gs_debian_multiarch ;;
    fedora) gs_fedora_multiarch_docs ;;
    alpine) gs_alpine_multiarch_docs ;;
    esac
}

gs_debian_multiarch() {
    gs_warn "kind=multiarch family=debian msg='mixed arm64+amd64 apt can pull the wrong arch of a library and break the guest; pin with apt-get install pkg:arm64 if a conflict appears'"
    _arch_file=/var/lib/dpkg/arch
    _path=$(gs_path "$_arch_file")
    _native=${OS_ARCH:-arm64}
    if [ -z "$ROOT" ] && gs_has_cmd dpkg; then
        _native=$(dpkg --print-architecture 2>/dev/null || echo arm64)
        if dpkg --print-foreign-architectures 2>/dev/null | grep -qx amd64; then
            gs_unchanged "kind=dpkg-arch arch=amd64"
        elif [ "$DRY_RUN" -eq 1 ]; then
            gs_info "action=dpkg --add-architecture amd64 dry_run=true"
            CHANGED_COUNT=$((CHANGED_COUNT + 1))
        else
            dpkg --add-architecture amd64 || gs_die "dpkg --add-architecture amd64 failed"
            gs_changed "kind=dpkg-arch arch=amd64"
            gs_undo_line "dpkg --remove-architecture amd64 2>/dev/null || true"
        fi
    else
        if [ -f "$_path" ] && grep -qx amd64 "$_path"; then
            gs_unchanged "kind=dpkg-arch arch=amd64 path=$_arch_file"
        else
            if [ -f "$_path" ]; then
                _body=$(cat "$_path")
                case "$_body" in
                *amd64*)
                    gs_unchanged "kind=dpkg-arch arch=amd64"
                    return 0
                    ;;
                esac
                _body=$(printf '%s\namd64\n' "$_body")
            else
                _body=$(printf '%s\namd64\n' "$_native")
            fi
            printf '%s' "$_body" | gs_install_file "$_arch_file" 0644
        fi
    fi
    gs_debian_apt_sources
}

gs_debian_apt_sources() {
    # Ubuntu aarch64 uses ports.ubuntu.com (no amd64). Add archive.ubuntu.com.
    case " $OS_ID $OS_ID_LIKE " in
    *" ubuntu "*)
        _suite=${OS_VERSION_CODENAME:-}
        if [ -z "$_suite" ]; then
            gs_warn "kind=apt-amd64 result=skipped hint='no VERSION_CODENAME; add archive.ubuntu.com [arch=amd64] by hand'"
            return 0
        fi
        gs_install_file /etc/apt/sources.list.d/ternvale-amd64.list 0644 <<EOF
# ${MARKER}
# amd64 packages for Ubuntu-on-ARM. Do not use ports.ubuntu.com for amd64.
deb [arch=amd64] http://archive.ubuntu.com/ubuntu ${_suite} main restricted universe multiverse
deb [arch=amd64] http://archive.ubuntu.com/ubuntu ${_suite}-updates main restricted universe multiverse
deb [arch=amd64] http://security.ubuntu.com/ubuntu ${_suite}-security main restricted universe multiverse
EOF
        gs_warn "kind=apt-ports msg='if apt update errors on amd64 against ports.ubuntu.com, add [arch=arm64] to those existing lines'"
        ;;
    *)
        gs_info "kind=apt family=debian msg='dpkg amd64 is set; official Debian mirrors usually serve amd64 from the same URLs. Run apt-get update.'"
        ;;
    esac
}

gs_fedora_multiarch_docs() {
    gs_install_file /usr/share/doc/ternvale/MULTIARCH-FEDORA.txt 0644 <<'EOF'
# ${MARKER}
Ternvale does not enable a full x86_64 DNF repo (mixed-arch Fedora often
conflicts). After Rosetta binfmt is registered, static x86_64 binaries run
without extra packages.

Optional (you run these):
  dnf install glibc.x86_64 libgcc.x86_64
  dnf --forcearch=x86_64 install <pkg>

If those packages are missing from the aarch64 mirror, add an x86_64 Fedora
repository and keep native packages pinned to aarch64.
EOF
    gs_info "kind=multiarch family=fedora documented=true path=/usr/share/doc/ternvale/MULTIARCH-FEDORA.txt"
}

gs_alpine_multiarch_docs() {
    _ver=${OS_VERSION_ID:-}
    _rel=${_ver%.*}
    [ -n "$_rel" ] || _rel="latest-stable"
    gs_install_file /usr/share/doc/ternvale/MULTIARCH-ALPINE.txt 0644 <<EOF
# ${MARKER}
apk is not a mixed-arch package manager. Prefer static x86_64 binaries under
Rosetta binfmt.

Optional separate x86_64 root (do not mix into /):
  mkdir -p /opt/x86_64
  apk --arch x86_64 --root /opt/x86_64 --initdb add \\
    --repository https://dl-cdn.alpinelinux.org/alpine/v${_rel}/main \\
    --repository https://dl-cdn.alpinelinux.org/alpine/v${_rel}/community \\
    musl libc-utils
EOF
    gs_info "kind=multiarch family=alpine documented=true path=/usr/share/doc/ternvale/MULTIARCH-ALPINE.txt"
}
