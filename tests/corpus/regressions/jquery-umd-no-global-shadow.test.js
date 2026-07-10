// @expect function:function
// @seeds 8

(function (root, factory) {
  return factory(root);
})(typeof window !== "undefined" ? window : this, function (root, noGlobal) {
  function inner(value, noGlobal) {
    var type = typeof noGlobal;
    return type + value;
  }

  inner(1, "string");

  if (void 0 === noGlobal) {
    root.jQuery = root.$ = function jQuery() {};
  }

  return typeof root.jQuery + ":" + typeof root.$;
});
