// @expect 3
// @seeds 8

const arr = [];
arr.push.apply(arr, [1, 2]);

const flat = [].concat.apply([], [[arr.length], [3]]);
flat[1];
