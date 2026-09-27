class Group:
    def command(self):
        return self


class Base:
    def greet(self):
        return 1


class Child(Base):
    def greet(self):
        return super().greet()
