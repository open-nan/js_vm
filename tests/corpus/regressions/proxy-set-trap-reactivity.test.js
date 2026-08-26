// @expect 7
// @seeds 4

const target = { value: 1 };
const state = new Proxy(target, {
  get(object, key) {
    return object[key];
  },
  set(object, key, value) {
    object[key] = value + 2;
    return true;
  },
});

state.value = state.value + 4;
state.value;
