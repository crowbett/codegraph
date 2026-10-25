/* Minimal <math.h> for SQLite on wasm32-unknown-unknown; Rust's compiler-builtins
   supplies the functions. */
#ifndef CODEGRAPH_WEB_MATH_H
#define CODEGRAPH_WEB_MATH_H
#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define HUGE_VAL __builtin_huge_val()
#define INFINITY __builtin_inff()
#define NAN __builtin_nanf("")
double fabs(double x);
double log(double x);
#endif
