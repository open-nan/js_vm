// @expect 37
// @seeds 8

y = 1;
const assignment = y + 1;
const wrapperMath = (new Boolean(true) / true) + (new Number(6) - new String("2"));

let objectRef = {};
let sameRef = objectRef;
const identity = objectRef == sameRef && objectRef === sameRef ? 10 : 0;
const looseObject = ({ valueOf: function() { return 1; } } == true) ? 20 : 0;

assignment + wrapperMath + identity + looseObject;
