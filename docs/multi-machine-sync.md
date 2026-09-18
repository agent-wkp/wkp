# Multiple machines, via a private git repo

The same store, cloned onto more than one machine, staying in sync
through a plain git remote you already trust — a private GitHub repo, a
bare repo on a NAS, or any git server. No `wkp-hub`, no extra service:
git's own fetch/push protocol is the sync transport (design 6.1).

## Set up the first machine

```bash
cd your-workspace
wkp init
wkp index
git remote add origin git@github.com:you/your-private-wkp-store.git
git push -u origin main
```

Nothing `wkp`-specific here — `git remote add` and `git push` are
ordinary git. `wkp init` already registered the merge driver
(`.gitattributes: *.md merge=wkp`) and SSH commit signing on this clone;
that's the only setup `wkp` itself needs before you can push.

## Add a second machine

```bash
git clone git@github.com:you/your-private-wkp-store.git your-workspace
cd your-workspace
wkp init
wkp index
```

`wkp init` is idempotent and safe to re-run even on a store that already
has real content — it fills in what's missing (this machine's own
device identity, its entry in the SSH `allowed_signers` file, its own
entry in the `recipients` file for encrypted content) without touching
anything that's already there. Each machine gets its **own** device
identity and its own `sync/<device-id>` branch — never share
`.wkp/device-id`/`.wkp/device-identity` between machines by copying
them; that defeats the per-device model (see
[`docs/migrating-from-python.md`](migrating-from-python.md) for why
those two files specifically are never safe to delete-and-regenerate
either).

## Everyday use

```bash
wkp sync                # fetch, merge, push -- default remote: origin
wkp sync status          # report unresolved conflicts, change nothing
```

`wkp sync` fetches every other device's `sync/<device-id>` branch,
merges each into this device's own working branch via the structural
merge driver, then pushes. Safe-mode semantics throughout (design 6.2):
nothing is ever silently discarded.

- **Frontmatter conflicts** merge by field rule — union of `tags`, max
  of `updated`, both `provenance` entries kept.
- **Add/add** on the same path keeps both bodies, with provenance
  markers.
- **Modify/delete** keeps the modification; the deletion is re-proposed
  as an `inbox/` item for a human to confirm (`wkp resolve-conflicts`
  handles this specific case explicitly).
- Anything left over after that is standard git conflict markers,
  surfaced by `wkp sync status` for a human or agent to resolve like
  any other task.

## Automating it: `wkpd`

Running `wkp sync` by hand works, but a watch-triggered daemon is
usually nicer for a machine you leave running:

```bash
wkp wkpd --principal device:laptop --signing-key-file ~/.ssh/id_ed25519
```

Debounced auto-commit of local changes, incremental re-index, and
opportunistic `wkp sync` — a `SessionStart` hook or a session start
never blocks on network availability either way.
[`deploy/systemd/wkpd.service`](../deploy/systemd/wkpd.service) is a
ready-to-edit `systemd --user` unit if you want it running persistently
rather than started by hand.

## No shared remote at all: air-gapped sync

```bash
wkp bundle export /path/to/transfer.bundle          # on machine A
# move transfer.bundle across by USB, SCP, email, whatever
wkp bundle import /path/to/transfer.bundle          # on machine B
```

`git bundle` under the hood — the same merge semantics as `wkp sync`,
just without a network round trip. `--since <ref>` on export limits it
to what changed since a point you've already synced, instead of
everything.

## Encrypted (`visibility: private`) content

A device only gets automatically added to the store's `recipients` file
(the list of public keys private content is encrypted to) at `wkp init`
time. That means a **new** device can decrypt anything written *after*
it joins, but not anything encrypted before — re-encrypting existing
private files to include a newly added device isn't automatic. See
design section 7.2 for the full encryption model.

## Full design reference

[`docs/design/wkp-hub-design-v0.1.md`](design/wkp-hub-design-v0.1.md)
section 6 (sync model) and section 6.4 (registration and tiering, for
how this compares to hub mode) is the authoritative source this doc
summarizes.
