#!/bin/sh
# Register FEXInterpreter for i386 and x86_64 ELF via binfmt_misc.
# Magic/mask: ELF class + machine (i386 0x03, x86_64 0x3e), same family as
# FEX's packaged binfmt files. TODO(verify) against the pinned FEX tag.
set -eu

if [ -z "${FEX_INTERPRETER:-}" ]; then
  if [ -x /usr/bin/FEXInterpreter ]; then
    INTERPRETER=/usr/bin/FEXInterpreter
  elif [ -x /usr/bin/FEX ]; then
    INTERPRETER=/usr/bin/FEX
  else
    INTERPRETER=/usr/bin/FEXInterpreter
  fi
else
  INTERPRETER="$FEX_INTERPRETER"
fi
BINFMT_D="${ROOT:-}/etc/binfmt.d"

x64_magic='\\x7fELF\\x02\\x01\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x02\\x00\\x3e\\x00'
x64_mask='\\xff\\xff\\xff\\xff\\xff\\xfe\\xfe\\x00\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xfe\\xff\\xff\\xff'
i386_magic='\\x7fELF\\x01\\x01\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x02\\x00\\x03\\x00'
i386_mask='\\xff\\xff\\xff\\xff\\xff\\xfe\\xfe\\x00\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xfe\\xff\\xff\\xff'

if [ ! -x "$INTERPRETER" ]; then
  echo "WARN target=ternvale::compat reason=missing-interpreter path=$INTERPRETER" >&2
  exit 1
fi

mkdir -p "$BINFMT_D"
# Flags PCF: preserve argv0, credentials, fix-binary (FEX packaged files).
printf ':FEX-x86_64:M::%s:%s:%s:PCF\n' "$x64_magic" "$x64_mask" "$INTERPRETER" \
  >"$BINFMT_D/FEX-x86_64.conf"
printf ':FEX-i386:M::%s:%s:%s:PCF\n' "$i386_magic" "$i386_mask" "$INTERPRETER" \
  >"$BINFMT_D/FEX-i386.conf"

if [ -z "${ROOT:-}" ] && [ -w /proc/sys/fs/binfmt_misc/register ]; then
  echo "INFO target=ternvale::compat action=binfmt-reload" >&2
  if command -v systemctl >/dev/null 2>&1; then
    systemctl restart systemd-binfmt 2>/dev/null || true
  fi
fi

echo "INFO target=ternvale::compat action=fex-binfmt interpreter=$INTERPRETER" >&2
