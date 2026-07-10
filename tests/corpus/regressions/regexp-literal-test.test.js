// @expect 3
// @seeds 8

const html = /HTML$/i.test("HTML") ? 1 : 0;
const miss = /XML$/i.test("HTML") ? 0 : 2;
html + miss;
