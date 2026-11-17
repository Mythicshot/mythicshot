// node --test crates/app/tests/
// Runs tour_core.js against the shared fixture that tour/core.rs is also tested against.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import vm from "node:vm";

const here = new URL(".", import.meta.url);
const src = readFileSync(new URL("../src/tour_core.js", here), "utf8");
const cases = JSON.parse(readFileSync(new URL("fixtures/tour-core-cases.json", here), "utf8"));

// Loaded the way a page inlines it: a classic script in a bare context with no DOM.
const ctx = vm.createContext({});
vm.runInContext(src, ctx);
const TourCore = ctx.TourCore;

// The fixture writes non-finite numbers as strings, since JSON has none.
const n = (v) => (v === "NaN" ? NaN : v === "Infinity" ? Infinity : v === "-Infinity" ? -Infinity : v);

const close = (a, b, what) => assert.ok(Math.abs(a - b) <= 0.01, `${what}: ${a} vs ${b}`);

test("loads as a classic script with TourCore as its only global, and as a module", () => {
  assert.deepEqual(Object.keys(ctx), ["TourCore"]);
  assert.equal(/\b(document|window|innerHTML|outerHTML|insertAdjacentHTML|eval)\b/.test(src.replace(/^\s*\/\/.*$/gm, "")), false);
  const mod = createRequire(import.meta.url)("../src/tour_core.js");
  assert.deepEqual(Object.keys(mod).sort(), Object.keys(TourCore).sort());
});

test("place cases", () => {
  for (const c of cases.place) {
    const [x0, y0, x1, y1] = (c.avoid || []).map(n);
    const avoid = c.avoid && { x0, y0, x1, y1 };
    const p = TourCore.placeCard(c.image[0], c.image[1], c.card[0], c.card[1], avoid, c.placement);
    close(p.x, c.expect.x, `${c.name} x`);
    close(p.y, c.expect.y, `${c.name} y`);
    assert.equal(p.side, c.expect.side, c.name);
  }
});

test("zoom cases", () => {
  for (const c of cases.zoom) {
    const z = TourCore.zoomOffset(n(c.view[0]), n(c.view[1]), n(c.focus[0]), n(c.focus[1]), n(c.k));
    close(z.tx, c.expect.tx, `${c.name} tx`);
    close(z.ty, c.expect.ty, `${c.name} ty`);
  }
});

test("bold cases", () => {
  for (const c of cases.bold) {
    // Spread first: the pieces come from the script's own realm.
    const got = [...TourCore.parseBold(c.text)].map((p) => [p.text, p.bold]);
    assert.deepEqual(got, c.expect, c.name);
  }
});

test("bold of anything but a string is no pieces", () => {
  for (const v of [null, undefined, 42, {}, ["**a**"]]) assert.equal(TourCore.parseBold(v).length, 0);
});

test("url cases", () => {
  for (const c of cases.url) {
    assert.equal(TourCore.safeUrl(c.input), c.expect, JSON.stringify(c.input));
  }
  assert.equal(TourCore.safeUrl(undefined), null);
  assert.equal(TourCore.safeUrl(42), null);
});

test("text cases", () => {
  for (const c of cases.text) {
    assert.equal(TourCore.cleanText(c.input, c.max, c.multiline), c.expect, c.name);
  }
  assert.equal(TourCore.cleanText(null, 10, false), "");
});

test("color cases", () => {
  for (const c of cases.color) {
    assert.equal(TourCore.color(c.input, null), c.expect, JSON.stringify(c.input));
  }
});

test("num and color", () => {
  assert.equal(TourCore.num("1e9", 0, 100, 50), 50);
  assert.equal(TourCore.num(NaN, 0, 100, 50), 50);
  assert.equal(TourCore.num(Infinity, 0, 100, 50), 50);
  assert.equal(TourCore.num(140.4, 105, 140, 118), 140);
  assert.equal(TourCore.num(90, 105, 140, 118), 105);
  assert.equal(TourCore.color("#FFAA00", "#5dcaa5"), "#ffaa00");
  assert.equal(TourCore.color("red;x", "#5dcaa5"), "#5dcaa5");
  assert.equal(TourCore.color("#ffaa0", "#5dcaa5"), "#5dcaa5");
  assert.equal(TourCore.color(0xffaa00, "#5dcaa5"), "#5dcaa5");
});
