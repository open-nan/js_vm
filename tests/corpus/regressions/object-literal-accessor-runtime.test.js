// @expect 5
// @seeds 8

const ref = {
  _value: 1,
  get value() {
    return this._value;
  },
  set value(next) {
    this._value = next + 1;
  },
};

ref.value = 4;
ref.value;
