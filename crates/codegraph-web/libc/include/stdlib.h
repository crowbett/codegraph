/* tree-sitter's wasm <stdlib.h> plus what SQLite also calls (defined in src/sys.rs). */
#include_next <stdlib.h>
#ifndef CODEGRAPH_WEB_STDLIB_H
#define CODEGRAPH_WEB_STDLIB_H
void qsort(void *base, size_t n, size_t size, int (*cmp)(const void *, const void *));
#endif
