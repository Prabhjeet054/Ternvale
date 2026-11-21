# Headless daemon

`ternvaled` runs the control hub and an HTTPS gateway on **localhost**
(`127.0.0.1:8443`) for Mac minis and CI. See `docs/GATEWAY.md` in the source
tree for bind policy, token scopes, certificate rotation, and REST paths.

Remote display streaming is not available yet; use serial WebSocket and PNG
screenshots, or a local viewer.
