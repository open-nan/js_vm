// @expect 23
// @seeds 8

function readOuter() {
  let value = 1;
  {
    let value = 9;
    value = value + 1;
  }
  return value;
}

let total = readOuter();

while (total < 10) {
  let inner = 10;
  total = total + inner;
  break;
}

total + (typeof inner === "undefined" ? 12 : 100);
