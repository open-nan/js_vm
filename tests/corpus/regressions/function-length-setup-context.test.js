// @expect 3
// @seeds 4

function runSetup(setup) {
  const context = setup.length > 1 ? { slots: { default: () => 3 } } : null;
  return setup({}, context);
}

const component = {
  setup(props, { slots }) {
    return slots.default();
  },
};

runSetup(component.setup);
