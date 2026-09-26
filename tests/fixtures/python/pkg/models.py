"""Models."""


class Base:
    def save(self):
        """Persist the object."""
        return self.validate()

    def validate(self):
        return True


class User(Base):
    """A user."""

    def rename(self, name):
        self.name = name
        return self.save()
