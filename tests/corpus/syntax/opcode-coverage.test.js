// @expect 9
// @seeds 8

import { missing as importedValue } from "coverage-fixture";

debugger;

function hiddenUnsupportedOpcode() {
  const copy = { ...{ value: 1 } };
  return copy.value;
}

function increment(value) {
  return value + 1;
}

const base = 8;
const output = increment(base);

output;
