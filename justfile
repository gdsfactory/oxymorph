# initialize repo
init:
  git submodule update --init --recursive

# build the Python package (release, via uv + maturin) and run its tests
test-python *args:
  uv sync --reinstall-package oxymorph
  uv run --no-sync python -m unittest discover -s crates/oxymorph-python/tests -v {{args}}
