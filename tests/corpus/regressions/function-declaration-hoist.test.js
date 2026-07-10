// @expect 3
// @seeds 8

function outer(value) {
  return later(value);

  function later(input) {
    return input + 1;
  }
}

outer(2);
