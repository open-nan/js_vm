// @expect pending
// @seeds 4

async function attachAfterAwait(targetPromise) {
  const target = await targetPromise;
  if (target === undefined) {
    return "skipped-native-promise";
  }
  target.addEventListener("change", () => {});
  return "attached";
}

let result = "pending";
attachAfterAwait(
  Promise.resolve({
    addEventListener() {
      result = "leaked-promise-target";
    },
  }),
).then((value) => {
  result = value;
});

result;
