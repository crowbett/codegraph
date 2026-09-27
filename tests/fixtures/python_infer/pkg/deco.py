import typing as t

from .core import Group


def group() -> t.Union[Group, t.Callable[[t.Callable[..., t.Any]], Group]]:
    def wrap(f):
        return Group()

    return wrap
