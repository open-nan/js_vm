// @expect 2,4,6
// @seeds 4

const roots = [
  { rootComponents: [1, 2] },
  { rootComponents: undefined },
  { rootComponents: [3] },
];

const out = roots.flatMap((item) => {
  const children = item.rootComponents;
  if (children === undefined) return [];
  return children.map((value) => value * 2);
});

out.join(",");
