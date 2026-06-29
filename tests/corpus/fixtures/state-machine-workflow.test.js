// @expect build:pass:8
// @seeds 10

class Task {
  constructor(name, score) {
    this.name = name;
    this.score = score;
  }

  pass() {
    return this.score > 5;
  }
}

function evaluate(task) {
  let status = "new";

  switch (task.pass() ? 1 : 0) {
    case 1:
      status = "pass";
      break;
    default:
      status = "fail";
  }

  return `${task.name}:${status}:${task.score}`;
}

const task = new Task("build", 8);
evaluate(task);
