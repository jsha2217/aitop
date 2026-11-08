# Preparing a release

Publishing has not been performed. `Prepare release artifacts` is a manual GitHub Actions workflow that builds archives; it does not create a GitHub Release, upload credentials, or publish a package.

1. Run formatting, strict Clippy, tests, and a release build on macOS and Linux.
2. Compare provider values manually with their own tools using designated test accounts. Record the CLI/app version and supported fields. A fixture test alone is not live certification.
3. Verify the actual TUI in a terminal, including light/dark appearance, narrow widths, keyboard input, quit, Ctrl-C, and restoration. Regenerate assets from the demo renderer.
4. Build a target and prepare a local archive:

   ```sh
   cargo build --release --locked --target aarch64-apple-darwin
   python3 scripts/package_release.py --target aarch64-apple-darwin
   ```

5. Inspect archive contents and checksum. Test a clean installation from the archive. The arm64 macOS build must not be labeled as an Intel binary; each archive names its target.
6. After review, publish the tag and release artifacts. Do not claim notarization or code signing unless those steps have actually been completed.
7. Only after the actual download URL and checksum exist, create/test a Homebrew Formula in the intended tap with that URL and checksum, then add its working install command to README. A hypothetical formula or unpublished tap is not an installation method.

Linux arm64, Windows, automated self-updates, signing/notarization, and hosted Homebrew installation remain outside this source preview. No misleading placeholder download URLs are provided. Cargo source installation is available now.
