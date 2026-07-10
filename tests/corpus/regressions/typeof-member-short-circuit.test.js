// @expect browser
// @seeds 8
const target =
  typeof module === "object" && typeof module.exports === "object"
    ? "commonjs"
    : "browser";
target;
