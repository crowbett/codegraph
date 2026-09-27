import pkg


class Custom(pkg.Group):
    pass


def run():
    pkg.utils.helper()
    c = Custom()
    c.command()


@pkg.deco.group()
def cli():
    pass


@cli.command()
def sub():
    pass
