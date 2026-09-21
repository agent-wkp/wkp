#!/bin/bash
# Kubernetes analogue of test-pod-lifecycle.sh, for `KubernetesOrchestrator`
# (issue #141, ADR-0012's 2026-09-20 addendum): starts a real tenant Pod,
# proves it's reachable over the Service DNS name `http.rs`'s
# `proxy_to_tenant_pod` already builds, stops it, then proves the reaper
# stops an idle one on its own -- the same three properties
# test-pod-lifecycle.sh proves for the podman backend.
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
# describes (get/list/watch/create/delete on pods/services/pvcs, scoped
# to one namespace), and `deploy/hub/k8s/seccomp-daemonset.yaml` already
# applied cluster-wide. Simplest concrete way to run it: `oc exec` into
# the real front-door Deployment once it's live, with
# `WKP_HUB_POD_ORCHESTRATOR=kubernetes` already set in its env.
set -euo pipefail

WKP_HUB_BIN="${WKP_HUB_BIN:-wkp-hub}"
IMAGE="${WKP_HUB_TENANT_IMAGE:?WKP_HUB_TENANT_IMAGE must be set (the image tenant Pods run)}"
NAMESPACE="${WKP_HUB_K8S_NAMESPACE:?WKP_HUB_K8S_NAMESPACE must be set}"
TENANT="k8s-lifecycle-test-$$"
: "${DATABASE_URL:?DATABASE_URL must be set}"

export WKP_HUB_POD_ORCHESTRATOR=kubernetes
export WKP_HUB_TENANT_IMAGE="$IMAGE"

WORKDIR="$(mktemp -d)"
cleanup() {
    "$WKP_HUB_BIN" stop-pod "$TENANT" >/dev/null 2>&1 || true
    kubectl delete pvc "repo-${TENANT}" -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1 || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

log() { printf '==> %s\n' "$*"; }

log "creating tenant '$TENANT'"
"$WKP_HUB_BIN" migrate >/dev/null
"$WKP_HUB_BIN" tenant create "$TENANT"

log "starting the tenant's pod"
"$WKP_HUB_BIN" start-pod "$TENANT"

log "waiting for the pod to be Ready"
kubectl wait --for=condition=Ready "pod/wkp-tenant-${TENANT}" -n "$NAMESPACE" --timeout=60s

log "pushing a shared item over the Service DNS name (not just kubectl exec into the pod)"
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
# `KubernetesOrchestrator::start_pod` creates), exactly the way the real
# front door's `proxy_to_tenant_pod` does -- not `kubectl exec ... git`
# inside the pod itself, which would only prove intra-pod reachability.
git -C "$CLIENT_DIR" push --quiet "http://${TENANT}:8080/${TENANT}.git" main || {
    echo "FAIL: push to http://${TENANT}:8080/ over the Service DNS name failed" >&2
    exit 1
}
log "PASS: pod reachable and serving git over its Service DNS name"

log "stopping the pod"
"$WKP_HUB_BIN" stop-pod "$TENANT"
if kubectl get "pod/wkp-tenant-${TENANT}" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: pod still exists after stop-pod" >&2
    exit 1
fi
if ! kubectl get "pvc/repo-${TENANT}" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: PVC was deleted along with the pod -- stop_pod must never touch tenant data" >&2
    exit 1
fi
log "PASS: pod removed, PVC (the tenant's actual data) untouched"

log "testing the reaper: start again, backdate activity, sweep"
"$WKP_HUB_BIN" start-pod "$TENANT"
kubectl wait --for=condition=Ready "pod/wkp-tenant-${TENANT}" -n "$NAMESPACE" --timeout=60s
psql "$DATABASE_URL" -c \
    "UPDATE tenants SET last_active_at = now() - interval '1 hour' WHERE slug = '${TENANT}'" \
    >/dev/null
"$WKP_HUB_BIN" reap-idle-pods --idle-minutes 30
if kubectl get "pod/wkp-tenant-${TENANT}" -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "FAIL: reaper did not stop an idle pod" >&2
    exit 1
fi
log "PASS: reaper stopped the idle pod"

log "ALL CHECKS PASSED"
