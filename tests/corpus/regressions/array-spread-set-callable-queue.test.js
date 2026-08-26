// @expect 7
// @seeds 4

const queue = [];
function pushJob(job) {
  Array.isArray(job) ? queue.push(...job) : queue.push(job);
}

const job = () => 7;
pushJob(job);

const deduped = [...new Set(queue)].sort((left, right) => {
  const leftId = left.id == null ? Infinity : left.id;
  const rightId = right.id == null ? Infinity : right.id;
  return leftId - rightId;
});

deduped[0]();
