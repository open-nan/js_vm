// @expect 5
// @seeds 8

function makeCounter() {
  let value = 1;

  return function next() {
    value = value + 1;
    return value;
  };
}

const next = makeCounter();
const a = next();
const b = next();

a + b;
