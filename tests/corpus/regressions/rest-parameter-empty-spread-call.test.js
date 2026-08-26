// @expect 7
// @seeds 4

const calls = [];

function plugin(app, first, second) {
  calls.push(app.value + (first || 0) + (second || 0));
}

const app = {
  value: 7,
  use(item, ...options) {
    item.apply(undefined, [this, ...options]);
    return this;
  },
};

app.use(plugin);
calls[0];
