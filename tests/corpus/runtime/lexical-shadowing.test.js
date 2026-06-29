// @expect 13
// @seeds 8

let value = 1;

function makeAdder() {
  let value = 10;

  return function add(delta) {
    value = value + delta;
    return value;
  };
}

const add = makeAdder();
const inner = add(2);

value + inner;
