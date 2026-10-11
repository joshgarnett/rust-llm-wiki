# Install and verify lwiki

The v0.3.0 download, installer and installed-product qualification are pending in
this checkout. There is no ready public v0.3.0 download command here. Contributors
can build a native candidate using [the developer build guide](../developer/builds.md).
An older release may expose a different command set; inspect its own capabilities
and use its version's documentation.

## Check a supplied binary

Use a binary for your exact OS and architecture. Verify its archive checksum and
source identity against the accompanying release manifest before putting it on
PATH. Check these commands from an unrelated working directory:

```sh
lwiki --version
lwiki --help
lwiki --json capabilities
```

Local operation requires no Python, Node, model runtime or database server. The
supported distribution goal is native macOS and GNU/Linux. Windows compilation
or capability output does not qualify vault writes; those remain unsupported.

## macOS and Linux trust

The owner has chosen unsigned macOS distribution without notarization. A checksum
checks integrity against its supplied value; it does not authenticate the publisher.
A downloaded macOS executable can require the normal OS confirmation for an
unidentified developer. Verify its origin and digest before considering that
confirmation. Follow your OS's ordinary security controls; this guide does not
require disabling Gatekeeper or removing quarantine globally. Actual downloaded
artifact checks on the supported macOS architectures remain a qualification gate.

Linux archives also require exact architecture, digest and extracted executable
checks. Test with an ordinary user account and the normal system PATH. Native
GNU/Linux recovery and final downloaded bytes need their own qualification.

## Upgrade or uninstall

Before an upgrade, close editors/writers and make a complete wiki backup including
hidden files, immutable snapshots and retained history. Keep the old executable
and its version information with that backup. Test the new executable against a
copy first; a schema-changing storage operation may prevent an old reader from
opening the changed copy. Restore the complete pre-upgrade copy to roll back.

Removing an executable does not remove your wiki. Remove only the specific binary
or installation directory you selected. To stop persistent fallback selection,
run `lwiki wiki default clear` before uninstalling if desired; this preserves
other preferences. See [backup and restore](recovery.md#backup-and-restore).
