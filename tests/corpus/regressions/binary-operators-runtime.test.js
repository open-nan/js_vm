// @expect 29
// @seeds 8

const bits = ((5 & 3) | 8) ^ 1;
const shifted = (bits << 1) + (16 >> 2) + (-1 >>> 31);
const power = 2 ** 3;
const obj = { a: 1 };
const arr = [1];
const ok = ("a" in obj) && ("length" in arr) && (arr instanceof Array);

shifted + power;
