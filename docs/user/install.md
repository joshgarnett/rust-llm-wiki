# Install lwiki

Install the command, put it on your PATH, then run [guided setup](getting-started.md)
to create your wiki and connect your agent. Releases support Apple silicon Macs
running macOS 15, 26 or 27, and x86-64 GNU/Linux with glibc 2.39 or newer.
The installer checks your system before installing. Windows wiki writes are unsupported.

## Download and install

Open the [GitHub Releases page](https://github.com/joshgarnett/rust-llm-wiki/releases)
and download `install.sh` from the release you want. The installer downloads the
matching archive, checks its recorded hashes and version, and installs it for
your user. Run it without `sudo`.

You need `curl` 8.4 or newer for downloads. On macOS, you also need Apple's
Command Line Tools. If they are missing, run `xcode-select --install` and finish
the installation before continuing. On Linux, install your distribution's
`binutils` package. These tools let the installer check that the download can run
on your system. It reports an incompatible system before changing the active
command.

In the folder where you saved `install.sh`, run:

```sh
sh install.sh --version 0.3.0
```

Use the version of the release you selected, without the leading `v`. The default
installation is under `$HOME/.local`, with the command at
`$HOME/.local/bin/lwiki`. You should see an **Installed** message with the version
and location.

Make the command available in this terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
lwiki --version
```

The reported version should match the one you installed. Add the same `export`
line to your shell's startup file so future terminals can find it. For zsh, use
`~/.zshrc`; for interactive bash, use `~/.bashrc`. Open a new terminal and check
`lwiki --version` again before continuing with [setup](getting-started.md).

To install somewhere else, pass an absolute path you own:

```sh
sh install.sh --version 0.3.0 --prefix "/absolute/path/to/lwiki-install"
```

Add that location's `bin` directory to PATH instead. The installer does not
change your shell files or overwrite an unrelated command at the destination.

macOS releases are unsigned and are not notarized. macOS may ask you to approve
an app from an unidentified developer. Check that the download came from the
project's release page before using your system's normal approval controls.

## Install downloaded assets without a network connection

Download `release-manifest.txt` and the archive for your system from the same
release. Put both in one folder, then tell the installer where to find them:

```sh
sh install.sh --version 0.3.0 --from-dir "/absolute/path/to/release-files"
```

The archive names end in `aarch64-apple-darwin.tar.gz` for Apple silicon macOS
and `x86_64-unknown-linux-gnu.tar.gz` for x86-64 GNU/Linux. `--dry-run` checks the
files and shows the proposed installation without activating it.

## Upgrade or go back to an earlier version

[Back up your whole wiki](recovery.md#backup-and-restore) before upgrading.
Install the new version with the same prefix. The installer keeps earlier
installed versions, so you can select one again:

```sh
sh install.sh --rollback EARLIER_VERSION
```

Replace `EARLIER_VERSION` with an installed version, such as `0.3.0`. This changes
the executable, not the wiki. If a new version changed the wiki's storage format,
restore your complete pre-upgrade backup before using the old executable.

## Remove lwiki

```sh
sh install.sh --uninstall
```

Use the same `--prefix` if you chose a custom location. Uninstall removes the
command's launch links and keeps the retained versions, your wiki, settings and
agent instructions. Removing lwiki never deletes your notes. You can remove
[installed agent instructions](integration.md#connect-your-agent)
separately if you no longer need them.

To build the command from source instead, see the [developer build guide](../developer/builds.md).
