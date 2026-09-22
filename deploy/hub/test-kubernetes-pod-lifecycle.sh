#!/bin/bash
# Kubernetes analogue of test-pod-lifecycle.sh, for `KubernetesOrchestrator`
# (issue #141, ADR-0012's 2026-09-21 addendum): starts a real tenant's two
# Pods (serve, index), proves the serve Pod is reachable over the Service
# DNS name `http.rs`'s `proxy_to_tenant_pod` already builds, proves the
# index Pod's NetworkPolicy actually denies it egress, stops both, then
# proves the reaper stops an idle tenant's Pods on its own -- the same
# properties test-pod-lifecycle.sh proves for the podman backend, plus the
# network-isolation check that backend gets from `--network none` instead.
#
# NOT wired into CI (CLAUDE.md: workflow edits and feature code don't
# land in the same PR) and NOT runnable from a plain dev shell outside a
# cluster -- `KubernetesOrchestrator` only supports in-cluster
# authentication (a Pod's own projected ServiceAccount token), by
# design (ADR-0012 addendum's own "Credentials" section), matching
# issue #141's real target ("Kubernetes support is required for
# production ... single-host podman is not the intended production
# deployment target"): wkp-hub is meant to run *as* a workload in the
# cluster it manages, not be pointed at a remote cluster from a laptop.
#
# Run this by copying it into (or `kubectl cp`-ing it to) a Pod that
# already has: `wkp-hub` on PATH, `kubectl`, `git`, `psql` (the
# `deploy/hub/Containerfile` image has all four), a ServiceAccount with
# the RBAC `docs/adr/0012-pod-orchestrator-abstraction.md`'s addendum
# describes (get/list/watch/create/delete on pods/services/pvcs/
# networkpolicies, scoped to one namespace), and the shared RWX storage
# PVC (`paperless-ink/infra`'s `manifests/70-shared-tenant-storage.yaml`)
# already applied. Simplest concrete way to run it: `oc exec` into the
# real front-door Deployment once it's live, with
# `WKP_HUB_POD_ORCHESTRATOR=kubernetes` already set in its env.
set -euo pipefail

WKP_HUB_BIN="${WKP_HUB_BIN:-wkp-hub}"
IMAGE="${WKP_HUB_TENANT_IMAGE:?WKP_HUB_TENANT_IMAGE must be set (the image tenant Pods run)}"
NAMESPACE="${WKP_HUB_K8S_NAMESPACE:?WKP_HUB_K8S_NAMESPACE must be set}"
TENANT="k8s-lifecycle-test-$$"
: "${DATABASE_URL:?DATABASE_URL must be set}"

export WKP_HUB_POD_ORCHESTRATOR=kubernetes
export WKP_HUB_TENANT_IMAGE="$IMAGE"

SERVE_POD="wkp-tenant-${TENANT}-serve"
INDEX_POD="wkp-tenant-${TENANT}-index"

WORKDIR="$(mktemp -d)"
cleanup() {
    "$WKP_HUB_BIN" stop-pod "$TENANT" >/dev/null 2>&1 || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

log() { printf '==> %s\n' "$*"; }

log "creating tenant '$TENANT'"
"$WKP_HUB_BIN" migrate >/dev/null
"$WKP_HUB_BIN" tenant create "$TENANT"

log "starting the tenant's two pods (serve, index)"
"$WKP_HUB_BIN" start-pod "$TENANT"

log "waiting for both pods to be Ready"
kubectl wait --for=condition=Ready "pod/${SERVE_POD}" -n "$NAMESPACE" --timeout=60s
kubectl wait --for=condition=Ready "pod/${INDEX_POD}" -n "$NAMESPACE" --timeout=60s

log "checking the index pod's NetworkPolicy actually denies it egress"
# Run the identical probe from the *serve* Pod first (no egress policy
# applies to it) and require it to succeed -- otherwise a probe failure
# for any other reason (no external route from this cluster, the shell
# lacking /dev/tcp, a transient network blip) would make the index pod's
# own failure look like policy enforcement when it isn't proof of
# anything. Only a passing baseline plus a failing index-pod probe
# actually isolates the NetworkPolicy's effect.
if ! kubectl exec "$SERVE_POD" -n "$NAMESPACE" -- timeout 5 sh -c \
    'echo | cat > /dev/tcp/1.1.1.1/443' 2>/dev/null; then
    echo "FAIL: baseline probe from the serve pod (no egress policy) failed -- can't tell whether a later index-pod failure means anything" >&2
    exit 1
fi
if kubectl exec "$INDEX_POD" -n "$NAMESPACE" -- timeout 5 sh -c \
    'echo | cat > /dev/tcp/1.1.1.1/443' 2>/dev/null; then
    echo "FAIL: index pod reached an external address -- NetworkPolicy is not enforcing" >&2
    exit 1
fi
log "PASS: index pod's egress is denied (serve pod's identical probe succeeded, ruling out a broken probe)"

log "pushing a shared item over the Service DNS name (not just kubectl exec into a pod)"
CLIENT_DIR="$WORKDIR/client"
git init --quiet -b main "$CLIENT_DIR"
cat > "$CLIENT_DIR/shared.md" <<'EOF'
---
visibility: shared
title: kubernetes pod lifecycle test item
---

pushed by deploy/hub/test-kubernetes-pod-lifecycle.sh
EOF
git -C "$CLIENT_DIR" add -A
git -C "$CLIENT_DIR" -c user.email=test@example.com -c user.name=test \
    commit -q -m "kubernetes pod lifecycle test"

# Resolves "$TENANT" via the cluster's own DNS (the Service
# `KubernetesOrchestrator::start_pod` creates, selecting the serve Pod
# only), exactly the way the real front door's `proxy_to_tenant_pod`
# does -- not `kubectl exec ... git` inside a pod itself, which would
# only prove intra-pod reachability.
git -C "$CLIENT_DIR" push --quiet "http://${TENANT}:8080/${TENANT}.git" main || {
    echo "FAIL: push to http://${TENANT}:8080/ over the Service DNS name failed" >&2
    exit 1
}
log "PASS: serve pod reachable and serving git over its Service DNS name"

log "stopping both pods"
"$WKP_HUB_BIN" stop-pod "$TENANT"
if kubectl get "pod/${SERVE_POD}" -n "$NAMESPACE" >/dev/null 2>&1 \
    || kubectl get "pod/${INDEX_POD}" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: at least one pod still exists after stop-pod" >&2
    exit 1
fi
if kubectl get "networkpolicy/wkp-tenant-${TENANT}-index-deny-egress" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: NetworkPolicy still exists after stop-pod" >&2
    exit 1
fi
log "PASS: both pods and the NetworkPolicy removed"

# No `kubectl get pvc/...` check here: the front door's own ServiceAccount
# deliberately has no RBAC on persistentvolumeclaims at all (confirmed by
# hand that creating a Pod referencing one by name needs none -- see
# 20-rbac.yaml in paperless-ink/infra), so this script, which runs as
# that same identity, can't read the PVC's status either. stop_pod's own
# implementation never names the shared PVC in its delete list -- that's
# the actual guarantee, verified by code review of start_pod/stop_pod
# rather than by a runtime check this identity isn't permitted to make.

log "testing the reaper: start again, backdate activity, sweep"
"$WKP_HUB_BIN" start-pod "$TENANT"
kubectl wait --for=condition=Ready "pod/${SERVE_POD}" -n "$NAMESPACE" --timeout=60s
kubectl wait --for=condition=Ready "pod/${INDEX_POD}" -n "$NAMESPACE" --timeout=60s
psql "$DATABASE_URL" -c \
    "UPDATE tenants SET last_active_at = now() - interval '1 hour' WHERE slug = '${TENANT}'" \
    >/dev/null
"$WKP_HUB_BIN" reap-idle-pods --idle-minutes 30
if kubectl get "pod/${SERVE_POD}" -n "$NAMESPACE" >/dev/null 2>&1 \
    || kubectl get "pod/${INDEX_POD}" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: reaper did not stop an idle tenant's pods" >&2
    exit 1
fi
log "PASS: reaper stopped both idle pods"

log "ALL CHECKS PASSED"
