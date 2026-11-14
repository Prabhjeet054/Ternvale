# Distro detection from os-release. Sourced.

gs_read_os_release() {
    _f=$(gs_path /etc/os-release)
    if [ ! -f "$_f" ]; then
        _f=$(gs_path /usr/lib/os-release)
    fi
    if [ ! -f "$_f" ]; then
        gs_die "reason=no-os-release path=/etc/os-release hint='unsupported: need ID in os-release'"
    fi
    OS_ID=
    OS_ID_LIKE=
    OS_VERSION_ID=
    OS_VERSION_CODENAME=
    OS_PRETTY=
    # shellcheck disable=SC1090
    # Parse KEY=VAL without executing unknown shell.
    while IFS='=' read -r _k _v; do
        case "$_k" in
        ID | ID_LIKE | VERSION_ID | VERSION_CODENAME | PRETTY_NAME) ;;
        *) continue ;;
        esac
        _v=${_v#\"}
        _v=${_v%\"}
        _v=${_v#\'}
        _v=${_v%\'}
        case "$_k" in
        ID) OS_ID=$_v ;;
        ID_LIKE) OS_ID_LIKE=$_v ;;
        VERSION_ID) OS_VERSION_ID=$_v ;;
        VERSION_CODENAME) OS_VERSION_CODENAME=$_v ;;
        PRETTY_NAME) OS_PRETTY=$_v ;;
        esac
    done <"$_f"
    gs_debug "os_id=$OS_ID id_like=$OS_ID_LIKE version=$OS_VERSION_ID"
}

# FAMILY: debian | fedora | alpine
gs_detect_family() {
    gs_read_os_release
    _like=" ${OS_ID} ${OS_ID_LIKE} "
    case "$_like" in
    *" debian "* | *" ubuntu "* | *" linuxmint "* | *" pop "* | *" raspbian "*)
        FAMILY=debian
        ;;
    *" fedora "* | *" rhel "* | *" centos "* | *" rocky "* | *" alma "* | *" almalinux "* | *" nobara "*)
        FAMILY=fedora
        ;;
    *" alpine "*)
        FAMILY=alpine
        ;;
    *)
        gs_die "reason=unsupported-distro id=${OS_ID:-unknown} id_like=${OS_ID_LIKE:-} supported='debian ubuntu fedora rhel centos rocky alma alpine' hint='install x86 binfmt by hand or use a supported guest'"
        ;;
    esac
    gs_info "action=detect family=$FAMILY id=$OS_ID pretty=${OS_PRETTY:-$OS_ID}"
}
