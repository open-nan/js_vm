// @expect 13
// @seeds 12

function outer(base) {
  return function inner(value) {
    return base + value;
  };
}

const add = outer(8);
add(5);
