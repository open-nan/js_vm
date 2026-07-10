// @expect *
// @seeds 8

function outer() {
  return function run(type, callback) {
    var token;
    var index = 0;
    var tokens = type.toLowerCase().match(/[^\x20\t\r\n\f]+/g) || [];

    if (typeof callback === "function") {
      while ((token = tokens[index++])) {
        return token[0];
      }
    }
  };
}

outer()("*", function noop() {});
