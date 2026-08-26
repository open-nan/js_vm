// @expect 15
// @seeds 4

const queue = [];

function enqueue(job) {
  Array.isArray(job) ? queue.push(...job) : queue.push(job);
}

const first = () => 7;
const second = () => 8;
enqueue([first, second]);

const deduped = [...new Set(queue)];
deduped[0]() + deduped[1]();
