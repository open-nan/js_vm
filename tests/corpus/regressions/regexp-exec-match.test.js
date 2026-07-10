// @expect a
// @seeds 8

const r = RegExp("a", "g");
const direct = r.exec("cat")[0];
const matched = "cat".match(RegExp("a"))[0];

RegExp("z").test("cat") ? "bad" : direct + matched.slice(1);
