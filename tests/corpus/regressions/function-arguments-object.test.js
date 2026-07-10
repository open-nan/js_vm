// @expect 9
// @seeds 8

function extend() {
  const target = arguments[0] || {};
  target.value = arguments[1];
  return target.value;
}

extend({}, 9);
