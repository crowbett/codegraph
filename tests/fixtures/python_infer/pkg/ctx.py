import typing as t


class Context:
    def __enter__(self) -> t.Self:
        return self

    def fail(self, msg):
        return msg


class Command:
    context_class: type[Context] = Context

    def make_context(self) -> Context:
        return self.context_class()

    def invoke(self, ctx: Context):
        return ctx


class Special(Command):
    def special(self):
        return 1


class Bar:
    def update(self, n):
        return n


def progress() -> Bar:
    return Bar()


def pairs() -> t.Iterator[tuple[str, Command]]:
    yield ("a", Command())
