// @expect 3
// @seeds 8

typeof e;

function make() {
  var e = [];
  return function(value) {
    e.push(value);
    return e.length;
  };
}

const add = make();
add(1) + add(2);
