// @expect 165110
// @seeds 8

let score = 0;

function* counter() {
  score += 1;
  const delta = yield score;
  score += delta;
  yield score;
  return score + 1;
}

const iterator = counter();
const beforeStart = score;
const first = iterator.next();
const afterFirstYield = score;
const second = iterator.next(4);
const third = iterator.next();

beforeStart
  + first.value * 10
  + afterFirstYield * 100
  + second.value * 1000
  + third.value * 10000
  + (third.done ? 100000 : 0);
