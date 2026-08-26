// @expect 3
// @seeds 6

function resolveSiteLocaleData({ base, locales, ...siteData }, route) {
  return {
    ...siteData,
    ...locales[route],
    head: [...(locales[route]?.head ?? []), ...siteData.head],
  };
}

const value = resolveSiteLocaleData(
  {
    base: "/",
    locales: {
      "/zh/": {
        head: [["meta", { name: "locale", content: "zh" }]],
      },
    },
    head: [["meta", { name: "description", content: "site" }]],
    theme: "default",
  },
  "/zh/",
);

value.head.length + (value.theme === "default" ? 1 : 0);
