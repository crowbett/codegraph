import Widget from "./widget.js";
const { slugify } = require("../lib/util");

export function start() {
  const w = new Widget();
  w.render();
  return slugify("Hello");
}

start();
