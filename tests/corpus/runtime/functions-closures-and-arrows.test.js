// @expect 16
// @seeds 8

function makeAdder(base) {
  return function add(value) {
    return base + value;
  };
}

const addFive = makeAdder(5);
const inc = (n) => n + 1;

addFive(10) + inc(0);
