// @expect 10
// @seeds 12

this.alert = console.log;

let value = 1;
try {
  null.value;
} catch (err) {
  value = value + 4;
}

this.answer = value + 5;
this.answer;
