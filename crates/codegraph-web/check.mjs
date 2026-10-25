// node check.mjs <wasm> <answers.json>: the wasm build must import nothing and answer
// every call exactly as native codegraph does (sample/answers.json, written by
// `UPDATE_ANSWERS=1 cargo test webdemo` in the repo root).
import { readFileSync } from "node:fs";
import { isDeepStrictEqual } from "node:util";

const [file, answersFile] = process.argv.slice(2);
const bytes = readFileSync(file);
const imports = WebAssembly.Module.imports(new WebAssembly.Module(bytes));
if (imports.length) throw new Error(`imports: ${JSON.stringify(imports)}`);
// The ABI is memory + demo_*; the rest are #[no_mangle] C-facing functions rustc exports
// from a cdylib. Pinned so a new export shows up here.
const EXPORTS = "abort,calloc,demo_alloc,demo_call,demo_free,free,malloc,memory,qsort,realloc," +
  "sqlite3_os_end,sqlite3_os_init,strcspn,strrchr,strspn";
const exports = WebAssembly.Module.exports(new WebAssembly.Module(bytes)).map((e) => e.name).sort().join();
if (exports !== EXPORTS) throw new Error(`exports changed: ${exports}`);
const { instance } = await WebAssembly.instantiate(bytes, {});
const x = instance.exports;
const call = (method, params) => {
  const put = (s) => {
    const b = new TextEncoder().encode(s);
    const p = x.demo_alloc(b.length);
    new Uint8Array(x.memory.buffer, p, b.length).set(b);
    return [p, b.length];
  };
  const [mp, ml] = put(method);
  const [pp, pl] = put(JSON.stringify(params));
  const r = x.demo_call(mp, ml, pp, pl);
  const n = new DataView(x.memory.buffer).getUint32(r, true);
  const out = JSON.parse(new TextDecoder().decode(new Uint8Array(x.memory.buffer, r + 4, n)));
  x.demo_free(mp, ml);
  x.demo_free(pp, pl);
  x.demo_free(r, n + 4);
  return out;
};
const want = JSON.parse(readFileSync(answersFile, "utf8"));
const got = { sample: call("sample", {}), calls: {} };
for (const spec of Object.keys(want.calls)) {
  got.calls[spec] = {
    callers: call("callers", { symbol: spec }),
    callees: call("callees", { symbol: spec }),
  };
}
for (const [k, v] of Object.entries(got.calls)) {
  if (!isDeepStrictEqual(v, want.calls[k])) throw new Error(`${k}: wasm answer differs from native`);
}
if (!isDeepStrictEqual(got.sample, want.sample)) throw new Error("sample listing or edge counts differ from native");
console.log(`ok: 0 imports, sample and ${Object.keys(got.calls).length} symbols x callers/callees match native`, got.sample.counts);
