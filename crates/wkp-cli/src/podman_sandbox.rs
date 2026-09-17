//! `wkp --sandbox podman <subcommand> [args...]` (ADR-0015, issue #225):
//! re-execs the requested subcommand inside a container instead of
//! running it natively. This is the macOS filesystem-isolation
//! alternative ADR-0015 decided on in place of a native Seatbelt
//! mechanism -- opt-in, not a default on any platform.
//!
//! This is deliberately *not* the self-invocation bug class CLAUDE.md
//! documents a real incident for (a fork loop from a process re-exec'ing
//! itself with ambiguous path resolution): the container's own
//! `ENTRYPOINT` (`deploy/Containerfile.sandbox`) invokes the compiled
//! `wkp` binary directly with the given subcommand, and [`run`] never
//! passes `--sandbox` back in. There is no argument sequence that leads
//! back into a second `--sandbox` re-exec -- no recursion is reachable
//! by construction, not just unlikely in practice.

use std::process::{Command, Stdio};

/// Overrides the image `run` pulls, instead of the version-matched
/// `ghcr.io/<owner>/wkp:v<version>-sandbox` tag [`image_reference`]
/// would otherwise resolve. Exists so this can be exercised end-to-end
/// (CI, or a developer's own machine) against a locally built image,
/// without requiring the exact version currently being compiled to
/// already exist as a real, published release.
const IMAGE_OVERRIDE_ENV: &str = "WKP_SANDBOX_IMAGE";

/// The image `--sandbox podman` runs by default, tagged to match *this*
/// binary's own version -- so a sandboxed run is provably the same
/// code, not "whatever `:latest` happens to be today." Derived from
/// `CARGO_PKG_REPOSITORY` (set from the workspace `Cargo.toml`) rather
/// than hardcoded, so a fork publishing under a different owner gets
/// its own image name for free.
///
/// A distinct `-sandbox`-suffixed tag, not M6-7 (issue #176)'s existing
/// `ghcr.io/<owner>/wkp:v<version>` image: that one is built `FROM
/// scratch` (`deploy/Containerfile`) to prove the CLI binary's own
/// static-link property, so it has no `git` binary inside it at all --
/// found by hand while building this feature (every `wkp` subcommand
/// that touches a real store shells out to `git`, so the scratch image
/// fails immediately with "git not found on PATH" for anything beyond
/// `--version`). `deploy/Containerfile.sandbox` builds this tag instead:
/// a small Fedora-minimal base (this project's own container
/// preference) with `git` installed alongside the release binary.
fn image_reference(override_image: Option<&str>) -> Result<String, String> {
    if let Some(image) = override_image {
        return Ok(image.to_string());
    }
    let repo = env!("CARGO_PKG_REPOSITORY");
    let owner = repo
        .rsplit('/')
        .nth(1)
        .ok_or_else(|| format!("wkp: cannot derive an image owner from repository URL '{repo}'"))?;
    Ok(format!(
        "ghcr.io/{owner}/wkp:v{}-sandbox",
        env!("CARGO_PKG_VERSION")
    ))
}

/// `true` if `podman --version` cannot be run at all -- not installed,
/// or not on `PATH`. Checked up front so the failure a user sees is
/// "podman not found," not a confusing raw spawn error from the real
/// `podman run` invocation below.
fn podman_missing() -> bool {
    Command::new("podman")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
}

/// Re-execs `subcommand_args` (e.g. `["index"]`, `["search", "topic"]`)
/// inside a container via `podman run`, bind-mounting the current
/// directory at the same path so the sandboxed process resolves the
/// store the same way the native one would. Returns the child's own
/// exit code; stdin/stdout/stderr are inherited so interactive and
/// piped use (`wkp remember <<EOF ... EOF`, `wkp search ... | ...`)
/// behave the same as running natively.
pub fn run(subcommand_args: &[String]) -> Result<i32, String> {
    if podman_missing() {
        return Err(
            "podman not found on PATH -- --sandbox podman needs a working podman install \
             (on macOS, a running `podman machine`)"
                .to_string(),
        );
    }

    let cwd = std::env::current_dir().map_err(|e| format!("cannot read cwd: {e}"))?;
    let override_image = std::env::var(IMAGE_OVERRIDE_ENV).ok();
    let image = image_reference(override_image.as_deref())?;
    let mount = format!("{}:{}:Z", cwd.display(), cwd.display());

    let status = Command::new("podman")
        .arg("run")
        .arg("--rm")
        .arg("-v")
        .arg(&mount)
        .arg("-w")
        .arg(&cwd)
        .arg(&image)
        .args(subcommand_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run podman: {e}"))?;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_reference_uses_override_when_set() {
        let image =
            image_reference(Some("localhost/wkp-test:dev")).expect("override should resolve");
        assert_eq!(image, "localhost/wkp-test:dev");
    }

    #[test]
    fn image_reference_derives_ghcr_tag_without_override() {
        let image = image_reference(None).expect("should derive a default image reference");
        assert!(
            image.starts_with("ghcr.io/"),
            "expected a ghcr.io image, got {image}"
        );
        assert!(
            image.ends_with(concat!(":v", env!("CARGO_PKG_VERSION"), "-sandbox")),
            "expected the image tag to match this binary's own version, got {image}"
        );
    }
}
