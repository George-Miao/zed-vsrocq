# VsRocq for Zed

Use VsRocq in Zed with a local browser proof view. Zed uses VsRocq for diagnostics, completion, and hover.

## Setup

1. Install [Rocq and VsRocq](https://github.com/rocq-prover/vsrocq#installation).
2. Install this extension in Zed.
3. Make sure Zed can find `vsrocqtop` in its command path or opam environment.

The extension downloads the bridge from `George-Miao/zed-vsrocq` releases when no local bridge is available. You can select a local executable in [Config](#config).

## Usage

Open a `.v` file as Rocq. Open Zed's code-action menu and select a `VsRocq:` action:

- Step forward or backward.
- Run to cursor or run to end.
- Interrupt or reset.
- Open proof view.

An explicit proof command opens the browser by default.

### `.v` association

This extension associates `.v` files to Rocq. Verilog also uses that file extension. If both extensions are installed, select Rocq for proof files or set a project-level language association.

## Config

Put settings under `lsp.vsrocq.settings` in Zed:

```json
{
  "lsp": {
    "vsrocq": {
      "settings": {
        "proof": {
          "block": true,
          "autoOpen": true
        }
      }
    }
  }
}
```

The following keys are relative to `settings`:

| Key                         | Use                                                                                                                 |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `proof.block`               | Stop at the first error. Set to `false` to continue checking. Default: `true`.                                      |
| `proof.autoOpen`            | Open the browser for explicit proof commands. Set to `false` to open it only with the open action. Default: `true`. |
| `server.path`               | Select a Rocq language-server executable outside the detected toolchain.                                            |
| `server.args`               | Supply arguments for that executable.                                                                               |
| `bridge.path`               | Select a local bridge executable instead of downloading one.                                                        |
| `bridge.release_repository` | Change the download repository. Default: `George-Miao/zed-vsrocq`.                                                  |
| `bridge.release_tag`        | Select a specific release.                                                                                          |
| `bridge.download_url`       | Download a raw bridge executable from an explicit HTTPS URL.                                                        |

Zed's `lsp.vsrocq.binary.path` can also select a bridge executable. Use `vsrocq-bridge.exe` on Windows.

## Development

Use the Nix development shell on Linux or macOS:

```sh
nix develop
bun install --frozen-lockfile
build-bridge
build-extension
check-project
```

The flake uses `numtide/devshell` and supplies the Rust and Rocq toolchains and Bun.

To load the local extension, run `zed: extensions`, select `Install Dev Extension`, and select this directory. Start Zed from the development shell so that it can find the proof toolchain.
