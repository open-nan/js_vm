// @expect 28
// @seeds 4

const wrapped = Object(2n) + 1n;
const symbolic = {
  [Symbol.toPrimitive]: function() {
    return 3n;
  },
} + 1n;
const viaValueOf = {
  valueOf: function() {
    return 4n;
  },
} + 1n;
const viaToString = {
  valueOf: null,
  toString: function() {
    return 5n;
  },
} + 1n;
const loose = Object(6n) == 6n ? 10n : 0n;

wrapped + symbolic + viaValueOf + viaToString + loose;
