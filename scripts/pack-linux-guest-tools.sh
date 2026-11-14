#!/bin/sh
# Stage Linux guest tools (setup helper + FEX binfmt). Does not include RootFS.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
dest="${1:-$root/target/linux-guest-tools}"
prefix="${FEX_PREFIX:-$root/target/fex/prefix}"

rm -rf "$dest"
mkdir -p "$dest/usr/local/sbin" \
  "$dest/usr/local/lib/ternvale/guest-setup" \
  "$dest/usr/local/lib/ternvale/fex" \
  "$dest/usr/share/doc/ternvale"

cp "$root/guest-tools/ternvale-guest-setup" "$dest/usr/local/sbin/"
chmod 0755 "$dest/usr/local/sbin/ternvale-guest-setup"
cp "$root/guest-tools/lib/guest-setup/"*.sh "$dest/usr/local/lib/ternvale/guest-setup/"
cp "$root/guest-tools/fex/register-binfmt.sh" "$dest/usr/local/lib/ternvale/fex/"
chmod 0755 "$dest/usr/local/lib/ternvale/fex/register-binfmt.sh"
cp "$root/guest-tools/fex/README.md" "$dest/usr/share/doc/ternvale/FEX.md"
cp "$root/docs/X86.md" "$dest/usr/share/doc/ternvale/X86.md"

mkdir -p "$dest/usr/bin"
copied=0
for bin in FEXInterpreter FEX FEXBash FEXMountImage; do
  if [ -x "$prefix/bin/$bin" ]; then
    cp "$prefix/bin/$bin" "$dest/usr/bin/"
    chmod 0755 "$dest/usr/bin/$bin"
    copied=1
  fi
done
if [ "$copied" -eq 0 ]; then
  printf 'pack-linux-guest-tools: no FEX/FEXInterpreter at %s (PPA or scripts/build-fex.sh)\n' "$prefix" >&2
fi

# Explicitly omit RootFS / squashfs.
find "$dest" -name '*.sqsh' -print -delete | while read -r gone; do
  printf 'pack-linux-guest-tools: removed bundled squashfs %s\n' "$gone" >&2
done

tar -C "$dest" -czf "$dest.tar.gz" .
printf 'pack-linux-guest-tools: %s and %s.tar.gz\n' "$dest" "$dest" >&2
