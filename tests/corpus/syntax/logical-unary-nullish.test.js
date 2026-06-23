// @expect 9
// @seeds 8

const a = null ?? 4;
const b = undefined ?? 5;
const ok = !false && typeof a === "number";

ok ? a + b : 0;
