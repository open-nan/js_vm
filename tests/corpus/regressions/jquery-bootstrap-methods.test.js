// @expect 18
// @seeds 8

const tokens = " FIELDSET input ".match(/[^\x20\t\r\n\f]+/g);
const name = tokens[0].toLowerCase();
const normalized = "a-b-c".replace(/-/g, "+");

const values = [3, 1, 2];
values.sort();
const removed = values.splice(1, 1)[0];

const flat = [[1], [2, 3]].flat();

name.length + normalized.length + removed + flat[2];
