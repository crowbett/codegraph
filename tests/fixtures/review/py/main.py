import pkg.utils
from pkg.utils import Base


class Child(Base[int]):
    def run(self):
        return self.fail()


def main():
    pkg.echo(1)
