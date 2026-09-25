## Summary

<!-- What does this PR do? Link related issues with "Closes #123". -->

## Changes

<!-- Bullet list of the meaningful changes. -->

## Type of change

- [ ] `feat` (new capability)
- [ ] `fix` (bug fix)
- [ ] `docs`
- [ ] `test`
- [ ] `bench`
- [ ] `chore`

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes (locally with the `.githooks` hook installed)
- [ ] New logic has table-driven unit tests; security-sensitive paths covered
- [ ] CHANGELOG.md updated (for user-visible changes)
- [ ] Conventional-commit style title
- [ ] No new system-binary invocations (`ssh`, `rsync`, `tar`, `cp`, …)
- [ ] Wire paths normalized (`to_wire_path`/`from_wire_path`) if protocol touched