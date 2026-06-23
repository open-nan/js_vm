// @expect Ada:14
// @seeds 8

const player = {
  name: "Ada",
  score: 7,
};

const { name, score } = player;
const again = player["score"];

`${name}:${score + again}`;
