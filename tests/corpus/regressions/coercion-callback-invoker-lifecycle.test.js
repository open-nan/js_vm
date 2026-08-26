// @expect 7:3|4|symbol|function
// @seeds 4

let called = 0;
const object = {
  [Symbol.toPrimitive]: function() {
    called = 7;
    return 3;
  },
};

const bigintObject = {
  [Symbol.toPrimitive]: function() {
    return 3n;
  },
};

const unary = +object;
const bigint = bigintObject + 1n;
const symbolType = typeof Symbol.toPrimitive;
const memberType = typeof object[Symbol.toPrimitive];

called + ":" + unary + "|" + bigint + "|" + symbolType + "|" + memberType;
