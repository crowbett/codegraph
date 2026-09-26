from ..util import helper
from .. import models
from ..models import User as U

MAX_TASKS = 4


def run():
    helper()
    u = U()
    models.User()
    return u.rename("x")
