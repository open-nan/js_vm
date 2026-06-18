// @expect 42
// @seeds 10

console.log("chain", 42);

const kind = typeof console;
kind === "object" ? 42 : 0;
