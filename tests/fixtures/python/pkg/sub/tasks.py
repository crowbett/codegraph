from ..util import helper
from .. import models
from ..models import User as U

MAX_TASKS = 4


def run():
    helper()
    u = U()
    models.User()
    return u.rename("x")


def registry(kind: U | None = None) -> dict:
    handlers = {"run": run, "user": U}
    default = helper
    local = handlers
    return local


def kinds():
    cls = models.User
    return cls is U
