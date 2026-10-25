/* tree-sitter's wasm <string.h> plus what SQLite also calls (defined in src/sys.rs). */
#include_next <string.h>
#ifndef CODEGRAPH_WEB_STRING_H
#define CODEGRAPH_WEB_STRING_H
size_t strspn(const char *s, const char *accept);
size_t strcspn(const char *s, const char *reject);
char *strrchr(const char *s, int c);
#endif
