// @expect /home
// @seeds 6

const state = {
  path: "/home",
  fullPath: "/home?tab=1",
  redirectedFrom: undefined,
};

const route = {};
for (const key in state) {
  Object.defineProperty(route, key, {
    enumerable: true,
    get: () => state[key],
  });
}

route.path;
