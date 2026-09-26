#include <stdio.h>
#include "util.h"

int main(void) {
    struct point p = {1, 2};
    printf("%d\n", util_add(p.x, p.y));
    return 0;
}
