// @expect 7
// @seeds 4

const provides = {};
const clientDataKey = Symbol("");
const headUpdateKey = Symbol("");

provides[clientDataKey] = { pageLayout: { value: 7 } };
provides[headUpdateKey] = function updateHead() {
  return 1;
};

const clientData = clientDataKey in provides ? provides[clientDataKey] : undefined;
clientData.pageLayout.value;
