#!/bin/sh
# Add translated-process / failure counts for qemu-user (or another engine).
# Host scrapes /var/lib/ternvale/x86-metrics.json via the agent.
set -eu

ENGINE=qemu-user
PROCESSES=0
FAILURES=0
FILE=/var/lib/ternvale/x86-metrics.json

while [ $# -gt 0 ]; do
    case "$1" in
    --engine)
        shift
        ENGINE=$1
        ;;
    --processes)
        shift
        PROCESSES=$1
        ;;
    --failures)
        shift
        FAILURES=$1
        ;;
    --file)
        shift
        FILE=$1
        ;;
    *)
        echo "ERROR target=ternvale::compat reason=unknown-arg arg=$1" >&2
        exit 2
        ;;
    esac
    shift
done

case "$ENGINE" in
rosetta | fex | qemu-user) ;;
*)
    echo "WARN target=ternvale::compat reason=unknown-engine engine=$ENGINE" >&2
    exit 1
    ;;
esac

dir=$(dirname "$FILE")
mkdir -p "$dir"
if [ ! -f "$FILE" ]; then
    printf '%s\n' "{\"engine\":\"$ENGINE\",\"processes\":0,\"failures\":0}" >"$FILE"
fi

# POSIX: rewrite the three-field JSON file (guest-hostile values already bounded).
old=$(cat "$FILE")
old_p=$(printf '%s' "$old" | sed -n 's/.*"processes":[[:space:]]*\([0-9][0-9]*\).*/\1/p')
old_f=$(printf '%s' "$old" | sed -n 's/.*"failures":[[:space:]]*\([0-9][0-9]*\).*/\1/p')
old_p=${old_p:-0}
old_f=${old_f:-0}
new_p=$((old_p + PROCESSES))
new_f=$((old_f + FAILURES))
printf '{"engine":"%s","processes":%s,"failures":%s}\n' "$ENGINE" "$new_p" "$new_f" >"$FILE"
echo "INFO target=ternvale::compat engine=$ENGINE processes=$new_p failures=$new_f" >&2
cat "$FILE"
