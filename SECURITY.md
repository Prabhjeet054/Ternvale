# Security policy

Ternvale treats the guest as hostile. A bug in a device model, parser, or
privileged helper can become a guest escape or host file access. Please report
vulnerabilities privately.

## Contact

1. **Preferred:** open a private GitHub security advisory on this repository.
2. **Email:** `security@ternvale.dev` (same domain as the update feed
   `https://updates.ternvale.dev/latest.json`). If that mailbox is not yet
   live, use the GitHub advisory.

Do not file a public issue, PR, or discussion for an unfixed vulnerability.

## What to include

- Affected Ternvale version / git revision and host macOS version.
- Guest OS (Linux / Windows) and a minimal config that reproduces the issue.
- Impact you believe you have (guest escape, host file read/write, DoS of the
  VMM, info leak).
- Proof of concept **without** exploit packing for unrelated software. Guest
  payloads that exercise Ternvale parsers are welcome.

## Scope

In scope: the VMM process, control socket, guest agent protocol, virtio and
MMIO device models, virtio-fs, USB passthrough, the privileged vmnet helper,
snapshot/NVRAM/TPM on-disk formats, and the update-feed parser.

Out of scope unless they break Ternvale’s isolation: bugs only in the guest OS
or in Apple Hypervisor.framework; social engineering; physical access to an
unlocked Mac already running Ternvale as the same user.

## Process

We aim to acknowledge a report within 7 days and to share a fix-or-mitigation
plan within 30 days. Please do not disclose publicly until a release containing
the fix is out, or 90 days after the first report, whichever comes first.

See [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for trust boundaries and
[docs/SECURITY_GAPS.md](docs/SECURITY_GAPS.md) for known coverage gaps
(backlog for Steps 79–82).
