// @expect 7:true
// @seeds 6

function createApp() {
  const app = {
    value: 7,
    use(plugin) {
      plugin?.({ app });
      return this;
    },
  };

  for (const item of [{ enhance: undefined }]) {
    item.enhance?.({ app });
  }

  return `${app.value}:${app.use() === app}`;
}

createApp();
