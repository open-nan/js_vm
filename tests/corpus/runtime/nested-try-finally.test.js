// @expect 7
// @seeds 8

let value = 0;

try {
  try {
    throw 4;
  } catch (err) {
    value = err + 1;
  } finally {
    value = value + 2;
  }
} catch (outer) {
  value = 99;
}

value;
