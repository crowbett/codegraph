#include "shapes.hpp"

namespace geo {

Circle::Circle(double r) : r_(r) {}

double Circle::area() const { return 3.14159 * r_ * r_; }

double total(const Shape& a, const Shape& b) { return a.area() + b.area(); }

}  // namespace geo
