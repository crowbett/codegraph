import typing as t

import pkg.deco
from pkg.ctx import Command, Special, pairs, progress


class Sub(Command):
    def invoke(self, ctx):
        return ctx.fail("x")


@pkg.deco.group(cls=Special)
def cli2():
    pass


def run(obj, table: dict[str, Command]):
    with progress() as bar:
        bar.update(1)
    c = t.cast(Command, obj)
    c.make_context()
    if isinstance(obj, Special):
        obj.special()
    for name, cmd in pairs():
        cmd.make_context()
    table["k"].make_context()
    table.get("k").make_context()
    ctx = Command().context_class()
    ctx.fail("y")
    with Command().make_context() as ctx2:
        ctx2.fail("z")
    cli2.special()
    return [c2.make_context() for n, c2 in pairs()]


def filter(items):
    return items


def uses_filter():
    return filter([1])


def make_bar():
    b = progress()
    return b


def uses_helper():
    make_bar().update(2)


def pager():
    from pkg.utils import helper

    return helper()


def helper():
    return 2
