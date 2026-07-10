// @expect 6
// @seeds 8

const length = hostStringPrimitive.length;
const hasMiddle = hostStringPrimitive.includes("ell") ? 1 : 0;

length + hasMiddle;
