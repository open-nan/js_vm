// @expect-async ready
// @seeds 8

let state = "start";

async function create() {
  const marker = await "ready";
  return {
    mount() {
      state = marker;
    },
  };
}

create().then(({ mount }) => {
  mount();
});

state;
