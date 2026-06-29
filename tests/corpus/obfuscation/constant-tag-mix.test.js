// @expect ok:7
// @seeds 12

const box = {
  label: "ok",
  enabled: true,
  count: 3,
  empty: null,
};

const suffix = box.empty ?? 4;

box.enabled ? `${box.label}:${box.count + suffix}` : "bad";
