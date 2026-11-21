# License signing (private)

The Ed25519 **seed / signing key must never be committed**. Keep it in
Keychain, an HSM, or a directory listed in `.gitignore` (`/secrets/`).

The in-app verifier uses only `ternvale-license::pubkey::EMBEDDED_PUBLIC_HEX`.

On a machine that holds the seed:

```sh
# Once: 32-byte seed (do not print or copy into git)
cargo run -p ternvale-license --bin ternvale-license-sign -- --gen-seed secrets/license.seed

# Issue a key
cargo run -p ternvale-license --bin ternvale-license-sign -- \
  --key secrets/license.seed --edition pro --seats 1 --days 365 --id LIC-001 \
  --out secrets/LIC-001.tvlk
```

Print the verifying key from a local tool and paste it into
`ternvale-license/src/pubkey.rs` before shipping. Never paste the seed.
