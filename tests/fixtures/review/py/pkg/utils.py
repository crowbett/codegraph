from typing import Generic, TypeVar

T = TypeVar("T")


def echo(x):
    return x


class Base(Generic[T]):
    def fail(self):
        return 1
