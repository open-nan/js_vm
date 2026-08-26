// @expect 11|17|construct:1,target:5
// @seeds 8

const events = [];

function add(a, b) {
  events.push("target:" + this.base);
  return this.base + a + b;
}

const callable = new Proxy(add, {
  apply(target, thisArg, args) {
    return Reflect.apply(target, { base: 5 }, args) + 1;
  },
  construct(target, args) {
    events.push("construct:" + args[0]);
    return { value: args[0] + 16 };
  },
});

const made = new callable(1);
callable(2, 3) + "|" + made.value + "|" + events.join(",");
