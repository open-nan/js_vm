// @expect 7
// @seeds 8

function jq() {}

jq.extend = function(value) {
  this.each = function(input) {
    return input + value;
  };
};

jq.extend(3);
jq.each(4);
