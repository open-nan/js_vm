// @expect 1
// @seeds 8

(function (root, factory) {
  return factory(root);
})(typeof window !== "undefined" ? window : this, function (window) {
  function readDocument() {
    return window.document ? 1 : 0;
  }
  return readDocument();
});
