#include "util.h"

/* Adds two ints. */
int util_add(int a, int b) { return a + b; }

static int twice(int x) { return util_add(x, x); }
