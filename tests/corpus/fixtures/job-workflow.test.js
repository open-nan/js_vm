// @expect READY:24
// @seeds 10

class Job {
  constructor(name, values) {
    this.name = name;
    this.values = values;
  }

  total() {
    return this.values[0] + this.values[1] + this.values[2];
  }
}

function describe(job) {
  const total = job.total();
  return `${job.name}:${total}`;
}

const job = new Job("READY", [7, 8, 9]);
describe(job);
