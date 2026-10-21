# Desktop releases

The release workflow covers Windows, macOS and Linux on x64 and ARM64. Each target
uses a native GitHub-hosted runner and Rust 1.98.0, runs the workspace tests, builds
the app and MCP server, checks their machine types, and starts the MCP server with
an isolated configuration directory. macOS dependencies are checked for unbundled
non-system libraries; Linux executables are checked with ldd.

## Build and package locally

Run on the matching operating system and CPU:

```sh
cargo test --workspace --release --locked --target-dir target/dist
cargo build --release --locked -p quarry-app -p quarry-ai --target-dir target/dist
python3 packaging/verify.py --platform macos --arch arm64
python3 packaging/release.py package --platform macos --arch arm64
```

Use `--platform linux` on Linux, `--arch x64` for Intel/AMD, and `python` instead
of `python3` on Windows. On Windows, use
`packaging/windows/build-installer.ps1` for the build: it links the static C runtime
and creates the EXE installer. Its optional `-Architecture x64|arm64` argument must
match the active Rust toolchain. macOS needs Xcode command-line tools; Linux's build
packages are listed in the workflow.

Outputs go to `target/downloads`:

- Windows: EXE installer and portable ZIP per CPU.
- macOS: DMG with an Applications shortcut, and a ZIP containing Quarry.app per CPU.
  The MCP helper sits next to the app executable inside Contents/MacOS.
- Linux: DEB with desktop entry, icon and computed shared-library dependencies, and
  a tarball per CPU. Portable binaries need the graphics/TLS libraries installed on
  the host; the Ubuntu 22.04 build establishes a glibc 2.35 baseline.

The platform packages preserve the existing settings locations and keychain service.
The Windows installer retains its AppId so upgrades find earlier installations.

## Publish

1. Update the workspace version in Cargo.toml and refresh Cargo.lock with
   `cargo update --workspace --offline`.
2. Commit and push to main. Check that all six native build jobs pass.
3. Create and push the corresponding annotated tag, for example `v0.1.3`.
4. The tag run rebuilds using main's dependency caches, downloads all six artifacts,
   requires all 12 expected packages, generates SHA256SUMS.txt, and publishes.

Publication starts from a draft. A failed upload leaves a draft that the same tag
run can complete on retry. The workflow refuses to replace a release that is already
public. Correct a public build with a new version instead of moving its tag.

## Signing and compatibility

Windows packages are unsigned. macOS binaries and the app bundle are ad hoc signed;
they are not Developer ID signed or Apple notarized. The download instructions make
this explicit. Frictionless macOS distribution needs an Apple Developer ID
certificate and notarization credentials; these are not part of the source repository.

Windows x64 targets Windows 10 1809 and newer; ARM64 installers target Windows 11.
macOS targets version 12 and newer and is built/tested on macOS 15. Linux targets
glibc-based distributions compatible with the Ubuntu 22.04 runtime. CI proves native
compilation, unit/SQLite tests and MCP startup; it does not exercise every desktop
environment, database server or old OS version.
