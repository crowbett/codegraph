/* tree-sitter's wasm <stdint.h> plus the constant macros SQLite uses. */
#include_next <stdint.h>
#ifndef CODEGRAPH_WEB_STDINT_H
#define CODEGRAPH_WEB_STDINT_H
#ifndef UINT64_C
#define UINT64_C(c) c##ULL
#endif
#ifndef INT64_C
#define INT64_C(c) c##LL
#endif
#endif
