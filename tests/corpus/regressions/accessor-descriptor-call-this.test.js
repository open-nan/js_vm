// @expect 8|11
// @seeds 8

const box = {
  base: 7,
  get value() {
    return this.base + 1;
  },
};

const descriptor = Object.getOwnPropertyDescriptor(box, "value");
descriptor.get.call(box) + "|" + descriptor.get.call({ base: 10 });
