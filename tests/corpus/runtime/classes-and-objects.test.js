// @expect 18
// @seeds 8

class Counter {
  constructor(start) {
    this.value = start;
  }

  inc(step) {
    this.value = this.value + step;
    return this.value;
  }

  static base() {
    return 10;
  }
}

const counter = new Counter(3);
counter.inc(5) + Counter.base();
