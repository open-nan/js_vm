// @expect recovered:7
// @seeds 8

let value = "start";

try {
  throw 6;
} catch (err) {
  value = `recovered:${err + 1}`;
} finally {
  value = value ?? "fallback";
}

value;
