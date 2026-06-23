// @expect 13
// @seeds 8

const data = {
  items: [2, 4, 6],
  first() {
    return this.items[0];
  },
};

data.items[2] + data.items.length + data.first() + 2;
