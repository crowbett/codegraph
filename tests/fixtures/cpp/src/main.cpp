#include "shapes.hpp"

int main() {
  geo::Circle c(2.0);
  geo::Circle d(1.0);
  return static_cast<int>(geo::total(c, d));
}
