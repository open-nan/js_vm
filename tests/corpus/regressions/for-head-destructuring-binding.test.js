// @expect 50
// @seeds 8

function Test262Error() {}

let total = 0;
const poisonedObject = Object.defineProperty({}, "poisoned", {
  get: function () {
    throw new Test262Error();
  },
});

try {
  for (const { poisoned } = poisonedObject; ; ) {}
} catch (err) {
  total += err instanceof Test262Error ? 1 : 100;
}

for (const [head = 3, ...tail] = [undefined, 4, 5]; total < 20; total += head + tail[1]) {}

const key = "score";
for (let { [key]: value = 6 } = { score: 7 }; total < 40; total += value) {}

for (let { named = function () {} } = {}; total < 50; total += named.name === "named" ? 4 : 100) {}

total;
