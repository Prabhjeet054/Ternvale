# Partner Center attestation (production signing)

Verified against Microsoft Learn **Attestation sign Windows drivers** and
**Driver code signing requirements** (retrieved 2026-11-19):

- https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation
- https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-reqs
- https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/driver-signing-offerings

TODO(verify): re-read those pages before each release; Microsoft changes
dashboard options.

## What attestation is

Microsoft **attestation signing** is for Windows 10/11 Desktop (including
**ARM64** kernel-mode packages). It is **not** WHCP certification and **must
not** be published to Windows Update for retail. It is the production path
Ternvale uses so a Windows 11 ARM64 guest with Secure Boot / integrity checks
can load `tvgpu-dod` / `tvhello` without test-signing.

## Prerequisites

1. **Hardware Developer Program** account in Partner Center.
2. An **EV (extended validation) code signing certificate** associated with
   that account. Microsoft lists CAs (DigiCert, Sectigo, GlobalSign, …).
   SHA-256 only (`signtool /fd sha256`).
3. You may sign the *submission CAB* with that EV cert (recommended) or with
   an Authenticode cert **registered** on the same dashboard account.
4. Windows ADK / WDK on the packaging machine (`makecab`, `inf2cat`).

**Never** put the EV PFX, password, or hardware token PIN in git, logs, or
`TERNVALE_LOG`. Use the guest keychain, CI secret store, or a hardware token.

## Package contents (per driver folder)

Each folder must be ARM64-only (same architecture set), name **&lt; 40
characters**, no UNC paths:

- `tvgpu-dod.sys` / `tvhello.sys`
- matching `.inf` (DCH / Universal)
- `.pdb` (Microsoft crash analysis)
- `.cat` (placeholder; Microsoft **regenerates** catalogs)

## Script

```powershell
.\drivers\release\package.ps1 -Config Release
.\drivers\release\prepare-attestation.ps1
# EV-sign the CAB with the vendor's recommended SignTool line, then:
```

## Manual submission

1. Sign in to [Partner Center hardware dashboard](https://partner.microsoft.com/dashboard).
2. **Submit new hardware**. Product name e.g. `Ternvale ARM64 guest drivers`.
3. Leave both **test-signing** options **unchecked** for a production
   attestation signature.
4. **Requested signatures**: include the Windows 11 / ARM64 attestation
   options offered in the form (labels change; pick current Desktop ARM64).
5. Upload the EV-signed CAB.
6. When finished, download the Microsoft-signed package. Copy it to
   `drivers/release/dist/` and rebuild `ternvale tools build-iso` so the
   tools ISO carries the attested bits.

Attestation-signed drivers are **not** Windows Certified. ELAM / Windows Hello
PE levels are out of scope (we do not ship those binaries).
