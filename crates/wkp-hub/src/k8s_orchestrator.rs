//! `KubernetesOrchestrator` (issue #141, ADR-0012 addendum): the
//! `PodOrchestrator` backend for a `wkp-hub` front door running as a
//! Kubernetes/OpenShift workload, instead of on a single podman host.
//! Built the same way [`crate::tenant_pod::PodmanOrchestrator`] is --
//! shelling out to a CLI (`kubectl` here, `podman` there) and generating
//! plain manifest text, not a Kubernetes client library. See
//! `docs/adr/0012-pod-orchestrator-abstraction.md`'s 2026-09-21 addendum
//! for the fuller reasoning behind every choice below, and
//! `docs/hub-mode.md`'s Kubernetes section for the operator-facing
//! deployment story (RBAC, the shared storage PVC).
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
//! ## Two Pods per tenant, not one Pod with two containers (2026-09-21
//! correction -- the first version of this file got this wrong)
//!
//! The podman backend's `index-worker` gets its "no network" property
//! two independent ways: `--network none` *and* a seccomp profile
//! (`tenant_pod.rs`'s own module doc comment). This file's first version
//! assumed the seccomp half alone was sufficient on Kubernetes, staged
//! cluster-wide by a DaemonSet writing the profile to every node's
//! kubelet seccomp root via a `hostPath` volume. Built and tested
//! against a real OpenShift cluster (`wbos.podzone.org`,
//! `paperless-ink/infra`), that turned out to need far more privilege
//! than it looked like on paper: every default/restricted SCC OpenShift
//! ships rejects `hostPath` outright ("not allowed to be used"), forcing
//! a dedicated ServiceAccount granted `hostmount-anyuid` just to stage
//! the profile file, plus a *second* custom SCC on the front door's own
//! ServiceAccount (SCC admission checks the *caller* creating a Pod, not
//! just the Pod's own spec) to let it reference that profile at all. All
//! of that privilege existed to install one file onto every node -- a
//! real, structural requirement of `securityContext.seccompProfile.
//! localhostProfile` (the kubelet refuses to admit a Pod naming a
//! profile that isn't already on-node; there is no way to supply one
//! inline the way podman's `--security-opt seccomp=<path>` does), not a
//! configuration mistake.
//!
//! **Corrected design: two separate Pods per tenant** (`wkp-tenant-
//! <slug>-serve`, `wkp-tenant-<slug>-index`), with `index-worker`'s "no
//! network" property enforced by a per-tenant `NetworkPolicy` denying
//! all egress from the index Pod, matched by label. OVN-Kubernetes (this
//! cluster's CNI, and the default for OpenShift generally) fully
//! enforces standard `NetworkPolicy` -- confirmed against the real
//! cluster before committing to this design, the same way the seccomp
//! approach's actual privilege cost was confirmed by building it, not
//! assumed. This needs zero cluster-wide staging (no DaemonSet, no
//! `hostPath`, no dedicated installer ServiceAccount) and zero extra SCC
//! grants beyond the RBAC this orchestrator already needed --
//! `NetworkPolicy` is a plain namespaced API object any ServiceAccount
//! with `create`/`delete` on it can manage, with no SCC dimension at
//! all. It trades one thing for another: two Pods to schedule instead of
//! one, and no single "the tenant's Pod" object -- every call site in
//! this file that used to name one Pod now names two.
//!
//! ## Storage: one shared `PersistentVolumeClaim`, `subPath`-isolated per
//! tenant -- not one PVC per tenant (2026-09-21 correction, same
//! investigation as above)
//!
//! The first version of this file created a fresh per-tenant PVC on
//! every `start_pod`, reasoning that a single shared PVC would "lose the
//! per-tenant filesystem isolation ADR-0014 already established." That
//! reasoning held for a *single-Pod* tenant (podman's model, and this
//! file's original one): one PVC, one Pod, trivial isolation by
//! construction. It stops holding once `index-worker` is a *second*,
//! separate Pod (previous section) needing concurrent access to the same
//! tenant data as `serve` -- this cluster's only StorageClass
//! (`lvms-vg1`, topolvm) is `ReadWriteOnce` only, so two Pods sharing one
//! tenant's data need `ReadWriteMany`, and creating a fresh RWX
//! PersistentVolume per tenant means running a real NFS (or similar)
//! server *per tenant*, which is far more infrastructure than one shared
//! server multiplexing every tenant behind `subPath`.
//!
//! **Corrected design**: one namespace-wide RWX PVC
//! (`paperless-ink/infra`'s `manifests/70-shared-tenant-storage.yaml`,
//! backed by `containers/nfs-server/` -- a pure-Rust NFS server chosen
//! there specifically because it needs no elevated privilege either,
//! for reasons parallel to this file's own seccomp-vs-NetworkPolicy
//! story; see that repo's `containers/nfs-server/README.md`), with each
//! tenant isolated by `subPath: <tenant_slug>` in both Pods' volume
//! mounts rather than by owning a whole distinct PV. Per-tenant
//! isolation is preserved (`subPath` confines each tenant to its own
//! subtree; the shared server itself enforces no per-client identity, so
//! network-level isolation -- `paperless-ink/infra`'s own
//! `NetworkPolicy` on the storage server -- is what keeps arbitrary
//! cluster workloads from reaching it at all, a concern that predates
//! and is independent of this file). An init container in each Pod
//! `mkdir -p`s the tenant's subdirectory against the *unscoped* mount
//! (no `subPath`) before the main container mounts the same PVC *with*
//! `subPath` -- `subPath` targets are not guaranteed to be created for
//! every volume plugin/Kubernetes version, so this doesn't rely on that.
//!
//! `stop_pod` still never touches this PVC (module-wide invariant,
//! unchanged from the first version): it's namespace-wide, shared by
//! every tenant, definitionally never a single tenant's `stop_pod` to
//! delete.
//!
//! ## Bare `Pod`s, not `Deployment`s -- deliberate, not an oversight
//!
//! A `Deployment`'s controller actively reconciles its Pod back into
//! existence if deleted. [`stop_pod`](KubernetesOrchestrator::stop_pod)
//! deleting a tenant's Pods is how idle-reap actually takes effect here
//! -- wrapping them in `Deployment`s would have the controller
//! immediately recreate what the reaper just tore down, silently
//! defeating M5-7's whole idle-cost-savings design. A bare `Pod` with
//! the default `restartPolicy: Always` still gets kubelet's own
//! container-level restart-on-crash for free (unlike podman's own
//! `pod_is_running`/`pod rm -f`-then-recreate dance, which exists
//! specifically because podman *doesn't* offer that) -- so this loses
//! nothing a `Deployment` would have given for the crash-recovery case,
//! while keeping this orchestrator's own start/stop calls the sole
//! authority over whether either Pod exists at all.
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

/// Overrides the name of the namespace-wide shared RWX PVC every
/// tenant's two Pods mount (via `subPath`) instead of owning one each --
/// see the module doc comment's storage section. Defaults to the name
/// `paperless-ink/infra`'s `manifests/70-shared-tenant-storage.yaml`
/// actually creates, so a real deployment needs no configuration; the
/// override exists for the same reason `NAMESPACE_ENV` does (a manual
/// test against a scratch namespace/PVC name).
const SHARED_PVC_ENV: &str = "WKP_HUB_K8S_SHARED_PVC";
const DEFAULT_SHARED_PVC: &str = "tenant-repos-shared";

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
    shared_pvc: String,
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
        let shared_pvc = match std::env::var(SHARED_PVC_ENV) {
            Ok(name) => name,
            Err(std::env::VarError::NotPresent) => DEFAULT_SHARED_PVC.to_string(),
            Err(e) => return Err(format!("{SHARED_PVC_ENV}: {e}")),
        };
        write_kubeconfig(&namespace)?;
        Ok(Self {
            namespace,
            shared_pvc,
        })
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
    /// meaning here -- this backend's storage is the shared RWX PVC
    /// named by [`SHARED_PVC_ENV`] instead (module doc comment).
    fn start_pod(&self, image: &str, _repos_root: &Path, tenant_slug: &str) -> Result<(), String> {
        self.apply(&tenant_manifest(
            &self.namespace,
            &self.shared_pvc,
            image,
            tenant_slug,
        ))
    }

    fn stop_pod(&self, tenant_slug: &str) -> Result<(), String> {
        let output = self.kubectl(&[
            "delete",
            &format!("pod/{}", serve_pod_name(tenant_slug)),
            &format!("pod/{}", index_pod_name(tenant_slug)),
            &format!("service/{tenant_slug}"),
            &format!("networkpolicy/{}", index_deny_egress_name(tenant_slug)),
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

fn serve_pod_name(tenant_slug: &str) -> String {
    format!("wkp-tenant-{tenant_slug}-serve")
}

fn index_pod_name(tenant_slug: &str) -> String {
    format!("wkp-tenant-{tenant_slug}-index")
}

fn index_deny_egress_name(tenant_slug: &str) -> String {
    format!("wkp-tenant-{tenant_slug}-index-deny-egress")
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

/// The exact manifest [`KubernetesOrchestrator::start_pod`] applies: two
/// Pods (`serve`, `index` -- module doc comment on why not one Pod with
/// two containers, and not a `Deployment`), each mounting the shared RWX
/// PVC at its own `subPath`, a `NetworkPolicy` denying all egress from
/// the index Pod, and a `Service` so the front door can reach the serve
/// Pod by the plain `tenant_slug` hostname `http.rs`'s
/// `proxy_to_tenant_pod` already builds -- Kubernetes' own
/// intra-namespace DNS resolves a bare Service name with no code change
/// needed there (ADR-0012's own note that the traffic-proxying half was
/// already runtime-agnostic).
///
/// Extracted as its own pure function, mirroring
/// [`crate::tenant_pod::repo_mount_arg`], so a test can assert on the
/// generated YAML without a real cluster to apply it against.
fn tenant_manifest(namespace: &str, shared_pvc: &str, image: &str, tenant_slug: &str) -> String {
    let mount_path = format!("/srv/wkp-hub/repos/{tenant_slug}");
    format!(
        r#"apiVersion: v1
kind: Pod
metadata:
  name: {serve_pod}
  namespace: {namespace}
  labels:
    app: {serve_pod}
    tenant: {tenant_slug}
spec:
  initContainers:
    - name: init-tenant-dir
      image: {image}
      command: ["mkdir", "-p", "/mnt/shared-repos/{tenant_slug}"]
      volumeMounts:
        - name: shared-repos
          mountPath: /mnt/shared-repos
  containers:
    - name: serve
      image: {image}
      command: ["/usr/local/bin/wkp-hub"]
      args: ["serve-tenant", "--tenant", "{tenant_slug}", "--port", "{SERVE_PORT}"]
      ports:
        - containerPort: {SERVE_PORT}
      volumeMounts:
        - name: shared-repos
          mountPath: {mount_path}
          subPath: {tenant_slug}
  volumes:
    - name: shared-repos
      persistentVolumeClaim:
        claimName: {shared_pvc}
---
apiVersion: v1
kind: Pod
metadata:
  name: {index_pod}
  namespace: {namespace}
  labels:
    app: {index_pod}
    tenant: {tenant_slug}
spec:
  initContainers:
    - name: init-tenant-dir
      image: {image}
      command: ["mkdir", "-p", "/mnt/shared-repos/{tenant_slug}"]
      volumeMounts:
        - name: shared-repos
          mountPath: /mnt/shared-repos
  containers:
    - name: index-worker
      image: {image}
      command: ["/usr/local/bin/wkp-hub"]
      args: ["index-worker", "--tenant", "{tenant_slug}"]
      volumeMounts:
        - name: shared-repos
          mountPath: {mount_path}
          subPath: {tenant_slug}
  volumes:
    - name: shared-repos
      persistentVolumeClaim:
        claimName: {shared_pvc}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {deny_egress}
  namespace: {namespace}
spec:
  podSelector:
    matchLabels: {{ app: {index_pod} }}
  policyTypes: ["Egress"]
  egress: []
---
apiVersion: v1
kind: Service
metadata:
  name: {tenant_slug}
  namespace: {namespace}
spec:
  selector:
    app: {serve_pod}
  ports:
    - port: {SERVE_PORT}
      targetPort: {SERVE_PORT}
"#,
        serve_pod = serve_pod_name(tenant_slug),
        index_pod = index_pod_name(tenant_slug),
        deny_egress = index_deny_egress_name(tenant_slug),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_manifest_names_every_object_from_the_slug_consistently() {
        let manifest = tenant_manifest(
            "paperless-ink",
            "tenant-repos-shared",
            "localhost/wkp-hub:test",
            "acme",
        );
        assert!(manifest.contains("name: wkp-tenant-acme-serve"));
        assert!(manifest.contains("name: wkp-tenant-acme-index"));
        assert!(manifest.contains("name: wkp-tenant-acme-index-deny-egress"));
        assert!(manifest.contains("name: acme\n")); // the Service itself
        assert!(manifest.contains("namespace: paperless-ink"));
        assert!(manifest.contains("image: localhost/wkp-hub:test"));
    }

    #[test]
    fn tenant_manifest_is_two_bare_pods_not_deployments() {
        // See the module doc comment: a Deployment's controller would
        // fight `stop_pod`'s deletion, silently defeating idle-reap.
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        assert_eq!(
            manifest.matches("kind: Pod").count(),
            2,
            "expected exactly two Pod objects"
        );
        assert!(!manifest.contains("kind: Deployment"));
    }

    #[test]
    fn tenant_manifest_puts_serve_and_index_in_separate_pods() {
        // 2026-09-21 correction: Kubernetes has no per-container
        // network-namespace opt-out, so index-worker's "no network"
        // property can only come from a NetworkPolicy scoped to its own
        // Pod -- which requires it to actually be its own Pod, not a
        // second container sharing the serve Pod's network namespace.
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        let serve_pod = manifest
            .split("name: wkp-tenant-acme-serve\n")
            .nth(1)
            .expect("serve Pod must be present")
            .split("---")
            .next()
            .unwrap();
        assert!(serve_pod.contains("name: serve"));
        assert!(!serve_pod.contains("name: index-worker"));

        let index_pod = manifest
            .split("name: wkp-tenant-acme-index\n")
            .nth(1)
            .expect("index Pod must be present")
            .split("---")
            .next()
            .unwrap();
        assert!(index_pod.contains("name: index-worker"));
        assert!(!index_pod.contains("name: serve\n"));
    }

    #[test]
    fn tenant_manifest_denies_all_egress_from_the_index_pod_only() {
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        assert!(manifest.contains("kind: NetworkPolicy"));
        let policy = manifest
            .split("kind: NetworkPolicy")
            .nth(1)
            .expect("NetworkPolicy must be present")
            .split("---")
            .next()
            .unwrap();
        assert!(
            policy.contains("app: wkp-tenant-acme-index"),
            "must select the index Pod"
        );
        assert!(
            !policy.contains("app: wkp-tenant-acme-serve"),
            "must not select the serve Pod"
        );
        assert!(policy.contains(r#"policyTypes: ["Egress"]"#));
        assert!(
            policy.contains("egress: []"),
            "must deny all egress, not just some rules"
        );
    }

    #[test]
    fn tenant_manifest_uses_the_shared_pvc_with_a_per_tenant_subpath_not_a_fresh_pvc() {
        // 2026-09-21 correction: no `kind: PersistentVolumeClaim` at all
        // any more -- both Pods reference the one namespace-wide shared
        // PVC by name, isolated from every other tenant only by subPath.
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        assert!(!manifest.contains("kind: PersistentVolumeClaim"));
        assert_eq!(
            manifest.matches("claimName: tenant-repos-shared").count(),
            2,
            "both Pods must reference the shared PVC by name"
        );
        assert_eq!(
            manifest.matches("subPath: acme").count(),
            2,
            "both Pods' main containers must scope their mount to this tenant's own subPath"
        );
    }

    #[test]
    fn tenant_manifest_creates_the_subpath_directory_before_mounting_it() {
        // subPath targets aren't guaranteed to be auto-created for every
        // volume plugin/Kubernetes version -- each Pod's init container
        // must mkdir -p it first, against the *unscoped* mount.
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        assert_eq!(
            manifest
                .matches(r#"command: ["mkdir", "-p", "/mnt/shared-repos/acme"]"#)
                .count(),
            2,
            "both Pods need an init container creating the tenant subdirectory"
        );
    }

    #[test]
    fn tenant_manifest_valid_yaml_with_four_documents() {
        // Cheap structural check without a real cluster to apply
        // against -- four `---`-separated documents (serve Pod, index
        // Pod, NetworkPolicy, Service), each independently well-formed.
        let manifest = tenant_manifest("paperless-ink", "tenant-repos-shared", "img", "acme");
        let docs: Vec<&str> = manifest.split("---").collect();
        assert_eq!(
            docs.len(),
            4,
            "expected serve Pod, index Pod, NetworkPolicy, and Service documents"
        );
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
