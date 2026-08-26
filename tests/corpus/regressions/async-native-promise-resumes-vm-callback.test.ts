// @expect-async done:7
// @seeds 4

async function readLater() {
  const target = await new Promise((resolve) => {
    resolve({
      base: 4,
      run(value) {
        return "done:" + (this.base + value);
      },
    });
  });

  return target.run(3);
}

readLater();
