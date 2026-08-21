# Build and release tooling

This directory is the home for repository-level build, packaging, signing,
publishing, and release scripts. Run scripts from any working directory; each
script resolves the repository root from its own location.

## Common entry points

- `package-linux-nfpm.sh`: build deb, rpm, and apk packages from existing CLI
  and daemon binaries.
- `package-linux-deb.sh`: compatibility wrapper for deb-only packaging.
- `set-release-number.sh`: update workspace and package metadata for a release.
- `submit-release.sh` and `upload-r2.sh`: publish release metadata/artifacts;
  these require external credentials and intentionally are not run by tests.
- `windows-build.sh` and `windows-sign.sh`: build/sign Windows artifacts.
- `cargo-publish.sh`: publish reusable crates in dependency order.
- `mac-deploy.sh`: legacy macOS packaging entry point; it exits with guidance
  when the separately maintained `macos-app.sh` helper is unavailable.

Packaging data remains in `linux/`, `docker/`, and `arch/` because Docker,
nFPM, and downstream PKGBUILDs consume those paths directly. The scripts that
orchestrate those assets live here. The existing release workflow continues to
invoke `build-scripts/package-linux-nfpm.sh`; no hosting-provider tooling is
required for local builds.

For local validation run:

```sh
bash -n build-scripts/*.sh build-scripts/nfpm/*.sh \
  build-scripts/nfpm/openrc/*.sh arch/*.sh
cargo test --workspace
```
