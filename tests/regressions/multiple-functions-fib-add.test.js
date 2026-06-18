// @expect 21
// @seeds 12

function fib(n) {
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}

function add(a, b) {
  return a + b;
}

const a = fib(6);
const b = fib(7);
const value = add(a, b);
value;
