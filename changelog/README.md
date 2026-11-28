# Changelog Fragments

Add a fragment file here for every user-facing change on your branch.

**Naming:** `<issue>.<type>.md` or `+<description>.<type>.md` (orphan, no issue).

**Types:** `feat`, `fix`, `perf`, `refactor`, `docs`, `removal`

**Example:**
```
echo "Add support for the Ngspice dialect." > changelog/42.feat.md
uvx towncrier create 42.feat.md --content "Add support for the Ngspice dialect."
```

At release, bump the version in `pyproject.toml`, `Cargo.toml` and `crates/oxymorph-python/Cargo.toml` (the
release workflow checks all three against the tag), then run `uvx towncrier build --version X.Y.Z` to compile
the fragments into `CHANGELOG.md`.
