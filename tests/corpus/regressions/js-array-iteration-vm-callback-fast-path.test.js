// @expect 45
// @seeds 8

const thisArg = { bias: 1 };
const values = [1, 2, 3, 4];

const mapped = values.map(function (value, index, array) {
  return value + this.bias + index + array.length;
}, thisArg);

const filtered = mapped.filter(function (value, index) {
  return value % 4 === 0 || index === 2;
});

let total = 0;
filtered.forEach(function (value, index) {
  total += value + index;
});

const allLarge = filtered.every(function (value) {
  return value > 7;
});

const hasTen = filtered.some(function (value) {
  return value === 10;
});

const found = filtered.find(function (value) {
  return value > 9;
});

total + found + (allLarge ? 1 : 0) + (hasTen ? 1 : 0);
