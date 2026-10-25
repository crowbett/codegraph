// node run.mjs <wasm> <method> <json params>
import { readFileSync } from "node:fs";
const [file, method = "callers", params = '{"symbol":"parse"}'] = process.argv.slice(2);
const { instance } = await WebAssembly.instantiate(readFileSync(file), {});
const x = instance.exports;
console.error("imports:", WebAssembly.Module.imports(new WebAssembly.Module(readFileSync(file))).length);
const put = (s) => {
  const b = new TextEncoder().encode(s);
  const p = x.demo_alloc(b.length);
  new Uint8Array(x.memory.buffer, p, b.length).set(b);
  return [p, b.length];
};
const [mp, ml] = put(method);
const [pp, pl] = put(params);
const r = x.demo_call(mp, ml, pp, pl);
const n = new DataView(x.memory.buffer).getUint32(r, true);
console.log(new TextDecoder().decode(new Uint8Array(x.memory.buffer, r + 4, n)));
x.demo_free(mp, ml); x.demo_free(pp, pl); x.demo_free(r, n + 4);
