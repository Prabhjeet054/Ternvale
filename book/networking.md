# Networking modes

Each `[[nics]]` entry is a virtio-net device.

| `backend` | What it does | Privileges |
| --- | --- | --- |
| `user` | Userspace NAT (SLIRP-style). Guest `10.0.2.15/24`, gateway `10.0.2.2`. | None |
| `loopback` | Fake gateway only (ARP/ICMP to `10.0.2.2`). No TCP to the host LAN. | None |
| `vmnet` | Apple vmnet. `mode`: `shared` (NAT), `host-only`, `bridged`. | Restricted helper (`ternvale-vmnetd`) |

Example:

```toml
[[nics]]
backend = "user"

[[nics]]
id = 1
backend = "vmnet"
mode = "shared"
```

Bridged mode needs a real host interface and may prompt for extra permission.
If the helper is missing or denied, the VM can crash on start; the app shows
that in the crash summary.

The app **Settings…** sheet edits NIC backend and vmnet mode only (User NAT,
Loopback, vmnet shared / host-only / bridged).

Policy knobs (`allow_lan`, anti-spoof, deny lists) apply to the user-mode
backend. Packet traces: `TERNVALE_LOG=ternvale::net=trace`. Extra notes for
developers: `docs/NET.md` in the source tree if present.
