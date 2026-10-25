/* Minimal <time.h> for SQLite on wasm32-unknown-unknown (src/sys.rs has no clock). */
#ifndef CODEGRAPH_WEB_TIME_H
#define CODEGRAPH_WEB_TIME_H
#include <stddef.h>
typedef long long time_t;
struct tm {
  int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
};
struct tm *localtime_r(const time_t *t, struct tm *out);
#endif
