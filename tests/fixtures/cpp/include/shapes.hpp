#pragma once

namespace geo {

class Shape {
 public:
  virtual ~Shape() = default;
  virtual double area() const = 0;
};

class Circle : public Shape {
 public:
  explicit Circle(double r);
  double area() const override;

 private:
  double r_;
};

double total(const Shape& a, const Shape& b);

}  // namespace geo
