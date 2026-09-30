"""List which classes of a Python repo derive from which of its classes, per the runtime.

The accuracy benchmark adds these pairs to scip-python's implementation relationships,
which drop some direct subclass pairs (click's `BadParameter(UsageError)`).

Usage: python pytruth.py <repo dir> [source root ...]. Every module under the source roots
(default: the repo dir) is imported by its dotted name, the way the code itself imports it;
modules that fail to import (missing optional dependencies) are skipped, which only means
fewer pairs. Output, one pair per line, tab-separated, paths relative to the repo with
forward slashes and 1-based lines of the `class` statement:

    <class file> <class line> <class name> <base file> <base line> <base name>
"""

import importlib
import inspect
import io
import os
import re
import sys
import warnings
from contextlib import redirect_stderr, redirect_stdout


def module_names(root):
    for dirpath, dirnames, files in os.walk(root):
        dirnames[:] = [d for d in dirnames if not d.startswith((".", "__")) and d != "node_modules"]
        rel = os.path.relpath(dirpath, root)
        parts = [] if rel == "." else rel.split(os.sep)
        if parts and not all(p.isidentifier() for p in parts):
            continue
        for f in files:
            if f.endswith(".py") and f[:-3].isidentifier():
                name = parts + ([] if f == "__init__.py" else [f[:-3]])
                if name:
                    yield ".".join(name)


def class_line(cls, path):
    """1-based line of `class Name` (decorators excluded), searched from inspect's start."""
    try:
        start = inspect.getsourcelines(cls)[1]
        with open(path, encoding="utf-8") as fh:
            lines = fh.read().splitlines()
    except (OSError, TypeError):
        return None
    pat = re.compile(r"^\s*class\s+" + re.escape(cls.__name__) + r"\b")
    for i in range(max(start - 1, 0), len(lines)):
        if pat.match(lines[i]):
            return i + 1
    return None


def main():
    repo = os.path.abspath(sys.argv[1])
    roots = [os.path.join(repo, r) for r in sys.argv[2:]] or [repo]
    for r in reversed(roots):
        sys.path.insert(0, r)
    warnings.simplefilter("ignore")
    modules = []
    for r in roots:
        for name in module_names(r):
            try:
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                    modules.append(importlib.import_module(name))
            except BaseException:  # noqa: BLE001 - any import failure means no data
                continue

    def where(obj):
        try:
            path = inspect.getsourcefile(obj)
        except TypeError:
            return None
        if not path:
            return None
        path = os.path.abspath(path)
        if os.path.commonpath([path, repo]) != repo:
            return None
        line = class_line(obj, path)
        if line is None:
            return None
        return os.path.relpath(path, repo).replace(os.sep, "/"), line

    out = set()
    for m in modules:
        for _, cls in inspect.getmembers(m, inspect.isclass):
            if cls.__module__ != m.__name__:
                continue
            here = where(cls)
            if here is None:
                continue
            for base in cls.__mro__[1:]:
                there = where(base)
                if there is not None:
                    out.add(f"{here[0]}\t{here[1]}\t{cls.__name__}\t{there[0]}\t{there[1]}\t{base.__name__}")
    sys.stdout.write("".join(line + "\n" for line in sorted(out)))


if __name__ == "__main__":
    main()
