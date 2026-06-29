// @expect item:a3
// @seeds 8

const items = [1, 2, 3];
const [first, second, third] = items;
const label = `item:a${third}`;

label;
