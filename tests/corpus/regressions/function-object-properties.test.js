// @expect 6
// @seeds 8

function jq() {}

jq.fn = jq.prototype = {};
jq.fn.extend = function(value) {
  return value + 2;
};

jq.prototype.extend(4);
