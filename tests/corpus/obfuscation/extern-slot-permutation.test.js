// @expect extern-ok
// @seeds 12

console.info("one");
window.console.warn("two");
fetch("/api/data");

const result =
  typeof console === "object" && typeof window === "object"
    ? "extern-ok"
    : "bad";

result;
