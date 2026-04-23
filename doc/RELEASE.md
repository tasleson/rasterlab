# Cutting a release

RasterLab releases are tag-driven. Pushing a tag matching `v*` triggers
`.github/workflows/release.yml`, which builds binaries on GitHub-hosted
runners and publishes them to a GitHub Release.

## Prerequisites

- You have push access to the `main` branch and permission to create tags.
- Working tree is clean (`git status` shows no changes).
- Pre-commit checklist from `CLAUDE.md` has been run on the commit you
  intend to tag:

  ```bash
  cargo fmt
  cargo clippy
  cargo bench
  cargo build --release
  ```

## Steps

1. **Pick the next version.** Follow semver. Update `version` in the
   workspace `Cargo.toml` (`[workspace.package]`) if it does not already
   match the release you want to cut.

2. **Commit the version bump** (if you made one) and push to `main`:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "release: v0.3.0"
   git push origin main
   ```

3. **Create and push the tag:**

   ```bash
   git tag v0.3.0
   git push origin v0.3.0
   ```

4. **Watch the workflow.** Go to the repo's Actions tab on GitHub, or
   run `gh run watch`. Two build jobs run in parallel
   (`linux-x86_64`, `macos-aarch64`), then a `release` job uploads the
   artifacts.

5. **Verify the release.** When the workflow finishes, a new release
   appears under the repo's Releases page with:

   - `rasterlab-v0.3.0-linux-x86_64.tar.gz` (+ `.sha256`)
   - `rasterlab-v0.3.0-macos-aarch64.tar.gz` (+ `.sha256`)

   Release notes are auto-generated from commits since the previous tag.

## If something goes wrong

- **Workflow failed mid-build.** Fix the issue on `main`, then delete
  and re-push the tag:

  ```bash
  git tag -d v0.3.0
  git push origin :refs/tags/v0.3.0
  # fix, commit, push main
  git tag v0.3.0
  git push origin v0.3.0
  ```

  If a partial GitHub Release was created, delete it from the web UI or
  with `gh release delete v0.3.0` before re-pushing.

- **Linux build fails on a missing system library.** The eframe/rfd
  dependency list drifts occasionally. Add the missing package to the
  `Install Linux dependencies` step in `release.yml` and push a fix.

- **macOS users report a Gatekeeper warning.** The binaries are
  unsigned. Users can right-click the binary and choose **Open**, or
  run `xattr -d com.apple.quarantine <path>`. Code signing requires a
  paid Apple Developer account and is out of scope.

## Notes

- Runner minutes on GitHub-hosted runners are free for public repos.
- Only first-party actions (`actions/checkout`, `actions/cache`,
  `actions/upload-artifact`, `actions/download-artifact`) plus the
  pre-installed `gh` CLI are used — no third-party dependencies.
- Windows builds are intentionally disabled.
