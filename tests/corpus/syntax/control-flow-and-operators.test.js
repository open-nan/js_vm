// @expect 14
// @seeds 8

let total = 0;

for (let i = 0; i < 5; i++) {
  if (i % 2 === 0) {
    total = total + i;
  }
}

let n = 0;
while (n < 3) {
  total = total + n;
  n++;
}

const pick = total > 8 ? total : 8;
pick + 5;
