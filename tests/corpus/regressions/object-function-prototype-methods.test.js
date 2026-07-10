// @expect function Object() { [native code] }
// @seeds 8

const class2type = {};
const hasOwn = class2type.hasOwnProperty;
const fnToString = hasOwn.toString;
const ok = hasOwn.call({ value: 1 }, "value");

ok ? fnToString.call(Object) : "missing";
