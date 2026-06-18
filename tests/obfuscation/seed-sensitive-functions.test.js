// @expect 37
// @seeds 12

function square(n) {
  return n * n;
}

const left = square(5);
const right = square(3);

left + right + 3;
