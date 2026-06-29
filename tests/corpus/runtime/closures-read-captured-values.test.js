// @expect 27
// @seeds 8

function makeScale(base) {
  return function scale(value) {
    return base * value;
  };
}

const scale = makeScale(3);
scale(4) + scale(5);
