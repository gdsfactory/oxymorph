# ponytail: every name is `Any`, so type checkers resolve oxymorph's API without checking it. Write per-class stubs
# when callers want their oxymorph calls type-checked.
from typing import Any

def __getattr__(name: str) -> Any: ...
