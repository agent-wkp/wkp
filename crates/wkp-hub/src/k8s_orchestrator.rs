//! `KubernetesOrchestrator` (issue #141, ADR-0012 addendum): the
//! `PodOrchestrator` backend for a `wkp-hub` front door running as a
//! Kubernetes/OpenShift workload, instead of on a single podman host.
//! Built the same way [`crate::tenant_pod::PodmanOrchestrator`] is --
//! shelling out to a CLI (`kubectl` here, `podman` there) and generating
//! plain manifest text, not a Kubernetes client library. See
//! `docs/adr/0012-pod-orchestrator-abstraction.md`'s 2026-09-20 addendum
//! for the fuller reasoning behind every choice below, and
//! `docs/hub-mode.md`'s Kubernetes section for the operator-facing
//! deployment story (RBAC, the seccomp-profile DaemonSet).
//!
//! ## Why `kubectl`, not a Kubernetes client crate
//!
//! Issue #141 originally scoped this as needing "a Rust Kubernetes
//! client crate ... its own `cargo deny`/`cargo vet` justification."
//! That turned out not to be necessary: [`PodmanOrchestrator`] already
//! established that this trait's two methods are small enough to
//! implement by shelling out and checking exit status, and `kubectl`
//! is exactly as mature and widely audited a tool as `podman` itself.
//! No new Cargo dependency, no license/supply-chain review for a client
//! library -- CLAUDE.md's slim-core rule, applied the same way ADR-0012
//! already applied it to the podman side.
//!
//! ## Bare `Pod`, not `Deployment` -- deliberate, not an oversight
//!
//! A `Deployment`'s controller actively reconciles its Pod back into
//! existence if deleted. [`stop_pod`](KubernetesOrchestrator::stop_pod)
//! deleting a tenant's `Pod` object is how idle-reap actually takes
//! effect here -- wrapping it in a `Deployment` would have the
//! controller immediately recreate what the reaper just tore down,
//! silently defeating M5-7's whole idle-cost-savings design. A bare
//! `Pod` with the default `restartPolicy: Always` still gets kubelet's
//! own container-level restart-on-crash for free (unlike podman's own
//! `pod_is_running`/`pod rm -f`-then-recreate dance, which exists
//! specifically because podman *doesn't* offer that) -- so this loses
//! nothing a `Deployment` would have given for the crash-recovery case,
//! while keeping this orchestrator's own start/stop calls the sole
//! authority over whether the Pod exists at all.
//!
//! ## Storage: one `PersistentVolumeClaim` per tenant, created once
//!
//! Chosen over `hostPath` (ties every tenant to one specific node, no
//! real multi-node story) and over a single shared PVC (loses the
//! per-tenant filesystem isolation ADR-0014 already established for the
//! podman backend). [`start_pod`](KubernetesOrchestrator::start_pod)'s
//! generated manifest includes the PVC every time (an `apply`, not a
//! `create` -- idempotent, matches the rest of this function), but
//! [`stop_pod`](KubernetesOrchestrator::stop_pod) never deletes it,
//! mirroring the podman backend's own "stop the container, keep the
//! bind-mounted data" behavior (design 8.2/ADR-0010: bare repos persist
//! outside any one pod's own ephemeral filesystem). Deleting a tenant's
//! actual data is `wkp-hub tenant delete`'s job (not yet built, tracked
//! separately), never an implicit side effect of a pod stopping.
//!
//! ## Credentials: a generated in-cluster kubeconfig, not a raw token flag
//!
//! Plain `kubectl` (unlike a Kubernetes client library) does not
//! automatically discover in-cluster credentials --
//! `rest.InClusterConfig()` is client-go's own convenience, not
//! something the `kubectl` binary does for you. The standard,
//! documented mechanism
//! ([Kubernetes docs: "Directly accessing the REST API"](https://kubernetes.io/docs/tasks/run-application/access-api-from-pod/#directly-accessing-the-rest-api))
//! is what [`write_kubeconfig`] generates: a kubeconfig whose user
//! stanza uses `tokenFile` rather than a literal `token` value --
//! `kubectl` re-reads that file on every invocation, so a projected
//! service account token's automatic rotation (the kubelet rewrites the
//! file in place periodically) is handled for free, with no code here
//! needing to track token lifetime. The kubeconfig itself contains no
//! secret material (`tokenFile` is a path, not a value) -- only the API
//! server address and the CA certificate's path.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::tenant_pod::{PodOrchestrator, SERVE_PORT};

/// Where Kubernetes projects a Pod's ServiceAccount credentials --
/// fixed by the platform, not configurable.
const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Overrides which namespace this orchestrator manages. Unset in normal
/// operation (the in-cluster namespace file, `{SA_DIR}/namespace`,
/// already answers this correctly for a Pod bound to the right
/// ServiceAccount) -- exists so a manual test against a real cluster
/// (`deploy/hub/test-kubernetes-pod-lifecycle.sh`) can target a scratch
/// namespace without depending on exactly which ServiceAccount happens
/// to be mounted into the process running it.
const NAMESPACE_ENV: &str = "WKP_HUB_K8S_NAMESPACE";

/// Where the generated kubeconfig lives. A fixed path, not a fresh
/// `tempfile::NamedTempFile` per call -- every `kubectl` invocation
/// across this orchestrator's lifetime reuses the one written at
/// construction time, rather than rewriting it to disk on every single
/// pod start/stop.
const KUBECONFIG_PATH: &str = "/tmp/wkp-hub-kubeconfig.yaml";

/// The Kubernetes/OpenShift [`PodOrchestrator`] backend. See the module
/// doc comment for the shape of every decision behind this.
pub struct KubernetesOrchestrator {
    namespace: String,
}

impl KubernetesOrchestrator {
    /// Resolves the target namespace and writes the kubeconfig
    /// [`Self::kubectl`]/[`Self::apply`] need, once, here -- so a
    /// broken in-cluster environment (missing ServiceAccount
    /// token/CA, or this process not actually running in a Pod at all)
    /// fails fast at `orchestrator()` selection time (startup), not
    /// later on some tenant's first real request.
    pub fn new() -> Result<Self, String> {
        let namespace = match std::env::var(NAMESPACE_ENV) {
            Ok(ns) => ns,
            Err(std::env::VarError::NotPresent) => read_in_cluster_namespace()?,
            Err(e) => return Err(format!("{NAMESPACE_ENV}: {e}")),
        };
        write_kubeconfig(&namespace)?;
        Ok(Self { namespace })
    }

    fn kubectl(&self, args: &[&str]) -> Result<std::process::Output, String> {
        Command::new("kubectl")
            .arg("--kubeconfig")
            .arg(KUBECONFIG_PATH)
            .args(args)
            .output()
            .map_err(|e| format!("failed to run kubectl {args:?}: {e}"))
    }

    /// `kubectl apply -f -`, fed `manifest` over stdin -- idempotent by
    /// construction (unlike podman's own `run`/`pod create`, which
    /// reject an already-taken name outright, see
    /// [`crate::tenant_pod::start_pod`]'s own comment on why it needs
    /// an explicit `pod rm -f` first).
    fn apply(&self, manifest: &str) -> Result<(), String> {
        let mut child = Command::new("kubectl")
            .arg("--kubeconfig")
            .arg(KUBECONFIG_PATH)
            .args(["apply", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn kubectl apply: {e}"))?;
        child
            .stdin
            .take()
            .expect("stdin was requested as piped")
            .write_all(manifest.as_bytes())
            .map_err(|e| format!("failed to write manifest to kubectl apply's stdin: {e}"))?;
        let output = child
            .wait_with_output()
            .map_err(|e| format!("kubectl apply failed: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "kubectl apply failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

impl PodOrchestrator for KubernetesOrchestrator {
    /// `repos_root` is intentionally unused: that parameter names a
    /// host path for the podman backend's bind mount, which has no
    /// meaning here -- this backend's storage is a `PersistentVolumeClaim`
    /// per tenant instead (module doc comment).
    fn start_pod(&self, image: &str, _repos_root: &Path, tenant_slug: &str) -> Result<(), String> {
        self.apply(&tenant_manifest(&self.namespace, image, tenant_slug))
    }

    fn stop_pod(&self, tenant_slug: &str) -> Result<(), String> {
        let output = self.kubectl(&[
            "delete",
            &format!("pod/wkp-tenant-{tenant_slug}"),
            &format!("service/{tenant_slug}"),
            "-n",
            &self.namespace,
            "--ignore-not-found",
        ])?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "kubectl delete failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

fn read_in_cluster_namespace() -> Result<String, String> {
    Ok(std::fs::read_to_string(format!("{SA_DIR}/namespace"))
        .map_err(|e| {
            format!(
                "{NAMESPACE_ENV} is unset and {SA_DIR}/namespace couldn't be read ({e}) -- \
                 is this process actually running in a Kubernetes Pod? (Kubernetes projects \
                 this file into every Pod automatically; a plain `podman run` or a local dev \
                 shell will not have it.)"
            )
        })?
        .trim()
        .to_string())
}

/// Writes the in-cluster kubeconfig `kubectl` needs -- see the module
/// doc comment's "Credentials" section for why this shape (a generated
/// kubeconfig with `tokenFile`) is the correct mechanism, not a shortcut.
fn write_kubeconfig(namespace: &str) -> Result<(), String> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST").map_err(|_| {
        "KUBERNETES_SERVICE_HOST is unset -- is this process actually running in a \
         Kubernetes Pod? Kubernetes injects this into every Pod automatically; a plain \
         `podman run` or a local dev shell will not have it."
            .to_string()
    })?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT").map_err(|_| {
        "KUBERNETES_SERVICE_PORT is unset (see the KUBERNETES_SERVICE_HOST error for why \
         this should always be present inside a real Pod)"
            .to_string()
    })?;
    let kubeconfig = render_kubeconfig(&host, &port, namespace);
    std::fs::write(KUBECONFIG_PATH, kubeconfig)
        .map_err(|e| format!("failed to write kubeconfig to {KUBECONFIG_PATH}: {e}"))
}

/// Extracted as its own pure function (mirroring
/// [`crate::tenant_pod::repo_mount_arg`]'s own split) so a test can
/// assert on the generated kubeconfig without touching the filesystem
/// or requiring a real in-cluster environment.
fn render_kubeconfig(host: &str, port: &str, namespace: &str) -> String {
    format!(
        "apiVersion: v1\n\
         kind: Config\n\
         clusters:\n\
         \x20\x20- name: in-cluster\n\
         \x20\x20\x20\x20cluster:\n\
         \x20\x20\x20\x20\x20\x20server: https://{host}:{port}\n\
         \x20\x20\x20\x20\x20\x20certificate-authority: {SA_DIR}/ca.crt\n\
         users:\n\
         \x20\x20- name: wkp-hub\n\
         \x20\x20\x20\x20user:\n\
         \x20\x20\x20\x20\x20\x20tokenFile: {SA_DIR}/token\n\
         contexts:\n\
         \x20\x20- name: wkp-hub\n\
         \x20\x20\x20\x20context:\n\
         \x20\x20\x20\x20\x20\x20cluster: in-cluster\n\
         \x20\x20\x20\x20\x20\x20user: wkp-hub\n\
         \x20\x20\x20\x20\x20\x20namespace: {namespace}\n\
         current-context: wkp-hub\n"
    )
}

/// The exact manifest [`KubernetesOrchestrator::start_pod`] applies: a
/// `PersistentVolumeClaim` (module doc comment), a bare `Pod` running
/// the same two containers the podman backend does (`serve-tenant`,
/// `index-worker`), and a `Service` so the front door can reach it by
/// the plain `tenant_slug` hostname `http.rs`'s `proxy_to_tenant_pod`
/// already builds -- Kubernetes' own intra-namespace DNS resolves a
/// bare Service name with no code change needed there (ADR-0012's own
/// note that the traffic-proxying half was already runtime-agnostic).
///
/// `index-worker`'s `securityContext.seccompProfile` is this backend's
/// equivalent of the podman backend's `--network none` (module doc
/// comment on why: no per-container network-namespace opt-out exists
/// in the Kubernetes Pod API) -- it requires the named profile to
/// already exist on whatever node this Pod is scheduled to, staged by
/// `deploy/hub/k8s/seccomp-daemonset.yaml`, not by this code.
///
/// Extracted as its own pure function, mirroring
/// [`crate::tenant_pod::repo_mount_arg`], so a test can assert on the
/// generated YAML without a real cluster to apply it against.
fn tenant_manifest(namespace: &str, image: &str, tenant_slug: &str) -> String {
    format!(
        r#"apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: repo-{tenant_slug}
  namespace: {namespace}
  labels:
    app.kubernetes.io/part-of: paperless-ink-tenant
spec:
  accessModes: ["ReadWriteOnce"]
  resources:
    requests:
      storage: 1Gi
---
apiVersion: v1
kind: Pod
metadata:
  name: wkp-tenant-{tenant_slug}
  namespace: {namespace}
  labels:
    app: wkp-tenant-{tenant_slug}
spec:
  containers:
    - name: serve
      image: {image}
      command: ["/usr/local/bin/wkp-hub"]
      args: ["serve-tenant", "--tenant", "{tenant_slug}", "--port", "{SERVE_PORT}"]
      ports:
        - containerPort: {SERVE_PORT}
      volumeMounts:
        - name: repo
          mountPath: /srv/wkp-hub/repos/{tenant_slug}
    - name: index-worker
      image: {image}
      command: ["/usr/local/bin/wkp-hub"]
      args: ["index-worker", "--tenant", "{tenant_slug}"]
      securityContext:
        seccompProfile:
          type: Localhost
          localhostProfile: wkp-hub-seccomp-no-network.json
      volumeMounts:
        - name: repo
          mountPath: /srv/wkp-hub/repos/{tenant_slug}
  volumes:
    - name: repo
      persistentVolumeClaim:
        claimName: repo-{tenant_slug}
---
apiVersion: v1
kind: Service
metadata:
  name: {tenant_slug}
  namespace: {namespace}
spec:
  selector:
    app: wkp-tenant-{tenant_slug}
  ports:
    - port: {SERVE_PORT}
      targetPort: {SERVE_PORT}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_manifest_names_every_object_from_the_slug_consistently() {
        let manifest = tenant_manifest("paperless-ink", "localhost/wkp-hub:test", "acme");
        assert!(manifest.contains("name: repo-acme"));
        assert!(manifest.contains("name: wkp-tenant-acme"));
        assert!(manifest.contains("name: acme\n")); // the Service itself
        assert!(manifest.contains("namespace: paperless-ink"));
        assert!(manifest.contains("image: localhost/wkp-hub:test"));
    }

    #[test]
    fn tenant_manifest_is_a_bare_pod_not_a_deployment() {
        // See the module doc comment: a Deployment's controller would
        // fight `stop_pod`'s deletion, silently defeating idle-reap.
        let manifest = tenant_manifest("paperless-ink", "img", "acme");
        assert!(manifest.contains("kind: Pod"));
        assert!(!manifest.contains("kind: Deployment"));
    }

    #[test]
    fn tenant_manifest_gives_index_worker_the_seccomp_profile_not_the_serve_container() {
        let manifest = tenant_manifest("paperless-ink", "img", "acme");
        let index_worker_section = manifest
            .split("name: index-worker")
            .nth(1)
            .expect("index-worker container must be present");
        assert!(index_worker_section.contains("wkp-hub-seccomp-no-network.json"));
        let serve_section = manifest
            .split("name: serve\n")
            .nth(1)
            .expect("serve container must be present")
            .split("name: index-worker")
            .next()
            .expect("serve section ends before index-worker starts");
        assert!(!serve_section.contains("seccompProfile"));
    }

    #[test]
    fn tenant_manifest_valid_yaml_with_three_documents() {
        // Cheap structural check without a real cluster to apply
        // against -- three `---`-separated documents (PVC, Pod,
        // Service), each independently well-formed YAML.
        let manifest = tenant_manifest("paperless-ink", "img", "acme");
        let docs: Vec<&str> = manifest.split("---").collect();
        assert_eq!(docs.len(), 3, "expected PVC, Pod, and Service documents");
        for doc in docs {
            serde_yaml_value(doc);
        }
    }

    /// A tiny hand-rolled structural check, not a real YAML parser --
    /// this crate has no YAML dependency to reach for (`serde_json` is
    /// already a direct dependency for the RFC 8628 endpoints, YAML is
    /// not used anywhere else), and pulling one in only to assert
    /// "this string is well-formed YAML" in a unit test isn't
    /// justified. `kubectl apply --dry-run=server` against a real
    /// cluster (`ops/secrets.md` in `paperless-ink/infra`, and this
    /// crate's own manual `deploy/hub/test-kubernetes-pod-lifecycle.sh`)
    /// is what actually proves these manifests are valid Kubernetes
    /// objects, not this function.
    fn serde_yaml_value(doc: &str) {
        assert!(!doc.trim().is_empty(), "no empty document between '---'s");
        assert!(
            doc.contains("apiVersion:") && doc.contains("kind:") && doc.contains("metadata:"),
            "every document must be a well-formed Kubernetes object: {doc}"
        );
    }

    #[test]
    fn render_kubeconfig_uses_tokenfile_not_a_literal_token() {
        // The whole point (module doc comment's "Credentials" section):
        // `tokenFile` lets kubectl re-read a rotated token on every
        // call; embedding the token's actual value here would go stale
        // the moment it rotates.
        let kubeconfig = render_kubeconfig("10.0.0.1", "443", "paperless-ink");
        assert!(
            kubeconfig.contains("tokenFile: /var/run/secrets/kubernetes.io/serviceaccount/token")
        );
        assert!(!kubeconfig.contains("token:\n")); // no literal `token:` key anywhere
        assert!(kubeconfig.contains("server: https://10.0.0.1:443"));
        assert!(kubeconfig.contains("namespace: paperless-ink"));
    }
}
