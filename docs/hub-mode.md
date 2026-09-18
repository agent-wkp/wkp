# Hub mode

`wkp-hub` is a separate binary (a distinct CI target from `wkp`, so the
laptop CLI never links the Postgres client or control-plane code) that
gives you a shared, always-on service instead of picking and managing
your own git host — device registration and revocation over mTLS,
per-tenant hard isolation (a container-runtime pod per tenant), and
optional server-side indexing.

## Should you use this, or [multi-machine sync](multi-machine-sync.md)?

Multi-machine sync against a private git repo covers "my own devices,
staying in sync" with zero extra infrastructure. Reach for hub mode
instead when you want:

- **Device revocation** — a lost laptop's access ends on its next
  connection attempt, enforced by the hub's own mTLS certificate
  verifier, not by trusting every device to behave.
- **Multiple people or teams** sharing tenants without each managing
  git-host access themselves.
- A control plane that can grow server-side indexing or other
  hub-specific features later (design section 8) — not something a
  plain git remote gives you.

If neither of those matters to you, multi-machine sync is genuinely
simpler and has nothing else to deploy.

## Deploying the hub

The hub needs Postgres (control plane) and a container runtime it can
orchestrate tenant pods through (`podman` today; `WKP_HUB_POD_ORCHESTRATOR`
reserves the name `kubernetes` for later, not yet implemented — see
ADR-0012). [`deploy/hub/Containerfile`](../deploy/hub/Containerfile)
builds the image; [`deploy/hub/entrypoint.sh`](../deploy/hub/entrypoint.sh)
is what actually runs.

```bash
podman build -f deploy/hub/Containerfile -t wkp-hub .
```

**Environment variables** `wkp-hub serve` reads:

| Variable | Required | Default | What |
|---|---|---|---|
| `DATABASE_URL` | Yes | — | Postgres connection string for the control plane |
| `WKP_HUB_PORT` | No | `8443` (container) | HTTPS listen port |
| `WKP_HUB_CA_DIR` | No | `/srv/wkp-hub/ca` | Where the hub's own root CA cert + `0600` key live — generated on first boot if missing, persisted so a restart doesn't invalidate every certificate already issued |
| `WKP_HUB_REPOS_ROOT` | No | `/srv/wkp-hub/repos` | Where per-tenant bare repos live |
| `WKP_HUB_TENANT_IMAGE` | No | `localhost/wkp-hub` | The image a tenant's own pod runs (the same image you just built, in `serve-tenant` mode) |
| `WKP_HUB_POD_ORCHESTRATOR` | No | `podman` | Only `podman` is implemented today |

```bash
podman run -d --name wkp-hub -p 8443:8443 \
  -e DATABASE_URL="postgres://user:pass@host/wkp_hub" \
  -v wkp-hub-ca:/srv/wkp-hub/ca \
  -v wkp-hub-repos:/srv/wkp-hub/repos \
  wkp-hub
```

Schema applies automatically on first connect — `wkp-hub migrate` just
confirms it (`wkp-hub: schema is up to date`), there's no separate
migration-file step to run by hand.

## Create a tenant

```bash
wkp-hub tenant create acme
```

Creates both the control-plane row and the tenant's own bare repo in
one command — a tenant isn't actually usable (nothing to push or pull)
until both exist.

## Register a device

On the hub, get the CA root a device needs to trust:

```bash
wkp-hub ca-cert > hub-ca.pem
```

Hand `hub-ca.pem` to whoever's registering a device (out of band — it's
public, not a secret, but there's no discovery mechanism for it yet).
On the device:

```bash
wkp hub register --hub-url https://hub.example.com:8443 \
  --tenant acme --ca-cert ./hub-ca.pem
```

Runs the OAuth 2.0 Device Authorization Grant (RFC 8628) — no password,
no browser redirect handled by the CLI. The device generates an Ed25519
key (OS keystore: macOS Keychain, Linux `secret-service`, falling back
to `ssh-agent`), submits a CSR built from it, and the hub's own CA signs
and returns a certificate. From here, `wkp sync`/`wkpd` against this hub
work exactly like [multi-machine sync](multi-machine-sync.md) does
against any other git remote — the hub's `origin` is just another git
server that happens to also do mTLS and revocation.

## Revoke a device

```bash
wkp-hub device revoke-id <device-id>
```

Effective on that device's *next* connection attempt — the mTLS
`ClientCertVerifier` checks revocation state on every TLS handshake.
For an incident where "next connection" isn't fast enough (a leaked
credential, a suspected CA compromise), `wkp-hub reset-all-connections
--by <label>` force-closes already-open connections too, not just
future ones.

## Known gap: Debian/Ubuntu client git

**If you're registering a device running Debian or Ubuntu, `wkp hub
register` will succeed but every subsequent `git push`/`git fetch` over
mTLS will fail.** Real, reproduced, tracked as
[issue #136](https://github.com/agent-wkp/wkp/issues/136) — Debian/Ubuntu's
`git` links against GnuTLS rather than OpenSSL, and GnuTLS's
client-certificate loading path can't parse the Ed25519 key/certificate
this flow issues today (`error reading X.509 key or certificate file`).
Not yet fixed; the likely fix (re-keying to ECDSA P-256, which works
against both OpenSSH signing and GnuTLS) is a real decision, not yet
made. No workaround exists for a real Debian/Ubuntu user today beyond
running git from a different distro's build.

## Reference

- [`docs/design/wkp-hub-design-v0.1.md`](design/wkp-hub-design-v0.1.md)
  section 8 — the full hub architecture and threat model this doc
  summarizes operationally.
- [`docs/adr/0009-per-tenant-pod-isolation.md`](adr/0009-per-tenant-pod-isolation.md),
  [`0011-drop-ssh-mtls-transport.md`](adr/0011-drop-ssh-mtls-transport.md),
  [`0012-pod-orchestrator-abstraction.md`](adr/0012-pod-orchestrator-abstraction.md) —
  the specific decisions behind pod-per-tenant isolation, the mTLS
  transport, and the pluggable orchestrator.
- [`deploy/hub/test-mtls-integration.sh`](../deploy/hub/test-mtls-integration.sh) —
  the real, CI-verified register → push → fetch → revoke → push-must-fail
  flow this doc describes, run end to end against a real hub.
- [`docs/cli-reference.md`](cli-reference.md#hub) — the `wkp hub
  register`/`wkp wkpd` flag reference.
