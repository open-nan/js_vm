// @expect 10:first/second:ReferenceError:1
// @seeds 8

var count = 0;
outer: {
  while (true) {
    count += 1;
    if (count === 10) break outer;
  }
  count = 100;
}

var first;
var second = null;
for (let value = "first"; second === null; value = "second") {
  if (!first) {
    first = function () {
      return value;
    };
  } else {
    second = function () {
      return value;
    };
  }
}

var status = "none";
try {
  while (x !== 1) {
    var x = 1;
    missingReference;
  }
} catch (error) {
  status = error.name;
}

`${count}:${first()}/${second()}:${status}:${x}`;
