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
  -e DATABASE_URL="postgres://user:REDACTED@host/wkp_hub" \
  -v wkp-hub-ca:/srv/wkp-hub/ca \
  -v wkp-hub-repos:/srv/wkp-hub/repos \
  wkp-hub
```

**Don't put the real password on the command line as shown above** — it
lands in shell history and process listings. `wkp-hub serve` reads
`DATABASE_URL` from its process environment and has no other input
path today (no `--database-url` flag, no file-based secret option) —
so supply the real value through whatever secret-injection mechanism
your own deployment already uses for other services (a systemd
credential, a Kubernetes `Secret`, a `podman secret` mounted and read
by a wrapper script), not typed inline.

Schema applies automatically on first connect — `wkp-hub migrate` just
confirms it (`wkp-hub: schema is up to date`), there's no separate
migration-file step to run by hand.

## Deploying on Kubernetes/OpenShift instead of a single podman host

`WKP_HUB_POD_ORCHESTRATOR=kubernetes` (issue #141,
[`docs/adr/0012-pod-orchestrator-abstraction.md`](adr/0012-pod-orchestrator-abstraction.md)'s
2026-09-21 addendum) runs the front door as an ordinary Kubernetes/
OpenShift workload instead of a single podman host, with each tenant
isolated across **two** Pods (`serve`, `index`) rather than podman's one
pod/two containers, a shared `NetworkPolicy`-isolated storage volume
instead of per-tenant PersistentVolumeClaims, and a `Service` per tenant.
This backend only supports **in-cluster** authentication — `wkp-hub` must
itself run as a Pod in the cluster it manages, using that Pod's own
projected ServiceAccount token; there is no support for pointing it at a
remote cluster via a `~/.kube/config`-style kubeconfig from outside.

Three things beyond `serve`'s usual environment variables:

1. **RBAC**: a namespaced `ServiceAccount` bound to a `Role` granting
   `get`/`list`/`watch`/`create`/`delete` on `pods`, `services`,
   `persistentvolumeclaims`, and `networkpolicies` in the one namespace
   `wkp-hub` runs in — never cluster-admin, never a `ClusterRole`.
   `persistentvolumeclaims` is retained even though this orchestrator no
   longer creates one per tenant — `kubectl apply` still reads the
   shared, pre-existing one as part of applying the Pod specs that
   reference it.
2. **The shared RWX storage PVC must already exist** —
   `paperless-ink/infra`'s `manifests/70-shared-tenant-storage.yaml` (a
   `PersistentVolumeClaim` named `tenant-repos-shared` by default,
   overridable via `WKP_HUB_K8S_SHARED_PVC`), backed by that repo's
   `containers/nfs-server/` — this orchestrator only ever mounts it (via
   a per-tenant `subPath`), it never creates it. This cluster's only
   StorageClass is `ReadWriteOnce`-only, so a tenant's `serve` and
   `index` Pods — two separate Pods needing concurrent access to the
   same data — need this shared `ReadWriteMany` volume; see that repo's
   `containers/nfs-server/README.md` for why a plain NFS server backs
   it, not a CSI driver or a per-tenant volume.
3. **The image itself needs `kubectl`** (and `psql`, for the manual
   lifecycle test below) — both already bundled by
   [`deploy/hub/Containerfile`](../deploy/hub/Containerfile).

No client library is involved — `KubernetesOrchestrator` shells out to
`kubectl` exactly the way the podman backend shells out to `podman`; see
the ADR addendum linked above for the full reasoning behind every choice
(why two Pods and not one with two containers, why a shared PVC and not
one per tenant, why a bare `Pod` and not a `Deployment`, why `tokenFile`
rather than a literal token).

**No seccomp profile, no DaemonSet, no custom SCC.** An earlier version
of this backend gave `index-worker`'s network isolation to a seccomp
profile staged cluster-wide by a DaemonSet writing to a `hostPath`
volume — built and tested against a real OpenShift cluster, and found to
need a dedicated ServiceAccount granted `hostmount-anyuid` plus a second
custom SCC on the front door's own ServiceAccount, just to get one file
onto every node. `index-worker` now runs in its own Pod with a
`NetworkPolicy` denying all its egress instead — a plain namespaced API
object with no SCC dimension at all. See the ADR's 2026-09-21 addendum
for the full story if you're wondering why this looks different from an
older deployment.

**Verifying a real deployment**:
[`deploy/hub/test-kubernetes-pod-lifecycle.sh`](../deploy/hub/test-kubernetes-pod-lifecycle.sh)
is the Kubernetes analogue of `test-pod-lifecycle.sh` — not wired into
CI (no Kubernetes cluster available there), run by hand against a real
cluster, e.g. `oc exec` into the running front-door Deployment once RBAC
is applied and the shared storage PVC already exists.

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
no browser redirect handled by the CLI. The device generates (or
reuses) an Ed25519 signing identity — OS keystore first, falling back
to a `0600` file at `.wkp/hub-signing-identity` — and submits a CSR
built from it; the hub's own CA signs and returns a certificate. On
approval, `wkp hub register` writes three files: the certificate
(`.wkp/hub-device-cert.pem`), the hub's CA root
(`.wkp/hub-ca-cert.pem`), and a PKCS#8 PEM copy of the same signing
key (`.wkp/hub-device-key.pem`, `0600`) — that last one is what
`git`/`curl`'s OpenSSL-backed TLS stack needs, since it can't load the
OpenSSH-format signing identity directly. (This is a separate identity
from `.wkp/device-identity`, the age *encryption* key multi-machine
sync uses — unrelated key, unrelated purpose.)

Registration only writes those files; it does not add a git remote or
configure git's TLS options. Do both before `wkp sync`/`wkpd` will
work against this hub:

```bash
git remote add origin https://hub.example.com:8443/acme.git
git config --local http.sslCert .wkp/hub-device-cert.pem
git config --local http.sslKey .wkp/hub-device-key.pem
git config --local http.sslCAInfo .wkp/hub-ca-cert.pem
```

From here, `wkp sync`/`wkpd` against this hub work exactly like
[multi-machine sync](multi-machine-sync.md) does against any other git
remote — the hub's `origin` is just another git server that happens to
also do mTLS and revocation.

## Revoke a device

```bash
wkp-hub device revoke-id <device-id>
```

Effective on that device's *next* connection attempt — the mTLS
`ClientCertVerifier` checks revocation state on every TLS handshake.
For an incident where "next connection" isn't fast enough (a leaked
credential), `wkp-hub reset-all-connections --by <label>` force-closes
already-open connections too, not just future ones.

**That command doesn't cover a suspected CA compromise.** It only
closes active connections — the hub keeps trusting its existing CA
afterward, so any certificate that CA already signed (and that hasn't
been individually revoked) can simply reconnect. Recovering from a
compromised CA means rotating the hub's own CA (replacing the files
under `WKP_HUB_CA_DIR`) and re-enrolling every device against the new
one — there's no single command for this today; treat it as a manual
incident-response procedure, not a routine one.

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
