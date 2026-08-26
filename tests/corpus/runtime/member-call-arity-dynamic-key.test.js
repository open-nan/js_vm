// @expect 29
// @seeds 8

const key0 = "zero";
const key1 = "one";
const key2 = "two";

const box = {
  value: 4,
  zero() {
    return this.value;
  },
  one(input) {
    return this.value + input;
  },
  two(left, right) {
    return this.value + left * right;
  },
};

box.zero() + box[key0]() + box[key1](3) + box[key2](2, 5);
